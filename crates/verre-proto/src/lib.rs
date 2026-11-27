//! Wire contract between the Verre core and its UI clients (desktop webview, browser, phone).
//!
//! Everything here is plain data. TypeScript bindings are generated from these types with
//! `cargo test -p verre-proto` into `apps/web/src/api/bindings`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub mod rpc;

pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------------------------

/// How Verre talks to an agent runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentKind {
    /// Claude Code: the `claude` binary driven over its stream-json stdio protocol.
    Claude,
    /// Codex: `codex app-server`.
    Codex,
    /// Any Agent Client Protocol agent over stdio (Antigravity `agy acp`, `devin acp`, ...).
    Acp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AuthState {
    Ok,
    Required,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentAuth {
    pub state: AuthState,
    /// Sign-in methods the agent itself offers. Verre never handles credentials.
    pub methods: Vec<AuthMethod>,
    /// Short human label reported by the agent (plan name, account type), if any.
    pub account: Option<String>,
    /// Active non-default model provider or gateway configured in the agent (e.g. `magpie`, `bedrock`, `litellm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<String>,
    /// Sanitized endpoint host or URL (never credentials) when routing through a custom provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ForkCapabilities {
    /// Agent supports forking from the latest state of a session.
    pub whole: bool,
    /// Agent supports forking from a specific turn id.
    pub at_turn: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryEditCapabilities {
    pub rollback_conversation: bool,
    pub rewind_files: bool,
    pub preview_rewind_files: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentCapabilities {
    pub image_input: bool,
    /// A prompt sent while a turn is running steers that turn instead of queueing.
    pub mid_turn_input: bool,
    /// Sessions created outside Verre can be continued.
    pub resume: bool,
    /// The agent can list its own stored sessions.
    pub list_sessions: bool,
    #[serde(default)]
    pub fork: ForkCapabilities,
    #[serde(default)]
    pub history_edit: HistoryEditCapabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentInfo {
    /// Stable id: `claude`, `codex`, `antigravity`, `devin`, or a user-defined ACP agent id.
    pub id: String,
    pub name: String,
    pub kind: AgentKind,
    pub installed: bool,
    /// Resolved absolute path of the binary Verre will launch.
    pub path: Option<String>,
    pub version: Option<String>,
    pub auth: AgentAuth,
    pub capabilities: AgentCapabilities,
    /// Why the agent is unusable right now, if it is.
    pub error: Option<String>,
    /// Most recent official rate-limit windows reported by this agent (e.g. 5h / weekly).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional = nullable)]
    pub rate_limits: Vec<RateLimit>,
    /// Unix timestamp (ms) when `rate_limits` were last observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number | null")]
    pub rate_limits_updated_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ConfigCategory {
    Mode,
    Model,
    ThoughtLevel,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConfigChoice {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
    pub group: Option<String>,
    /// True when this choice relaxes approval or filesystem safeguards (e.g. bypass / full access).
    #[serde(default)]
    pub danger: bool,
}

/// A session setting reported by the agent (model, mode, thinking level, ...). The UI renders
/// these generically; Verre never invents options the agent does not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: ConfigCategory,
    pub current_value: String,
    pub choices: Vec<ConfigChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub input_hint: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Projects and threads
// ---------------------------------------------------------------------------------------------

/// A working directory that has threads. Derived from session history plus folders the user added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Project {
    /// Normalized absolute path; also the project id.
    pub path: String,
    pub name: String,
    pub exists: bool,
    pub pinned: bool,
    pub hidden: bool,
    pub thread_count: u32,
    #[ts(type = "number | null")]
    pub last_activity_at: Option<i64>,
    /// Human-initiated root thread count (excludes child and internal/automated sessions).
    #[serde(default)]
    pub root_thread_count: u32,
    /// Latest update time among human-initiated root threads in this directory.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub last_root_activity_at: Option<i64>,
}

/// Where a session was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ThreadSource {
    #[serde(alias = "vetro")]
    Verre,
    /// The vendor's desktop app.
    Desktop,
    /// The vendor's interactive terminal UI.
    Cli,
    /// A non-interactive run (`codex exec`, `claude -p`, `agy -p`, `devin -p`), usually another agent.
    Exec,
    Ide,
    Cloud,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ThreadStatus {
    Idle,
    Running,
    /// Blocked on a permission request or a question for the user.
    AwaitingInput,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LinkConfidence {
    /// The child session id appears in the parent's tool output, or the agent reported the link.
    Exact,
    /// The child's first prompt appears in the parent's command.
    Prompt,
    /// Matched on time window and working directory only.
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadParent {
    pub thread_id: String,
    /// The tool call in the parent that started this session, when known.
    pub call_id: Option<String>,
    pub confidence: LinkConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadSummary {
    /// `<agentId>:<nativeId>`.
    pub id: String,
    pub agent_id: String,
    /// The agent's own session id.
    pub native_id: String,
    pub title: String,
    pub preview: Option<String>,
    pub cwd: Option<String>,
    #[ts(type = "number | null")]
    pub created_at: Option<i64>,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub source: ThreadSource,
    /// Raw origin label from the agent's own records (`Codex Desktop`, `codex_exec`, `claude-desktop`, ...).
    pub origin: Option<String>,
    pub status: ThreadStatus,
    /// A native session is currently attached inside Verre.
    pub live: bool,
    /// Sending a message will continue the native session. False means read-only history.
    pub resumable: bool,
    pub parent: Option<ThreadParent>,
    pub child_count: u32,
    pub model: Option<String>,
    pub archived: bool,
    /// Verre-local pin flag; never written to the agent's own store.
    #[serde(default)]
    pub pinned: bool,
    /// Automated or internal background session (e.g. Codex `subAgent*`, blank history projection, unlinked `exec`).
    /// Hidden from the primary sidebar by default unless explicitly requested.
    #[serde(default)]
    pub internal: bool,
}

// ---------------------------------------------------------------------------------------------
// Timeline items
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        /// Base64 payload. Either `data` or `uri` is set.
        data: Option<String>,
        uri: Option<String>,
    },
    /// A file or directory the user mentioned or attached by path.
    File {
        path: String,
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    /// A sub-agent or task spawned by the agent itself.
    Agent,
    Mcp,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ToolContent {
    Text {
        text: String,
    },
    /// A file change. Agents report either both texts or a unified diff.
    Diff {
        path: String,
        old_text: Option<String>,
        new_text: Option<String>,
        unified: Option<String>,
    },
    Terminal {
        command: Option<String>,
        cwd: Option<String>,
        output: String,
        exit_code: Option<i32>,
    },
    Image {
        mime_type: String,
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolLocation {
    pub path: String,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolCall {
    pub call_id: String,
    pub kind: ToolKind,
    /// The agent's own tool name (`Bash`, `shell`, `run_command`, `mcp__server__tool`, ...).
    pub name: Option<String>,
    pub title: String,
    pub status: ToolStatus,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub content: Vec<ToolContent>,
    pub locations: Vec<ToolLocation>,
    /// Sessions of other agents that this call started (for example a `codex exec` run).
    pub child_thread_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct QuestionChoice {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Question {
    pub id: String,
    pub header: Option<String>,
    pub text: String,
    pub choices: Vec<QuestionChoice>,
    pub multi_select: bool,
    pub allow_free_text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum RequestBody {
    /// The agent wants to run a tool and asks first.
    Permission { options: Vec<PermissionOption> },
    /// The agent asks the user to choose or answer.
    Questions { questions: Vec<Question> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum RequestResolution {
    Selected {
        option_id: String,
    },
    /// Answers keyed by question id.
    Answered {
        answers: BTreeMap<String, Vec<String>>,
    },
    Cancelled,
}

/// Something the agent is waiting on the user for. Stays in the timeline after it is resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InteractionRequest {
    pub request_id: String,
    /// The tool call this request is about, when there is one.
    pub call_id: Option<String>,
    pub title: String,
    pub detail: Option<String>,
    pub body: RequestBody,
    pub resolution: Option<RequestResolution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StopReason {
    EndTurn,
    Cancelled,
    MaxTokens,
    Refusal,
    Error,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TokenUsage {
    #[ts(type = "number | null")]
    pub input: Option<u64>,
    #[ts(type = "number | null")]
    pub cached_input: Option<u64>,
    #[ts(type = "number | null")]
    pub output: Option<u64>,
    #[ts(type = "number | null")]
    pub reasoning: Option<u64>,
    /// Tokens currently in the model's context, as reported by the agent.
    #[ts(type = "number | null")]
    pub context_used: Option<u64>,
    #[ts(type = "number | null")]
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ItemKind {
    UserMessage {
        content: Vec<ContentBlock>,
    },
    AgentMessage {
        /// Markdown.
        text: String,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
    },
    ToolCall(ToolCall),
    Plan {
        entries: Vec<PlanEntry>,
    },
    Request(InteractionRequest),
    /// Hook output, compaction, warnings and errors that the agent surfaced.
    Notice {
        level: NoticeLevel,
        text: String,
        detail: Option<Value>,
    },
    TurnEnd {
        stop_reason: StopReason,
        usage: Option<TokenUsage>,
        #[ts(type = "number | null")]
        duration_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Item {
    /// Unique within the thread and stable across reloads.
    pub id: String,
    /// Position in the thread; strictly increasing.
    #[ts(type = "number")]
    pub ordinal: u64,
    /// Milliseconds since the Unix epoch.
    #[ts(type = "number | null")]
    pub ts: Option<i64>,
    /// Items produced in response to the same user message share a turn id.
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub kind: ItemKind,
}

/// Account-level limits the agent reported (five-hour window, weekly window, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RateLimit {
    pub label: String,
    pub used_percent: f64,
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadUsage {
    pub last_turn: Option<TokenUsage>,
    pub total: Option<TokenUsage>,
    pub rate_limits: Vec<RateLimit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct QueuedMessage {
    pub id: String,
    pub content: Vec<ContentBlock>,
}

/// Everything about one thread that changes while it is open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ThreadEvent {
    ItemAdded {
        item: Item,
    },
    /// Full replacement of an existing item (tool call progress, resolved request, ...).
    ItemUpdated {
        item: Item,
    },
    /// Append to the `text` of a streaming agent message or reasoning item.
    TextDelta {
        item_id: String,
        delta: String,
    },
    Summary {
        thread: ThreadSummary,
    },
    Options {
        options: Vec<ConfigOption>,
    },
    Commands {
        commands: Vec<SlashCommand>,
    },
    Usage {
        usage: ThreadUsage,
    },
    Queue {
        messages: Vec<QueuedMessage>,
    },
}

/// One line of the raw wire log: exactly what Verre sent to or received from the agent process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RawEntry {
    #[ts(type = "number")]
    pub ts: i64,
    pub direction: RawDirection,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RawDirection {
    /// Verre to agent (stdin).
    Sent,
    /// Agent to Verre (stdout).
    Received,
    /// Agent stderr.
    Stderr,
    /// The command line and environment overrides used to start the process.
    Spawn,
}
