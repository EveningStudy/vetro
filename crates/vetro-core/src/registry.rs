use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{OwnedRwLockReadGuard, RwLock};

use vetro_agent_api::{Agent, AgentError, Result};
use vetro_proto::{
    rpc::{ErrorCode, Settings},
    AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthState,
};

const WARM_IDLE: Duration = Duration::from_secs(60);

#[cfg(feature = "antigravity-store")]
mod antigravity;
#[cfg(any(feature = "claude", feature = "acp"))]
mod detection;

#[derive(Default)]
pub struct Registry {
    agents: BTreeMap<String, Entry>,
    pub(crate) history_paths: BTreeMap<String, Vec<PathBuf>>,
}

struct Entry {
    agent: Arc<dyn Agent>,
    access: Arc<RwLock<()>>,
    activity: Mutex<Activity>,
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
                Entry {
                    agent,
                    access: Arc::new(RwLock::new(())),
                    activity: Mutex::new(Activity {
                        warm_since: Instant::now(),
                        last_release: None,
                        in_use: false,
                        release_requested: false,
                    }),
                },
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

    pub(crate) async fn enter(&self, id: &str, user_action: bool) -> OwnedRwLockReadGuard<()> {
        let entry = &self.agents[id];
        let guard = entry.access.clone().read_owned().await;
        let mut activity = entry.activity.lock().unwrap();
        if user_action || (!activity.in_use && activity.last_release.is_none()) {
            activity.warm_since = Instant::now();
        }
        if user_action {
            activity.last_release = None;
            activity.release_requested = false;
        }
        activity.in_use = true;
        guard
    }

    pub(crate) fn request_release(&self, id: &str) {
        self.agents[id].activity.lock().unwrap().release_requested = true;
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
        released: impl Fn(&str),
    ) {
        for (id, entry) in &self.agents {
            let Ok(_access) = entry.access.try_write() else {
                continue;
            };
            entry.agent.maintain().await;
            let authenticated = entry.agent.process_auth() == Some(AuthState::Ok);
            let release = {
                let activity = entry.activity.lock().unwrap();
                activity.in_use
                    && if authenticated {
                        idle.is_some_and(|idle| activity.warm_since.elapsed() >= idle)
                    } else {
                        activity.release_requested
                            || activity.last_release.is_some()
                            || activity.warm_since.elapsed() >= WARM_IDLE
                    }
            };
            if release
                && !has_live_thread(id)
                && tokio::time::timeout(Duration::from_secs(5), entry.agent.shutdown())
                    .await
                    .is_ok()
            {
                let mut activity = entry.activity.lock().unwrap();
                activity.in_use = false;
                activity.release_requested = false;
                activity.last_release = Some(Instant::now());
                if entry.agent.process_auth().is_some() {
                    released(id);
                }
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
        agents.push(Arc::new(vetro_agent_claude::ClaudeAgent::from_spec(
            binary.map(|path| launch_spec(path, config)),
            projects,
        )));
    }
    #[cfg(feature = "codex")]
    {
        let config = _settings.agents.get("codex");
        agents.push(Arc::new(vetro_agent_codex::CodexAgent::from_config(
            binary_override(config),
            |path| launch_spec(path, config),
        )));
    }
    #[cfg(feature = "acp")]
    {
        use vetro_agent_acp::{AcpAgent, SpawnSpec};
        for (id, name, command) in [
            ("antigravity", "Antigravity", "agy"),
            ("devin", "Devin", "devin"),
        ] {
            let config = _settings.agents.get(id);
            let binary = binary_override(config)
                .map(std::path::Path::to_owned)
                .or_else(|| detection::acp(command))
                .unwrap_or_else(|| command.into());
            let mut spec = launch_spec(binary, config);
            acp_defaults(id, &mut spec);
            let agent = AcpAgent::new(id, name, spec);
            #[cfg(feature = "antigravity-store")]
            if id == "antigravity" {
                if let Some(home) = std::env::home_dir() {
                    agents.push(Arc::new(antigravity::Antigravity::new(agent, home)));
                    continue;
                }
            }
            agents.push(Arc::new(agent));
        }
        for custom in &_settings.custom_acp_agents {
            let config = _settings.agents.get(&custom.id);
            let command = config
                .and_then(|config| config.binary_path.as_ref())
                .unwrap_or(&custom.command);
            let mut spec = SpawnSpec::new(command);
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
fn acp_defaults(id: &str, spec: &mut vetro_agent_api::process::SpawnSpec) {
    if id == "devin"
        && !spec.args.iter().any(|arg| {
            arg == "--process-model" || arg.to_string_lossy().starts_with("--process-model=")
        })
    {
        spec.args
            .splice(..0, ["--process-model".into(), "in-process".into()]);
    }
    spec.args.insert(0, "acp".into());
}

#[cfg(all(test, feature = "acp"))]
#[test]
fn devin_respects_explicit_process_model() {
    use vetro_agent_api::process::SpawnSpec;

    for args in [
        vec!["--process-model", "in-process"],
        vec!["--process-model=in-process"],
    ] {
        let mut spec = SpawnSpec::new("devin");
        spec.args.extend(args.iter().map(|arg| (*arg).into()));
        acp_defaults("devin", &mut spec);
        assert_eq!(spec.args[0], "acp");
        assert_eq!(spec.args.len(), args.len() + 1);
    }
    let mut spec = SpawnSpec::new("devin");
    acp_defaults("devin", &mut spec);
    assert_eq!(spec.args, ["acp", "--process-model", "in-process"]);
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
fn binary_override(config: Option<&vetro_proto::rpc::AgentSettings>) -> Option<&std::path::Path> {
    config?.binary_path.as_deref().map(std::path::Path::new)
}

#[cfg(any(feature = "claude", feature = "codex", feature = "acp"))]
fn launch_spec(
    binary: std::path::PathBuf,
    config: Option<&vetro_proto::rpc::AgentSettings>,
) -> vetro_agent_api::process::SpawnSpec {
    let mut spec = vetro_agent_api::process::SpawnSpec::new(binary);
    apply_config(&mut spec, config);
    spec
}

#[cfg(any(feature = "claude", feature = "codex", feature = "acp"))]
fn apply_config(
    spec: &mut vetro_agent_api::process::SpawnSpec,
    config: Option<&vetro_proto::rpc::AgentSettings>,
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
        "antigravity" => ("Antigravity", AgentKind::Acp),
        "devin" => ("Devin", AgentKind::Acp),
        _ => (id, AgentKind::Acp),
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
        },
        capabilities: AgentCapabilities::default(),
        error: None,
    }
}
