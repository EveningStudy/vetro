use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::mpsc;
use vetro_proto::{
    rpc::{AuthenticateResult, ErrorCode, RpcError},
    AgentInfo, ConfigOption, ContentBlock, ItemKind, RequestResolution, SlashCommand, ThreadSource,
    ThreadStatus, ThreadUsage,
};

pub use async_trait::async_trait;
pub use wirelog::WireLog;

pub mod detection;
pub mod jsonrpc;
pub mod process;
pub mod wirelog;

pub type Result<T> = std::result::Result<T, AgentError>;
pub type EventSink = mpsc::UnboundedSender<SessionEvent>;

pub fn gateway_origin(value: &str, official_hosts: &[&str]) -> Option<String> {
    let url = url::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || official_hosts.contains(&url.host_str()?.trim_end_matches('.'))
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

fn codex_subagent(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    source.contains("subagent") || source.contains("sub_agent")
}

pub fn codex_internal_source(source: &serde_json::Value) -> bool {
    source.as_str().map_or_else(
        || {
            source.as_object().is_some_and(|fields| {
                fields.iter().any(|(key, value)| {
                    codex_subagent(key) || value.as_str().is_some_and(codex_subagent)
                })
            })
        },
        codex_subagent,
    )
}

pub fn session_is_internal(
    agent_id: &str,
    source: ThreadSource,
    origin: Option<&str>,
    title: &str,
    preview: Option<&str>,
) -> bool {
    source == ThreadSource::Exec
        || (agent_id == "codex"
            && (origin.is_some_and(codex_subagent)
                || (origin == Some("history projection")
                    && title == "Codex thread"
                    && preview.unwrap_or_default().trim().is_empty())))
        || [Some(title), preview]
            .into_iter()
            .flatten()
            .any(|text| text.trim_start().starts_with("<caller_constraints>"))
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AgentError {
    pub code: ErrorCode,
    pub message: String,
}

impl AgentError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<AgentError> for RpcError {
    fn from(error: AgentError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// Upsert by id. The core assigns ordinals.
    Item(ItemDraft),
    TextDelta {
        item_id: String,
        delta: String,
    },
    Status(ThreadStatus),
    Title(String),
    Model(Option<String>),
    Options(Vec<ConfigOption>),
    Commands(Vec<SlashCommand>),
    Usage(ThreadUsage),
    AuthRequired,
    Closed {
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ItemDraft {
    pub id: String,
    pub ts: Option<i64>,
    pub turn_id: Option<String>,
    pub kind: ItemKind,
}

#[derive(Debug, Clone, Default)]
pub struct AgentOptions {
    pub options: Vec<ConfigOption>,
    pub commands: Vec<SlashCommand>,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub native_id: String,
    pub title: String,
    pub preview: Option<String>,
    pub cwd: Option<PathBuf>,
    pub created_at: Option<i64>,
    pub updated_at: i64,
    pub source: ThreadSource,
    pub origin: Option<String>,
    pub model: Option<String>,
    pub resumable: bool,
    pub parent_native_id: Option<String>,
    pub internal: bool,
}

impl SessionRecord {
    pub fn classify(mut self, agent_id: &str) -> Self {
        self.internal |= session_is_internal(
            agent_id,
            self.source,
            self.origin.as_deref(),
            &self.title,
            self.preview.as_deref(),
        );
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionPage {
    pub sessions: Vec<SessionRecord>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    /// Oldest first. IDs should match live item IDs when the native protocol allows it.
    pub items: Vec<ItemDraft>,
}

#[derive(Debug, Clone)]
pub struct StartRequest {
    pub cwd: PathBuf,
    pub content: Vec<ContentBlock>,
    /// Only choices the user changed from the agent's defaults.
    pub options: BTreeMap<String, String>,
}

#[async_trait]
pub trait Agent: Send + Sync + 'static {
    fn id(&self) -> &str;
    async fn info(&self) -> AgentInfo;
    async fn authenticate(&self, method_id: &str) -> Result<AuthenticateResult>;
    fn process_auth(&self) -> Option<vetro_proto::AuthState> {
        None
    }
    async fn recover_auth(&self) -> Result<bool> {
        Ok(false)
    }
    async fn maintain(&self) {}
    fn options_need_session(&self) -> bool {
        false
    }
    async fn options(
        &self,
        cwd: Option<&Path>,
        selected: &BTreeMap<String, String>,
    ) -> Result<AgentOptions>;
    async fn prepare_options(
        &self,
        cwd: Option<&Path>,
        selected: &BTreeMap<String, String>,
    ) -> Result<AgentOptions> {
        self.options(cwd, selected).await
    }
    async fn list(&self, cursor: Option<String>) -> Result<SessionPage>;
    async fn list_without_runtime(&self) -> Result<Option<SessionPage>> {
        Ok(None)
    }
    /// Read stored items without creating a live Vetro session.
    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript>;
    async fn read_without_runtime(&self, _native_id: &str) -> Result<Option<Transcript>> {
        Ok(None)
    }
    /// Create a native session and submit the first prompt.
    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>>;
    async fn attach(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>>;
    fn launch_only_option(&self, _id: &str) -> bool {
        false
    }
    async fn attach_with_options(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        _options: BTreeMap<String, String>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        self.attach(native_id, cwd, sink).await
    }
    async fn fork(
        &self,
        _native_id: &str,
        _cwd: Option<&Path>,
        _turn_id: Option<&str>,
        _sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        Err(AgentError::new(
            ErrorCode::NotResumable,
            "This agent does not support native session forking",
        ))
    }
    async fn shutdown(&self);
}

#[async_trait]
pub trait Session: Send + Sync + 'static {
    fn native_id(&self) -> &str;
    fn initial_options(&self) -> Option<AgentOptions> {
        None
    }
    async fn send(&self, content: Vec<ContentBlock>) -> Result<()>;
    async fn interrupt(&self) -> Result<()>;
    async fn set_option(&self, option_id: &str, value: &str) -> Result<Vec<ConfigOption>>;
    async fn respond(&self, request_id: &str, resolution: RequestResolution) -> Result<()>;
    async fn detach(&self);
    fn wire_log(&self) -> Arc<WireLog>;
}
