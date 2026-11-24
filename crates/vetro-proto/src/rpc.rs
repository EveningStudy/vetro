//! Envelopes, methods and events of the UI protocol.
//!
//! Transport: one WebSocket at `/api/ws`, JSON text frames.
//! - client -> server: [`RpcRequest`]
//! - server -> client: [`RpcResponse`] (has `id`) or [`ServerEvent`] (has `event`)
//!
//! The method catalogue lives in `docs/API.md`; parameter and result types are below.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::*;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RpcRequest {
    #[ts(type = "number")]
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    NotFound,
    /// The agent binary is missing or failed to start.
    AgentUnavailable,
    /// The agent needs the user to sign in first.
    AuthRequired,
    /// The thread is read-only history that the agent cannot continue.
    NotResumable,
    Busy,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
}

/// Exactly one of `result` and `error` is present.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct RpcResponse {
    #[ts(type = "number")]
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "event", content = "data", rename_all_fields = "camelCase")]
#[ts(export)]
#[allow(clippy::large_enum_variant)]
pub enum ServerEvent {
    /// Sent only for threads this connection opened. `seq` increases by one per thread.
    #[serde(rename = "thread.event")]
    Thread {
        thread_id: String,
        #[ts(type = "number")]
        seq: u64,
        event: ThreadEvent,
    },
    #[serde(rename = "threads.upserted")]
    ThreadsUpserted { threads: Vec<ThreadSummary> },
    #[serde(rename = "threads.removed")]
    ThreadsRemoved { thread_ids: Vec<String> },
    #[serde(rename = "agents.updated")]
    AgentsUpdated { agents: Vec<AgentInfo> },
    #[serde(rename = "projects.updated")]
    ProjectsUpdated { projects: Vec<Project> },
    /// Background history indexing progress for one agent.
    #[serde(rename = "index.progress")]
    IndexProgress {
        agent_id: String,
        scanned: u32,
        total: u32,
        done: bool,
    },
    #[serde(rename = "remote.updated")]
    RemoteUpdated { status: RemoteStatus },
    /// Local desktop shell request to focus a specific thread (e.g. notification click).
    #[serde(rename = "app.navigate")]
    AppNavigate { thread_id: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Empty {}

// --- app ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct HelloParams {
    pub client_name: String,
    pub protocol_version: u32,
    /// The session string from `POST /api/auth/local` or `/api/auth/pair`. A connection can call nothing
    /// else until a hello with a valid session succeeded.
    pub session: Option<String>,
}

/// Body of a successful `POST /api/auth/local` or `POST /api/auth/pair`. The client keeps `session` in
/// origin-scoped storage and presents it in `app.hello` and as `Authorization: Bearer` on HTTP calls.
/// Vetro sets no cookies.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AuthSession {
    pub session: String,
    pub remote_client: bool,
}

/// UI preferences of the calling device (theme, language, layout). Opaque to the core.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Prefs {
    pub prefs: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppInfo {
    pub version: String,
    pub protocol_version: u32,
    /// `windows`, `macos` or `linux`: the machine the core runs on.
    pub platform: String,
    pub hostname: String,
    pub home_dir: String,
    /// Vetro's only writable location.
    pub data_dir: String,
    /// This connection comes from another device.
    pub remote_client: bool,
}

// --- agents ------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentsResult {
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AuthenticateParams {
    pub agent_id: String,
    pub method_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct AgentOptionsParams {
    pub agent_id: String,
    pub cwd: Option<String>,
    /// Choices already made in the new-thread form (option id to value). Dependent options, such as the
    /// thinking levels of the chosen model, are returned for these choices.
    pub selected: Option<BTreeMap<String, String>>,
    /// The user explicitly asked to load options from the agent. Only then may an agent that reports its
    /// options per session create one; that session is used for the thread the user starts next.
    pub prepare: Option<bool>,
}

/// What the user needs to finish a sign-in that continues outside Vetro (device-code flows).
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AuthenticateResult {
    pub message: Option<String>,
    pub url: Option<String>,
    pub code: Option<String>,
}

/// What a new thread of this agent would start with. Comes from the agent, possibly cached.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentOptionsResult {
    pub options: Vec<ConfigOption>,
    pub commands: Vec<SlashCommand>,
    /// Nothing is cached yet and this agent only reports options from a session. The UI offers an explicit
    /// "load options" action (`prepare`) and otherwise starts the thread with the agent's defaults.
    #[serde(default)]
    pub needs_session: bool,
}

