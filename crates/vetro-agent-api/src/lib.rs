use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::mpsc;
use vetro_proto::{
    rpc::{AuthenticateResult, ErrorCode, RewindFilesResult, RpcError},
    AgentInfo, ConfigCategory, ConfigOption, ContentBlock, HistoryEditCapabilities, ItemKind,
    RequestResolution, SlashCommand, ThreadSource, ThreadStatus, ThreadUsage,
};

pub use async_trait::async_trait;
pub use wirelog::WireLog;

pub mod detection;
pub mod drift;
pub mod jsonrpc;
pub mod process;
pub mod wirelog;

pub type Result<T> = std::result::Result<T, AgentError>;
pub type EventSink = mpsc::UnboundedSender<SessionEvent>;

pub fn normalize_option_danger(options: &mut [ConfigOption]) {
    for option in options {
        let id = option.id.to_lowercase();
        let name = option.name.to_lowercase();
        let matches_option = |term| id.contains(term) || name.contains(term);
        let mode = id
            .split("model")
            .chain(name.split("model"))
            .any(|part| part.contains("mode"));
        if option.category != ConfigCategory::Mode
            && !mode
            && ![
                "perm",
                "approval",
                "sandbox",
                "autopilot",
                "trust",
                "safety",
                "policy",
            ]
            .into_iter()
            .any(matches_option)
        {
            continue;
        }
        let autopilot = matches_option("autopilot");
        let approval = matches_option("approval");
        for choice in &mut option.choices {
            choice.danger |= (approval && choice.value.trim().eq_ignore_ascii_case("never"))
                || [&choice.value, &choice.name].into_iter().any(|text| {
                    let text = text.trim().to_lowercase();
                    (autopilot && matches!(text.as_str(), "on" | "true" | "enabled" | "always"))
                        || [
                            "bypass",
                            "yolo",
                            "danger",
                            "full-access",
                            "full_access",
                            "full access",
                            "unrestricted",
                            "skip-permissions",
                            "skip_permissions",
                            "dontask",
                            "dont-ask",
                            "autonomous",
                            "trust-all",
                            "trust_all",
                            "allow-all",
                            "allow_all",
                        ]
                        .into_iter()
                        .any(|term| text.contains(term))
                });
        }
    }
}

#[cfg(test)]
#[test]
fn normalizes_builtin_and_custom_agent_choices() {
    use serde_json::json;

    for (id, category, value, label, danger) in [
        (
            "mode",
            "mode",
            "bypassPermissions",
            "Bypass permissions",
            true,
        ),
        (
            "permissions",
            "other",
            ":danger-full-access",
            "Full access",
            true,
        ),
        ("approvalPolicy", "other", "never", "Never", true),
        ("mode", "mode", "bypass", "Bypass", true),
        ("mode", "mode", "autonomous", "Autonomous", true),
        ("autopilot", "other", "on", "On", true),
        ("autopilot", "other", "choice-id", "Enabled", true),
        ("mode", "mode", "auto", "Auto", false),
        ("toolSafety", "other", "YOLO", "Advanced", true),
        ("model", "model", "yolo", "YOLO", false),
    ] {
        let mut option: ConfigOption = serde_json::from_value(json!({
            "id": id, "name": id, "category": category, "currentValue": value,
            "choices": [{"value": value, "name": label}]
        }))
        .unwrap();
        normalize_option_danger(std::slice::from_mut(&mut option));
        assert_eq!(option.choices[0].danger, danger, "{id}={value}");
    }
    let mut option: ConfigOption = serde_json::from_value(json!({
        "id": "mode", "name": "Mode", "category": "mode", "currentValue": "auto",
        "choices": [
            {"value": "vendor-unsafe", "name": "Advanced", "danger": true},
            {"value": "default", "name": "Default"},
            {"value": "vibe", "name": "Vibe"},
            {"value": "plan", "name": "Plan"},
            {"value": "spec", "name": "Spec"},
            {"value": "ask", "name": "Ask"},
            {"value": "smart", "name": "Smart"},
            {"value": "accept-edits", "name": "Accept edits"},
            {"value": "acceptEdits", "name": "Accept edits"},
            {"value": "read-only", "name": "Read only"},
            {"value": "workspace-write", "name": "Workspace write"},
            {"value": "off", "name": "Off"},
            {"value": "false", "name": "False"}
        ]
    }))
    .unwrap();
    normalize_option_danger(std::slice::from_mut(&mut option));
    assert!(option.choices[0].danger);
    assert!(option.choices[1..].iter().all(|choice| !choice.danger));
}

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

pub fn codex_web_source(source: &serde_json::Value) -> bool {
    matches!(
        source.as_str().or_else(|| source["custom"].as_str()),
        Some("chatgpt" | "cloud")
    )
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
    if source == ThreadSource::Vetro {
        return false;
    }
    source == ThreadSource::Exec
        || (agent_id == "codex"
            && (origin.is_some_and(codex_subagent)
                || (origin == Some("history projection")
                    && title == "Codex thread"
                    && preview.unwrap_or_default().trim().is_empty())))
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

impl ItemDraft {
    pub fn turn_end(
        turn_id: &str,
        stop_reason: vetro_proto::StopReason,
        ts: Option<i64>,
        duration_ms: Option<u64>,
        usage: Option<vetro_proto::TokenUsage>,
    ) -> Self {
        Self {
            id: format!("{turn_id}:end"),
            ts,
            turn_id: Some(turn_id.to_owned()),
            kind: ItemKind::TurnEnd {
                stop_reason,
                duration_ms,
                usage,
            },
        }
    }
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
    pub incomplete: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    /// Oldest first. IDs should match live item IDs when the native protocol allows it.
    pub items: Vec<ItemDraft>,
    pub usage: ThreadUsage,
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
    fn resume_option_at_launch(&self, _id: &str) -> bool {
        false
    }
    fn retry_option_on_resume(&self, _id: &str, _error: &AgentError) -> bool {
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
    fn history_edit_capabilities(&self) -> HistoryEditCapabilities {
        HistoryEditCapabilities::default()
    }
    async fn rollback_conversation(
        &self,
        _native_id: &str,
        _cwd: Option<&Path>,
        _before_turn_id: &str,
    ) -> Result<()> {
        Err(AgentError::new(
            ErrorCode::BadRequest,
            "Agent does not support conversation rollback",
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
    fn current_options(&self) -> Option<Vec<ConfigOption>> {
        None
    }
    async fn send(&self, content: Vec<ContentBlock>) -> Result<()>;
    async fn interrupt(&self) -> Result<()>;
    async fn set_option(&self, option_id: &str, value: &str) -> Result<Vec<ConfigOption>>;
    async fn respond(&self, request_id: &str, resolution: RequestResolution) -> Result<()>;
    async fn rewind_files(
        &self,
        _user_message_id: &str,
        _dry_run: bool,
    ) -> Result<RewindFilesResult> {
        Err(AgentError::new(
            ErrorCode::BadRequest,
            "Agent does not support file rewind",
        ))
    }
    async fn detach(&self);
    fn wire_log(&self) -> Arc<WireLog>;
    /// Live processes started by the agent process that hosts this session.
    fn descendants(&self) -> Vec<process::ProcessIdentity> {
        Vec::new()
    }
}
