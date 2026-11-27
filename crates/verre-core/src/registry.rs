#[cfg(feature = "acp")]
use std::ffi::OsStr;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tokio::sync::{OwnedRwLockReadGuard, RwLock};

use verre_agent_api::{Agent, AgentError, Result};
use verre_proto::{
    rpc::{ErrorCode, Settings},
    AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthState,
};

const WARM_IDLE: Duration = Duration::from_secs(60);

#[cfg(feature = "antigravity-store")]
pub(crate) mod antigravity;
#[cfg(any(feature = "claude", feature = "acp"))]
mod detection;

#[derive(Default)]
pub struct Registry {
    agents: BTreeMap<String, Arc<Entry>>,
    pub(crate) history_paths: BTreeMap<String, Vec<PathBuf>>,
}

struct Entry {
    agent: Arc<dyn Agent>,
    access: Arc<RwLock<()>>,
    activity: Mutex<Activity>,
    retired: AtomicBool,
}

struct Activity {
    warm_since: Instant,
    last_release: Option<Instant>,
    in_use: bool,
    release_requested: bool,
}

impl Registry {
    pub fn new(agents: impl IntoIterator<Item = Arc<dyn Agent>>) -> Result<Self> {
        let mut registry = Self::default();
        for agent in agents {
            let id = agent.id().to_owned();
            if id.is_empty() || id.contains(':') || registry.agents.contains_key(&id) {
                return Err(crate::bad_request(format!(
                    "Invalid or duplicate agent id: {id}"
                )));
            }
            registry.agents.insert(
                id,
                Arc::new(Entry {
                    agent,
                    access: Arc::new(RwLock::new(())),
                    activity: Mutex::new(Activity {
                        warm_since: Instant::now(),
                        last_release: None,
                        in_use: false,
                        release_requested: false,
                    }),
                    retired: AtomicBool::new(false),
                }),
            );
        }
        Ok(registry)
    }

    pub fn from_settings(settings: &Settings) -> Result<Self> {
        #[allow(unused_mut)]
        let mut registry = Self::new(built_ins(settings))?;
        #[cfg(feature = "claude")]
        registry
            .history_paths
            .insert("claude".into(), vec![claude_projects(settings)]);
        #[cfg(feature = "codex")]
        if let Some(path) = agent_path(settings, "codex", "CODEX_HOME", ".codex") {
            registry.history_paths.insert("codex".into(), vec![path]);
        }
        #[cfg(feature = "antigravity-store")]
        if let Some(home) = std::env::home_dir() {
            registry.history_paths.insert(
                "antigravity".into(),
                vec![
                    home.join(".gemini/antigravity"),
                    home.join(".gemini/antigravity-cli"),
                ],
            );
        }
        Ok(registry)
    }

    pub fn watch_history(mut self, agent: &str, paths: Vec<PathBuf>) -> Self {
        self.history_paths.insert(agent.into(), paths);
        self
    }

    pub(crate) fn reconfigure(
        &self,
        previous: &Settings,
        current: &Settings,
    ) -> Result<(Self, Self)> {
        let mut next = Self::from_settings(current)?;
        let mut retired = Self::default();
        for (id, entry) in &self.agents {
            let old_custom = previous
                .custom_acp_agents
                .iter()
                .find(|agent| &agent.id == id);
            let new_custom = current
                .custom_acp_agents
                .iter()
                .find(|agent| &agent.id == id);
            let same =
                old_custom == new_custom && previous.agents.get(id) == current.agents.get(id);
            if same {
                next.agents.insert(id.clone(), entry.clone());
                if let Some(paths) = self.history_paths.get(id) {
                    next.history_paths.insert(id.clone(), paths.clone());
                }
            } else {
                retired.agents.insert(id.clone(), entry.clone());
            }
        }
        Ok((next, retired))
    }

