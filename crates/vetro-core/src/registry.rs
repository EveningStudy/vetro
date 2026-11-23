use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{OwnedRwLockReadGuard, RwLock};

use vetro_agent_api::{Agent, AgentError, Result};
use vetro_proto::{
    rpc::{ErrorCode, Settings},
    AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthState,
};

#[cfg(feature = "antigravity-store")]
mod antigravity;
#[cfg(any(feature = "claude", feature = "acp"))]
mod detection;

#[derive(Default)]
pub struct Registry {
    agents: BTreeMap<String, Entry>,
}

struct Entry {
    agent: Arc<dyn Agent>,
    access: Arc<RwLock<()>>,
    activity: Mutex<Activity>,
}

struct Activity {
    last_user_action: Instant,
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
                        last_user_action: Instant::now(),
                        in_use: false,
                        release_requested: false,
                    }),
                },
            );
        }
        Ok(registry)
    }

    pub fn from_settings(settings: &Settings) -> Result<Self> {
        Self::new(built_ins(settings))
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
        if user_action {
            activity.last_user_action = Instant::now();
            activity.release_requested = false;
        }
        activity.in_use = true;
        guard
    }

    pub(crate) fn request_release(&self, id: &str) {
        self.agents[id].activity.lock().unwrap().release_requested = true;
    }

    pub(crate) async fn release_idle(
        &self,
        idle: Duration,
        has_live_thread: impl Fn(&str) -> bool,
    ) {
        for (id, entry) in &self.agents {
            let Ok(_access) = entry.access.try_write() else {
                continue;
            };
            let release = {
                let activity = entry.activity.lock().unwrap();
                activity.in_use
                    && (activity.release_requested || activity.last_user_action.elapsed() >= idle)
            };
            if release
                && !has_live_thread(id)
                && tokio::time::timeout(Duration::from_secs(5), entry.agent.shutdown())
                    .await
                    .is_ok()
            {
                entry.activity.lock().unwrap().in_use = false;
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
        let projects = config
            .and_then(|config| config.env.get("CLAUDE_CONFIG_DIR"))
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from))
            .or_else(|| std::env::home_dir().map(|home| home.join(".claude")))
            .unwrap_or_default()
            .join("projects");
        agents.push(Arc::new(vetro_agent_claude::ClaudeAgent::from_spec(
            binary.map(|path| launch_spec(path, config)),
            projects,
        )));
    }
    #[cfg(feature = "codex")]
    {
        let config = _settings.agents.get("codex");
        let spec = vetro_agent_codex::resolve_binary(binary_override(config))
            .map(|path| launch_spec(path, config));
        agents.push(Arc::new(vetro_agent_codex::CodexAgent::from_spec(spec)));
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
            let defaults: &[&str] = if id == "devin" {
                &["acp", "--process-model", "in-process"]
            } else {
                &["acp"]
            };
            spec.args
                .splice(..0, defaults.iter().map(|arg| (*arg).into()));
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