// --- projects ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectsResult {
    pub projects: Vec<Project>,
}

/// Parameters for `projects.add` and `projects.remove`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectAddParams {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct ProjectUpdateParams {
    pub path: String,
    pub name: Option<String>,
    pub pinned: Option<bool>,
    pub hidden: Option<bool>,
}

// --- threads -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct ThreadsListParams {
    pub project_path: Option<String>,
    pub agent_ids: Option<Vec<String>>,
    pub sources: Option<Vec<ThreadSource>>,
    /// Only the sessions started by this thread.
    pub parent_id: Option<String>,
    /// Include sessions that have a parent in a flat listing. Default false: they are reached through the parent.
    pub include_children: Option<bool>,
    /// Include automated or internal background sessions (`ThreadSummary.internal`). Default false.
    pub include_internal: Option<bool>,
    pub include_archived: Option<bool>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadsListResult {
    /// Newest first.
    pub threads: Vec<ThreadSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct ThreadOpenParams {
    pub thread_id: String,
    /// How many of the newest items to return. Default 200.
    pub limit: Option<u32>,
}

/// Opening a thread also subscribes the connection to its `thread.event`s after `seq`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadOpenResult {
    pub thread: ThreadSummary,
    /// Oldest first.
    pub items: Vec<Item>,
    pub has_more: bool,
    #[ts(type = "number")]
    pub seq: u64,
    pub options: Vec<ConfigOption>,
    pub commands: Vec<SlashCommand>,
    pub usage: ThreadUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadIdParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct ThreadItemsParams {
    pub thread_id: String,
    /// Return items with an ordinal below this one.
    #[ts(type = "number")]
    pub before: u64,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadItemsResult {
    pub items: Vec<Item>,
    pub has_more: bool,
}

/// Create a native session and send the first message. `content` goes to the agent unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadStartParams {
    pub agent_id: String,
    pub cwd: String,
    pub content: Vec<ContentBlock>,
    /// Option id to value, for options the user changed from the agent's defaults.
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadResult {
    pub thread: ThreadSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadSendParams {
    pub thread_id: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadSetOptionParams {
    pub thread_id: String,
    pub option_id: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OptionsResult {
    pub options: Vec<ConfigOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadRespondParams {
    pub thread_id: String,
    pub request_id: String,
    pub resolution: RequestResolution,
}

/// Vetro-local metadata only; never written into the agent's own store.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct ThreadUpdateParams {
    pub thread_id: String,
    pub title: Option<String>,
    pub archived: Option<bool>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct ThreadRawParams {
    pub thread_id: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadRawResult {
    /// Oldest first. Empty for history that was never attached in Vetro.
    pub entries: Vec<RawEntry>,
}

// --- search ------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct SearchParams {
    pub query: String,
    pub limit: Option<u32>,
    /// Also return threads of hidden projects. Default false.
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SearchHit {
    pub thread: ThreadSummary,
    /// Matching text with the match wrapped in `<mark>`...`</mark>`; everything else is plain text.
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
}

// --- fs (for the folder picker and @-mentions; read-only) --------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct FsListParams {
    /// Absolute directory. Omitted: the home directory.
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FsListResult {
    pub path: String,
    pub parent: Option<String>,
    /// Directories first, then files, each sorted by name. Drive roots on Windows when `path` is empty.
    pub entries: Vec<FsEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct FsFindParams {
    pub cwd: String,
    /// Fuzzy file-name query. Honors `.gitignore`.
    pub query: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FsFindResult {
    /// Paths relative to `cwd`, with forward slashes.
    pub paths: Vec<String>,
}

// --- workspace (read-only Git inspection & desktop external open) ------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WorkspacePathParams {
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitFileChange {
    /// Path relative to the repository root, using forward slashes.
    pub path: String,
    /// `modified`, `added`, `deleted`, `renamed`, `untracked`, or `other`.
    pub status: String,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitStatusResult {
    pub is_repo: bool,
    pub root: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
    pub files: Vec<GitFileChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitDiffParams {
    pub cwd: String,
    /// File path relative to the repository root (or `cwd`).
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitDiffResult {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    /// Unified diff text when available (capped for large files).
    pub diff: Option<String>,
    pub binary: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum OpenTarget {
    Folder,
    Terminal,
    Editor,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct OpenExternalParams {
    pub cwd: String,
    pub target: OpenTarget,
    /// Optional file path (relative to `cwd` or inside `cwd`) when `target` is `editor`.
    pub path: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, optional_fields)]
pub struct NotifyParams {
    pub title: String,
    pub body: String,
    pub thread_id: Option<String>,
}

// --- settings ----------------------------------------------------------------------------------

/// Per-agent launch settings. Vetro passes nothing to an agent beyond what is listed here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct AgentSettings {
    pub enabled: bool,
    /// Use this binary instead of the one found on the machine.
    pub binary_path: Option<String>,
    /// Extra command-line arguments appended as given.
    pub extra_args: Vec<String>,
    /// Extra environment variables for the agent process.
    pub env: BTreeMap<String, String>,
}

/// A user-defined ACP agent: any command that speaks ACP on stdio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CustomAcpAgent {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct Settings {
    /// Keyed by agent id. Missing entries mean defaults (enabled, auto-detected binary).
    pub agents: BTreeMap<String, AgentSettings>,
    pub custom_acp_agents: Vec<CustomAcpAgent>,
    /// Stop an idle agent process after this many minutes; the session is resumed on demand. 0 disables.
    pub idle_release_minutes: u32,
    /// Keep running in the tray when the window is closed.
    pub close_to_tray: bool,
    /// Fixed port for the local server. 0 picks a free port.
    pub port: u16,
    /// Preferred external editor (`auto` / None, `cursor`, `code`, `zed`, `system`, or a command).
    pub external_editor: Option<String>,
    /// Show native desktop notifications when background threads need input or finish.
    pub notifications_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            agents: BTreeMap::new(),
            custom_acp_agents: Vec::new(),
            idle_release_minutes: 30,
            close_to_tray: false,
            port: 0,
            external_editor: None,
            notifications_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SettingsUpdateParams {
    /// JSON merge patch (RFC 7386) applied to [`Settings`].
    pub patch: Value,
}

// --- remote access -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TailscaleStatus {
    pub installed: bool,
    pub running: bool,
    /// This machine's MagicDNS name, e.g. `host.tailnet.ts.net`.
    pub dns_name: Option<String>,
    /// The `https://` address when `tailscale serve` is forwarding to Vetro.
    pub serve_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteDevice {
    pub id: String,
    pub name: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number | null")]
    pub last_seen_at: Option<i64>,
    /// The device making this request.
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteStatus {
    /// Paired devices may connect. The server itself always listens on loopback only.
    pub enabled: bool,
    pub port: u16,
    pub tailscale: TailscaleStatus,
    pub devices: Vec<RemoteDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteSetParams {
    pub enabled: bool,
    /// Also start or stop `tailscale serve` for Vetro's port.
    pub tailscale_serve: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Pairing {
    /// Short one-time code the user types on the new device.
    pub code: String,
    /// Link that opens Vetro on the new device with the code filled in, when a remote URL is known.
    pub url: Option<String>,
    #[ts(type = "number")]
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteRevokeParams {
    pub device_id: String,
}