    pub fn get(&self, id: &str) -> Result<Arc<dyn Agent>> {
        self.agents
            .get(id)
            .map(|entry| entry.agent.clone())
            .ok_or_else(|| {
                AgentError::new(
                    ErrorCode::AgentUnavailable,
                    format!("Agent unavailable: {id}"),
                )
            })
    }

    pub fn all(&self) -> impl Iterator<Item = &Arc<dyn Agent>> {
        self.agents.values().map(|entry| &entry.agent)
    }

    pub(crate) fn retire(&self) {
        for entry in self.agents.values() {
            entry.retired.store(true, Ordering::Release);
        }
    }

    pub(crate) async fn enter(
        &self,
        id: &str,
        user_action: bool,
    ) -> Result<OwnedRwLockReadGuard<()>> {
        let unavailable = || {
            AgentError::new(
                ErrorCode::AgentUnavailable,
                format!("Agent configuration changed: {id}"),
            )
        };
        let entry = self.agents.get(id).ok_or_else(unavailable)?;
        let guard = entry.access.clone().read_owned().await;
        if entry.retired.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let mut activity = entry.activity.lock().unwrap();
        if user_action || (!activity.in_use && activity.last_release.is_none()) {
            activity.warm_since = Instant::now();
        }
        if user_action {
            activity.last_release = None;
            activity.release_requested = false;
        }
        activity.in_use = true;
        Ok(guard)
    }

    pub(crate) fn request_release(&self, id: &str) {
        if let Some(entry) = self.agents.get(id) {
            entry.activity.lock().unwrap().release_requested = true;
        }
    }

    pub(crate) async fn release_requested(&self, id: &str, has_live_thread: impl FnOnce() -> bool) {
        let Some(entry) = self.agents.get(id) else {
            return;
        };
        let Ok(_access) = entry.access.try_write() else {
            return;
        };
        let requested = {
            let activity = entry.activity.lock().unwrap();
            activity.in_use && activity.release_requested
        };
        if requested && !entry.retired.load(Ordering::Acquire) && !has_live_thread() {
            entry.release().await;
        }
    }

    pub(crate) fn background_poll_due(&self, id: &str) -> bool {
        self.agents[id]
            .activity
            .lock()
            .unwrap()
            .last_release
            .is_none_or(|released| released.elapsed() >= WARM_IDLE)
    }

    pub(crate) async fn release_idle(
        &self,
        has_live_thread: impl Fn(&str) -> bool,
        idle: Option<Duration>,
    ) {
        for (id, entry) in &self.agents {
            let Ok(_access) = entry.access.try_write() else {
                continue;
            };
            if entry.retired.load(Ordering::Acquire) {
                continue;
            }
            entry.agent.maintain().await;
            let authenticated = entry.agent.process_auth() == Some(AuthState::Ok);
            let release = {
                let activity = entry.activity.lock().unwrap();
                activity.in_use
                    && (activity.release_requested
                        || if authenticated {
                            idle.is_some_and(|idle| activity.warm_since.elapsed() >= idle)
                        } else {
                            activity.last_release.is_some()
                                || activity.warm_since.elapsed() >= WARM_IDLE
                        })
            };
            if release && !has_live_thread(id) {
                entry.release().await;
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut tasks = tokio::task::JoinSet::new();
        for entry in self.agents.values() {
            let agent = entry.agent.clone();
            let access = entry.access.clone();
            tasks.spawn(async move {
                let _access = access.write_owned().await;
                agent.shutdown().await;
            });
        }
        while tasks.join_next().await.is_some() {}
    }
}

impl Entry {
    async fn release(&self) {
        if tokio::time::timeout(Duration::from_secs(5), self.agent.shutdown())
            .await
            .is_ok()
        {
            let mut activity = self.activity.lock().unwrap();
            activity.in_use = false;
            activity.release_requested = false;
            activity.last_release = Some(Instant::now());
        }
    }
}

#[cfg(feature = "acp")]
pub(crate) struct AcpBuiltin {
    pub id: &'static str,
    pub name: &'static str,
    pub commands: &'static [&'static str],
    pub default_args: &'static [&'static str],
    pub default_env: &'static [(&'static str, &'static str)],
}

