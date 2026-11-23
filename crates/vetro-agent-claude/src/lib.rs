use std::{
    collections::BTreeMap,
    env, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

use serde_json::Value;
use vetro_agent_api::{
    async_trait, process::SpawnSpec, Agent, AgentError, AgentOptions, EventSink, Result, Session,
    SessionPage, StartRequest, Transcript,
};
use vetro_proto::{rpc::ErrorCode, AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthState};

mod events;
mod history;
mod session;

use session::ClaudeSession;

pub struct ClaudeAgent {
    spec: Option<SpawnSpec>,
    projects: PathBuf,
    sessions: Mutex<Vec<Weak<ClaudeSession>>>,
}

impl Default for ClaudeAgent {
    fn default() -> Self {
        let home =
            env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        let name = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let executable = env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
            .chain(home.iter().map(|home| home.join(".local/bin")))
            .map(|dir| dir.join(name))
            .find(|path| path.is_absolute() && path.is_file());
        let config = env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join(".claude")));
        Self {
            spec: executable.map(SpawnSpec::new),
            projects: config.map(|dir| dir.join("projects")).unwrap_or_default(),
            sessions: Mutex::default(),
        }
    }
}

impl ClaudeAgent {
    pub fn new(executable: impl Into<PathBuf>, projects: impl Into<PathBuf>) -> Self {
        Self::from_spec(Some(SpawnSpec::new(executable)), projects)
    }

    pub fn from_spec(spec: Option<SpawnSpec>, projects: impl Into<PathBuf>) -> Self {
        Self {
            spec,
            projects: projects.into(),
            sessions: Mutex::default(),
        }
    }

    fn spawn_spec(&self) -> Result<&SpawnSpec> {
        self.spec.as_ref().ok_or_else(|| {
            AgentError::new(
                ErrorCode::AgentUnavailable,
                "Claude Code executable not found",
            )
        })
    }

    async fn launch(
        &self,
        id: String,
        cwd: Option<PathBuf>,
        resume: bool,
        choices: BTreeMap<String, String>,
        sink: EventSink,
    ) -> Result<Arc<ClaudeSession>> {
        let session =
            ClaudeSession::launch(self.spawn_spec()?.clone(), id, cwd, resume, choices, sink)?;
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.retain(|session| session.strong_count() > 0);
            sessions.push(Arc::downgrade(&session));
        }
        if let Err(error) = session.initialize().await {
            session.detach().await;
            return Err(error);
        }
        Ok(session)
    }

    async fn probe(&self, cwd: Option<&Path>) -> Result<(Value, AgentOptions)> {
        let (sink, _events) = tokio::sync::mpsc::unbounded_channel();
        let session = self
            .launch(
                uuid()?,
                cwd.map(Path::to_path_buf),
                false,
                BTreeMap::new(),
                sink,
            )
            .await?;
        let metadata = session.metadata();
        session.detach().await;
        Ok(metadata)
    }
}

#[async_trait]
impl Agent for ClaudeAgent {
    fn id(&self) -> &str {
        "claude"
    }

    async fn info(&self) -> AgentInfo {
        let probe = self.probe(None).await.map(|(initialize, _)| initialize);
        let reply = probe.as_ref().ok();
        let account = reply.and_then(|reply| reply.get("account"));
        let account_label = account.and_then(|account| {
            string(account, "subscriptionType").or_else(|| {
                string(account, "apiProvider").filter(|provider| *provider != "firstParty")
            })
        });
        let error = probe.as_ref().err();
        AgentInfo {
            id: self.id().into(),
            name: "Claude Code".into(),
            kind: AgentKind::Claude,
            installed: self
                .spec
                .as_ref()
                .is_some_and(|spec| spec.program.is_file()),
            path: self
                .spec
                .as_ref()
                .map(|spec| spec.program.display().to_string()),
            version: reply_version(reply),
            auth: AgentAuth {
                state: if error
                    .as_ref()
                    .is_some_and(|error| error.code == ErrorCode::AuthRequired)
                {
                    AuthState::Required
                } else if account_label.is_some() {
                    AuthState::Ok
                } else {
                    AuthState::Unknown
                },
                methods: Vec::new(),
                account: account_label.map(str::to_owned),
            },
            capabilities: AgentCapabilities {
                image_input: true,
                mid_turn_input: true,
                resume: true,
                list_sessions: true,
            },
            error: error.map(|error| error.message.clone()),
        }
    }

    async fn authenticate(&self, _method_id: &str) -> Result<()> {
        Err(AgentError::new(
            ErrorCode::AuthRequired,
            "Sign in using Claude Code's native terminal: claude auth login",
        ))
    }

    async fn options(&self, cwd: Option<&Path>) -> Result<AgentOptions> {
        self.probe(cwd).await.map(|(_, options)| options)
    }

    async fn list(&self, _cursor: Option<String>) -> Result<SessionPage> {
        let root = self.projects.clone();
        Ok(SessionPage {
            sessions: disk(move || history::list(&root)).await?,
            next_cursor: None,
        })
    }

    async fn read(&self, native_id: &str, _cwd: Option<&Path>) -> Result<Transcript> {
        let root = self.projects.clone();
        let id = native_id.to_owned();
        disk(move || history::read(&root, &id)).await
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        let session = self
            .launch(uuid()?, Some(req.cwd), false, req.options, sink)
            .await?;
        if let Err(error) = session.send(req.content).await {
            session.detach().await;
            return Err(error);
        }
        Ok(session)
    }

    async fn attach(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        history::validate_id(native_id)?;
        let root = self.projects.clone();
        let id = native_id.to_owned();
        let stored = disk(move || history::original_cwd(&root, &id)).await?;
        let cwd = stored
            .or_else(|| cwd.map(Path::to_path_buf))
            .ok_or_else(|| {
                AgentError::new(
                    ErrorCode::NotResumable,
                    "Claude session has no known working directory",
                )
            })?;
        Ok(self
            .launch(native_id.to_owned(), Some(cwd), true, BTreeMap::new(), sink)
            .await?)
    }

    async fn shutdown(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
        for session in sessions.into_iter().filter_map(|session| session.upgrade()) {
            session.detach().await;
        }
    }
}

fn reply_version(reply: Option<&Value>) -> Option<String> {
    reply
        .and_then(|reply| string(reply, "claude_code_version"))
        .map(str::to_owned)
}

async fn disk<T: Send + 'static>(read: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(read)
        .await
        .map_err(|error| AgentError::new(ErrorCode::Internal, error.to_string()))?
}

fn string<'value>(value: &'value Value, key: &str) -> Option<&'value str> {
    value.get(key)?.as_str()
}

fn bad_request(message: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::BadRequest, message)
}

fn io_error(error: io::Error) -> AgentError {
    AgentError::new(ErrorCode::Internal, error.to_string())
}

fn uuid() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| AgentError::new(ErrorCode::Internal, error.to_string()))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    use std::fmt::Write;
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        write!(id, "{byte:02x}").unwrap();
    }
    Ok(id)
}
