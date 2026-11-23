use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::mpsc;
use vetro_proto::{
    rpc::{ErrorCode, RpcError},
    AgentInfo, ConfigOption, ContentBlock, ItemKind, RequestResolution, SlashCommand, ThreadSource,
    ThreadStatus, ThreadUsage,
};

pub use async_trait::async_trait;
pub use wirelog::WireLog;

pub mod jsonrpc;
pub mod process;
pub mod wirelog;

pub type Result<T> = std::result::Result<T, AgentError>;
pub type EventSink = mpsc::UnboundedSender<SessionEvent>;

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
    async fn authenticate(&self, method_id: &str) -> Result<()>;
    async fn options(&self, cwd: Option<&Path>) -> Result<AgentOptions>;
    async fn list(&self, cursor: Option<String>) -> Result<SessionPage>;
    /// Read stored items without creating a live Vetro session.
    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript>;
    /// Create a native session and submit the first prompt.
    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>>;
    async fn attach(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>>;
    async fn shutdown(&self);
}

#[async_trait]
pub trait Session: Send + Sync + 'static {
    fn native_id(&self) -> &str;
    async fn send(&self, content: Vec<ContentBlock>) -> Result<()>;
    async fn interrupt(&self) -> Result<()>;
    async fn set_option(&self, option_id: &str, value: &str) -> Result<Vec<ConfigOption>>;
    async fn respond(&self, request_id: &str, resolution: RequestResolution) -> Result<()>;
    async fn detach(&self);
    /// Sessions multiplexed on the same process share its wire log.
    fn wire_log(&self) -> Arc<WireLog>;
}