#[cfg(feature = "acp")]
pub(crate) const ACP_BUILTINS: &[AcpBuiltin] = &[
    AcpBuiltin {
        id: "antigravity",
        name: "Antigravity",
        commands: &["agy"],
        default_args: &["acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "devin",
        name: "Devin",
        commands: &["devin"],
        default_args: &["acp", "--process-model", "in-process"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "kiro",
        name: "Kiro",
        commands: &["kiro-cli"],
        default_args: &["acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "gemini",
        name: "Gemini CLI",
        commands: &["gemini"],
        default_args: &["--acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "opencode",
        name: "OpenCode",
        commands: &["opencode"],
        default_args: &["acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "cursor",
        name: "Cursor",
        commands: &["cursor-agent"],
        default_args: &["acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "copilot",
        name: "GitHub Copilot",
        commands: &["copilot"],
        default_args: &["--acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "goose",
        name: "Goose",
        commands: &["goose"],
        default_args: &["acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "cline",
        name: "Cline",
        commands: &["cline"],
        default_args: &["--acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "kimi",
        name: "Kimi Code",
        commands: &["kimi-code", "kimi"],
        default_args: &["acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "qwen-code",
        name: "Qwen Code",
        commands: &["qwen", "qwen-code"],
        default_args: &["--acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "droid",
        name: "Factory Droid",
        commands: &["droid"],
        default_args: &["exec", "--output-format", "acp-daemon"],
        default_env: &[
            ("DROID_DISABLE_AUTO_UPDATE", "true"),
            ("FACTORY_DROID_AUTO_UPDATE_ENABLED", "false"),
        ],
    },
    AcpBuiltin {
        id: "amp",
        name: "Amp",
        commands: &["amp-acp"],
        default_args: &[],
        default_env: &[],
    },
    AcpBuiltin {
        id: "auggie",
        name: "Auggie",
        commands: &["auggie"],
        default_args: &["--acp"],
        default_env: &[("AUGMENT_DISABLE_AUTO_UPDATE", "1")],
    },
    AcpBuiltin {
        id: "vibe",
        name: "Mistral Vibe",
        commands: &["vibe-acp"],
        default_args: &[],
        default_env: &[],
    },
    AcpBuiltin {
        id: "kilo",
        name: "Kilo",
        commands: &["kilo"],
        default_args: &["acp"],
        default_env: &[],
    },
    AcpBuiltin {
        id: "grok",
        name: "Grok",
        commands: &["grok"],
        default_args: &["agent", "stdio"],
        default_env: &[],
    },
];

fn built_ins(_settings: &Settings) -> Vec<Arc<dyn Agent>> {
    #[allow(unused_mut)]
    let mut agents: Vec<Arc<dyn Agent>> = Vec::new();
    #[cfg(feature = "claude")]
    {
        let config = _settings.agents.get("claude");
        let binary = match binary_override(config) {
            Some(path) => Some(path.to_owned()),
            None => detection::claude(),
        };
        let projects = claude_projects(_settings);
        agents.push(Arc::new(verre_agent_claude::ClaudeAgent::from_spec(
            binary.map(|path| launch_spec(path, config)),
            projects,
        )));
    }
    #[cfg(feature = "codex")]
    {
        let config = _settings.agents.get("codex");
        agents.push(Arc::new(verre_agent_codex::CodexAgent::from_config(
            binary_override(config),
            |path| launch_spec(path, config),
        )));
    }
    #[cfg(feature = "acp")]
    {
        use verre_agent_acp::{AcpAgent, SpawnSpec};
        for builtin in ACP_BUILTINS {
            let config = _settings.agents.get(builtin.id);
            let detected = binary_override(config)
                .map(resolve_binary)
                .or_else(|| builtin.commands.iter().find_map(|cmd| detection::acp(cmd)));
            let legacy_custom = _settings
                .custom_acp_agents
                .iter()
                .find(|custom| custom.id == builtin.id);
            let spec = if let (None, Some(custom)) = (&detected, legacy_custom) {
                let mut custom_spec = SpawnSpec::new(resolve_command(&custom.command));
                custom_spec.args.extend(custom.args.iter().map(Into::into));
                custom_spec.env.extend(
                    custom
                        .env
                        .iter()
                        .map(|(key, value)| (key.into(), value.into())),
                );
                apply_config(&mut custom_spec, config);
                custom_spec
            } else {
                let binary = detected.unwrap_or_else(|| PathBuf::from(builtin.commands[0]));
                let mut builtin_spec = launch_spec(binary, config);
                acp_defaults(builtin, &mut builtin_spec);
                builtin_spec
            };
            let agent = AcpAgent::new(builtin.id, builtin.name, spec);
            #[cfg(feature = "antigravity-store")]
            if builtin.id == "antigravity" {
                if let Some(home) = std::env::home_dir() {
                    agents.push(Arc::new(antigravity::Antigravity::new(agent, home)));
                    continue;
                }
            }
            agents.push(Arc::new(agent));
        }
        let built_in_count = agents.len();
        for custom in &_settings.custom_acp_agents {
            if agents[..built_in_count]
                .iter()
                .any(|agent| agent.id() == custom.id)
            {
                continue;
            }
            let config = _settings.agents.get(&custom.id);
            let command = config
                .and_then(|config| config.binary_path.as_ref())
                .unwrap_or(&custom.command);
            let mut spec = SpawnSpec::new(resolve_command(command));
            spec.args.extend(custom.args.iter().map(Into::into));
            spec.env.extend(
                custom
                    .env
                    .iter()
                    .map(|(key, value)| (key.into(), value.into())),
            );
            apply_config(&mut spec, config);
            agents.push(Arc::new(AcpAgent::new(&custom.id, &custom.name, spec)));
        }
    }
    agents
}

#[cfg(feature = "acp")]
fn acp_defaults(builtin: &AcpBuiltin, spec: &mut verre_agent_api::process::SpawnSpec) {
    if builtin.id == "devin" {
        if !spec.args.iter().any(|arg| {
            arg == "--process-model" || arg.to_string_lossy().starts_with("--process-model=")
        }) {
            spec.args
                .splice(..0, ["--process-model".into(), "in-process".into()]);
        }
        if let Some(index) = spec.args.iter().position(|arg| arg == "acp") {
            let acp = spec.args.remove(index);
            spec.args.insert(0, acp);
        } else {
            spec.args.insert(0, "acp".into());
        }
    } else if !builtin.default_args.is_empty() {
        let defaults: Vec<std::ffi::OsString> =
            builtin.default_args.iter().map(Into::into).collect();
        if builtin.default_args == ["acp"] {
            if let Some(index) = spec.args.iter().position(|arg| arg == "acp") {
                let acp = spec.args.remove(index);
                spec.args.insert(0, acp);
            } else {
                spec.args.insert(0, "acp".into());
            }
        } else if !spec.args.starts_with(&defaults)
            && !defaults.iter().all(|flag| spec.args.contains(flag))
        {
            spec.args.splice(..0, defaults);
        }
    }
    for (key, value) in builtin.default_env {
        spec.env
            .entry(OsStr::new(key).to_os_string())
            .or_insert_with(|| OsStr::new(value).to_os_string());
    }
}

#[cfg(all(test, feature = "acp"))]
fn find_builtin(id: &str) -> &'static AcpBuiltin {
    ACP_BUILTINS.iter().find(|entry| entry.id == id).unwrap()
}

#[cfg(all(test, feature = "acp"))]
#[test]
fn devin_respects_explicit_process_model() {
    use verre_agent_api::process::SpawnSpec;

    let devin = find_builtin("devin");
    for args in [
        vec!["--process-model", "in-process"],
        vec!["--process-model=in-process"],
    ] {
        let mut spec = SpawnSpec::new("devin");
        spec.args.extend(args.iter().map(|arg| (*arg).into()));
        acp_defaults(devin, &mut spec);
        assert_eq!(spec.args[0], "acp");
        assert_eq!(spec.args.len(), args.len() + 1);
    }
    let mut spec = SpawnSpec::new("devin");
    acp_defaults(devin, &mut spec);
    assert_eq!(spec.args, ["acp", "--process-model", "in-process"]);
}

#[cfg(all(test, feature = "acp"))]
#[test]
fn builtin_acp_presets_construct_idempotent_args_and_unique_ids() {
    use std::collections::HashSet;
    use verre_agent_api::process::SpawnSpec;

    let mut ids = HashSet::new();
    for builtin in ACP_BUILTINS {
        assert!(
            ids.insert(builtin.id),
            "duplicate builtin id {}",
            builtin.id
        );
        let mut spec = SpawnSpec::new(builtin.commands[0]);
        acp_defaults(builtin, &mut spec);
        let first = spec.args.clone();
        acp_defaults(builtin, &mut spec);
        assert_eq!(
            spec.args, first,
            "acp_defaults not idempotent for {}",
            builtin.id
        );
        let expected: Vec<std::ffi::OsString> =
            builtin.default_args.iter().map(Into::into).collect();
        assert_eq!(spec.args, expected, "unexpected args for {}", builtin.id);
    }

    let mut gemini = SpawnSpec::new("gemini");
    gemini.args = vec!["--model".into(), "pro".into()];
    acp_defaults(find_builtin("gemini"), &mut gemini);
    assert_eq!(gemini.args, ["--acp", "--model", "pro"]);

    let mut amp = SpawnSpec::new("amp-acp");
    acp_defaults(find_builtin("amp"), &mut amp);
    assert!(amp.args.is_empty());

    let mut droid = SpawnSpec::new("droid");
    droid
        .env
        .insert("DROID_DISABLE_AUTO_UPDATE".into(), "false".into());
    acp_defaults(find_builtin("droid"), &mut droid);
    assert_eq!(droid.args, ["exec", "--output-format", "acp-daemon"]);
    assert_eq!(
        droid
            .env
            .get(std::ffi::OsStr::new("DROID_DISABLE_AUTO_UPDATE"))
            .unwrap(),
        "false"
    );
    assert_eq!(
        droid
            .env
            .get(std::ffi::OsStr::new("FACTORY_DROID_AUTO_UPDATE_ENABLED"))
            .unwrap(),
        "false"
    );
}

#[cfg(all(test, feature = "acp"))]
#[tokio::test]
async fn kiro_is_registered_once_and_uses_builtin_overrides() {
    use verre_agent_api::process::SpawnSpec;
    use verre_proto::rpc::{AgentSettings, CustomAcpAgent};

    let mut spec = SpawnSpec::new("kiro-cli");
    let kiro = find_builtin("kiro");
    acp_defaults(kiro, &mut spec);
    acp_defaults(kiro, &mut spec);
    assert_eq!(spec.args, ["acp"]);

    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-kiro");
    let mut settings = Settings::default();
    settings.custom_acp_agents.push(CustomAcpAgent {
        id: "kiro".into(),
        name: "Legacy Kiro".into(),
        command: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
    });
    settings.agents.insert(
        "kiro".into(),
        AgentSettings {
            binary_path: Some(missing.to_string_lossy().into_owned()),
            ..Default::default()
        },
    );
    let registry = Registry::from_settings(&settings).unwrap();
    assert_eq!(
        registry.all().filter(|agent| agent.id() == "kiro").count(),
        1
    );
    let info = registry.get("kiro").unwrap().info().await;
    assert_eq!(info.name, "Kiro");
    assert!(!info.installed);

    let custom_gemini_path = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut fallback_settings = Settings::default();
    fallback_settings.agents.insert(
        "gemini".into(),
        AgentSettings {
            binary_path: None,
            ..Default::default()
        },
    );
    fallback_settings.custom_acp_agents.push(CustomAcpAgent {
        id: "gemini".into(),
        name: "Custom Gemini".into(),
        command: custom_gemini_path,
        args: vec!["--experimental-acp".into()],
        env: BTreeMap::new(),
    });
    let fallback_registry = Registry::from_settings(&fallback_settings).unwrap();
    let gemini_info = fallback_registry.get("gemini").unwrap().info().await;
    if detection::acp("gemini").is_none() {
        assert!(gemini_info.installed);
    }
}

#[cfg(feature = "claude")]
fn claude_projects(settings: &Settings) -> PathBuf {
    agent_path(settings, "claude", "CLAUDE_CONFIG_DIR", ".claude")
        .unwrap_or_default()
        .join("projects")
}

#[cfg(any(feature = "claude", feature = "codex"))]
fn agent_path(settings: &Settings, id: &str, variable: &str, default: &str) -> Option<PathBuf> {
    settings
        .agents
        .get(id)
        .and_then(|config| config.env.get(variable))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(variable).map(PathBuf::from))
        .or_else(|| std::env::home_dir().map(|home| home.join(default)))
}

#[cfg(any(feature = "claude", feature = "codex", feature = "acp"))]
fn binary_override(config: Option<&verre_proto::rpc::AgentSettings>) -> Option<&std::path::Path> {
    config?.binary_path.as_deref().map(std::path::Path::new)
}

#[cfg(feature = "acp")]
fn resolve_command(command: &str) -> PathBuf {
    resolve_binary(std::path::Path::new(command))
}

#[cfg(feature = "acp")]
fn resolve_binary(path: &std::path::Path) -> PathBuf {
    if path.components().count() == 1 {
        detection::acp(&path.to_string_lossy()).unwrap_or_else(|| path.to_owned())
    } else {
        path.to_owned()
    }
}

#[cfg(any(feature = "claude", feature = "codex", feature = "acp"))]
fn launch_spec(
    binary: std::path::PathBuf,
    config: Option<&verre_proto::rpc::AgentSettings>,
) -> verre_agent_api::process::SpawnSpec {
    let mut spec = verre_agent_api::process::SpawnSpec::new(binary);
    apply_config(&mut spec, config);
    spec
}

#[cfg(any(feature = "claude", feature = "codex", feature = "acp"))]
fn apply_config(
    spec: &mut verre_agent_api::process::SpawnSpec,
    config: Option<&verre_proto::rpc::AgentSettings>,
) {
    if let Some(config) = config {
        spec.args.extend(config.extra_args.iter().map(Into::into));
        spec.env.extend(
            config
                .env
                .iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
    }
}

pub(crate) fn disabled_info(id: &str) -> AgentInfo {
    let (name, kind) = match id {
        "claude" => ("Claude Code", AgentKind::Claude),
        "codex" => ("Codex", AgentKind::Codex),
        _ => {
            #[cfg(feature = "acp")]
            let name = ACP_BUILTINS
                .iter()
                .find(|entry| entry.id == id)
                .map_or(id, |entry| entry.name);
            #[cfg(not(feature = "acp"))]
            let name = id;
            (name, AgentKind::Acp)
        }
    };
    AgentInfo {
        id: id.into(),
        name: name.into(),
        kind,
        installed: false,
        path: None,
        version: None,
        auth: AgentAuth {
            state: AuthState::Unknown,
            methods: Vec::new(),
            account: None,
            provider: None,
            endpoint: None,
        },
        capabilities: AgentCapabilities::default(),
        error: None,
        rate_limits: Vec::new(),
        rate_limits_updated_at: None,
    }
}
