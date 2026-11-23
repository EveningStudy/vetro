use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use vetro_agent_api::{
    async_trait,
    jsonrpc::{Error, Id, RemoteError},
    AgentError, AgentOptions, EventSink, ItemDraft, Result, Session, SessionEvent, Transcript,
    WireLog,
};
use vetro_proto::{
    rpc::ErrorCode, ConfigCategory, ConfigOption, ContentBlock, ItemKind, NoticeLevel, PlanEntry,
    PlanStatus, RequestResolution, SlashCommand, StopReason, ThreadStatus, ThreadUsage, TokenUsage,
    ToolCall, ToolKind, ToolStatus,
};

use crate::{
    interaction::{cancelled, Interaction},
    mapping::{self, array, string},
    wire::{agent_error, Connection, Reply},
};

static NEXT_LIVE_INSTANCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct AcpSession {
    pub connection: Arc<Connection>,
    pub id: String,
    pub cwd: PathBuf,
    image: bool,
    steering: bool,
    pub can_close: bool,
    outbound: AsyncMutex<()>,
    responses: AsyncMutex<()>,
    state: Mutex<State>,
    initial_options: OnceLock<AgentOptions>,
    log: Arc<WireLog>,
}

#[derive(Default)]
struct State {
    sink: Option<EventSink>,
    replay: Option<Replay>,
    closed: bool,
    detaching: bool,
    instance: u64,
    counter: u64,
    replay_turn: u64,
    config: mapping::Config,
    commands: Vec<SlashCommand>,
    title: Option<String>,
    open: Option<Message>,
    ended_messages: HashMap<String, ItemDraft>,
    tools: HashMap<String, ItemDraft>,
    interactions: HashMap<String, PendingInteraction>,
    turn: Option<Turn>,
    usage: ThreadUsage,
}

struct Turn {
    id: String,
    since: Instant,
    pending: usize,
    user: Option<Vec<ContentBlock>>,
    usage: Option<TokenUsage>,
}

#[derive(Default)]
struct Replay {
    transcript: Transcript,
    index: HashMap<String, usize>,
}

struct PendingInteraction {
    wire_id: Id,
    protocol: Interaction,
    draft: ItemDraft,
}

struct Message {
    role: Role,
    native_id: Option<String>,
    draft: ItemDraft,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Agent,
    Thought,
}

impl AcpSession {
    pub fn new(
        connection: Arc<Connection>,
        id: String,
        cwd: PathBuf,
        capabilities: &crate::Capabilities,
        sink: Option<EventSink>,
        capture_replay: bool,
    ) -> Arc<Self> {
        let log = if capture_replay {
            Arc::new(WireLog::default())
        } else {
            connection.log().session(&id)
        };
        Arc::new(Self {
            connection,
            id,
            cwd,
            image: capabilities.image,
            steering: capabilities.steering,
            can_close: capabilities.close,
            outbound: AsyncMutex::new(()),
            responses: AsyncMutex::new(()),
            initial_options: OnceLock::new(),
            log,
            state: Mutex::new(State {
                sink,
                replay: capture_replay.then(Replay::default),
                instance: if capture_replay {
                    0
                } else {
                    NEXT_LIVE_INSTANCE.fetch_add(1, Ordering::Relaxed)
                },
                ..State::default()
            }),
        })
    }

    pub fn initialize(&self, value: &Value) {
        let mut state = self.state.lock().unwrap();
        if state.config.update(value) {
            state.emit_options();
        }
        if value["availableCommands"].is_array() {
            state.set_commands(&value["availableCommands"]);
        }
    }

    pub fn options(&self) -> AgentOptions {
        let state = self.state.lock().unwrap();
        AgentOptions {
            options: state.config.options(),
            commands: state.commands.clone(),
        }
    }

    pub fn capture_initial_options(&self) {
        self.initial_options.get_or_init(|| self.options());
    }

    async fn submit_prompt(
        self: &Arc<Self>,
        params: Value,
        turn_id: String,
        retry: bool,
    ) -> Result<()> {
        let session = Arc::downgrade(self);
        let retry_params = retry.then(|| params.clone());
        self.connection
            .submit("session/prompt", params, move |reply| {
                let Some(session) = session.upgrade() else {
                    return;
                };
                if let (Some(params), Err(Error::Remote(error))) = (retry_params, &reply) {
                    if error.code == -32000 {
                        session.connection.observe_auth("session/prompt", &reply);
                        tokio::spawn(async move {
                            if session.has_turn(&turn_id)
                                && matches!(session.connection.recover_auth().await, Ok(true))
                                && session.has_turn(&turn_id)
                                && session
                                    .submit_prompt_once(params, turn_id.clone())
                                    .await
                                    .is_ok()
                            {
                                return;
                            }
                            session.complete_prompt(&turn_id, reply);
                        });
                        return;
                    }
                }
                session.complete_prompt(&turn_id, reply);
            })
            .await
    }

    async fn submit_prompt_once(self: &Arc<Self>, params: Value, turn_id: String) -> Result<()> {
        let session = Arc::downgrade(self);
        self.connection
            .submit("session/prompt", params, move |reply| {
                if let Some(session) = session.upgrade() {
                    session.complete_prompt(&turn_id, reply);
                }
            })
            .await
    }

    pub fn activate(&self, value: &Value, sink: EventSink) {
        self.initialize(value);
        let mut state = self.state.lock().unwrap();
        state.end_message();
        state.ended_messages.clear();
        if state.closed {
            return;
        }
        state.sink = Some(sink);
        state.emit_options();
        if let Some(title) = &state.title {
            state.emit(SessionEvent::Title(title.clone()));
        }
        state.emit(SessionEvent::Commands(state.commands.clone()));
    }

    pub fn take_replay(&self) -> Transcript {
        let mut state = self.state.lock().unwrap();
        state.end_message();
        state
            .replay
            .take()
            .map(|replay| replay.transcript)
            .unwrap_or_default()
    }

    pub fn closed(&self, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.flush_user();
            state.end_message();
            state.interactions.clear();
            state.turn = None;
            state.closed = true;
            state.emit(SessionEvent::Closed { error });
            state.sink = None;
        }
    }

    pub fn update(&self, value: &Value) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        match value["sessionUpdate"].as_str() {
            Some("user_message_chunk") => state.message(Role::User, value),
            Some("agent_message_chunk") => state.message(Role::Agent, value),
            Some("agent_thought_chunk") => state.message(Role::Thought, value),
            Some("tool_call" | "tool_call_update") => {
                state.flush_user();
                state.end_message();
                state.tool(value);
            }
            Some("plan") => {
                state.flush_user();
                state.end_message();
                let entries = array(&value["entries"])
                    .filter_map(|entry| {
                        Some(PlanEntry {
                            content: string(entry, "content")?,
                            status: match entry["status"].as_str()? {
                                "pending" => PlanStatus::Pending,
                                "in_progress" => PlanStatus::InProgress,
                                "completed" => PlanStatus::Completed,
                                _ => return None,
                            },
                        })
                    })
                    .collect();
                let id = match &state.turn {
                    Some(turn) => format!("plan:{}", turn.id),
                    None => format!("plan:{}:{}", state.instance, state.replay_turn),
                };
                let draft = state.draft(id, ItemKind::Plan { entries });
                state.upsert(draft);
            }
            Some("available_commands_update") => state.set_commands(&value["availableCommands"]),
            Some("current_mode_update") => {
                if let Some(mode) = value["currentModeId"].as_str() {
                    if state.config.set_mode(mode) {
                        state.emit_options();
                    }
                }
            }
            Some("config_option_update") => {
                if state.config.update(value) {
                    state.emit_options();
                }
            }
            Some("session_info_update") => {
                if let Some(title) = string(value, "title") {
                    state.emit(SessionEvent::Title(title.clone()));
                    state.title = Some(title);
                }
            }
            Some("usage_update") => state.update_usage(value),
            _ => {}
        }
    }

    pub fn interaction(
        &self,
        wire_id: Id,
        method: &str,
        params: &Value,
    ) -> std::result::Result<Option<Value>, RemoteError> {
        let mut state = self.state.lock().unwrap();
        if state.sink.is_none() || state.closed || state.detaching {
            return Ok(Some(cancelled(method)));
        }
        let id = state.id("request");
        let (protocol, request) = Interaction::parse(method, params, id.clone())?;
        if matches!(&request.body, vetro_proto::RequestBody::Permission { options } if options.is_empty())
        {
            return Ok(Some(cancelled(method)));
        }
        state.flush_user();
        state.end_message();
        let draft = state.draft(id.clone(), ItemKind::Request(request));
        state.upsert(draft.clone());
        state.interactions.insert(
            id,
            PendingInteraction {
                wire_id,
                protocol,
                draft,
            },
        );
        state.emit(SessionEvent::Status(ThreadStatus::AwaitingInput));
        Ok(None)
    }

    fn has_turn(&self, turn_id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .turn
            .as_ref()
            .is_some_and(|turn| turn.id == turn_id)
    }

    fn complete_prompt(&self, turn_id: &str, reply: Reply) {
        self.connection.observe_auth("session/prompt", &reply);
        let mut state = self.state.lock().unwrap();
        if state.turn.as_ref().is_none_or(|turn| turn.id != turn_id) {
            return;
        }
        if matches!(&reply, Err(Error::Remote(error)) if error.code == -32000) {
            state.emit(SessionEvent::AuthRequired);
        }
        let Some(turn) = state.turn.as_mut() else {
            return;
        };
        turn.pending -= 1;
        if turn.pending > 0 {
            return;
        }
        let reason = match &reply {
            Ok(value) => match value["stopReason"].as_str() {
                Some("cancelled" | "canceled") => StopReason::Cancelled,
                Some("max_tokens" | "max_turn_requests") => StopReason::MaxTokens,
                Some("refusal") => StopReason::Refusal,
                Some("error") => StopReason::Error,
                _ => StopReason::EndTurn,
            },
            Err(Error::Remote(error)) if error.code == -32800 => StopReason::Cancelled,
            Err(_) => StopReason::Error,
        };
        if let Err(error) = &reply {
            if reason == StopReason::Error {
                let id = state.id("notice");
                let draft = state.draft(
                    id,
                    ItemKind::Notice {
                        level: NoticeLevel::Error,
                        text: error.to_string(),
                        detail: None,
                    },
                );
                state.upsert(draft);
            }
        }
        Self::finish_prompt(&mut state, reason);
    }

    fn finish_prompt(state: &mut State, reason: StopReason) {
        state.flush_user();
        state.end_message();
        let turn = state.turn.as_ref().unwrap();
        let draft = state.draft(
            format!("{}:end", turn.id),
            ItemKind::TurnEnd {
                stop_reason: reason,
                usage: turn.usage.clone(),
                duration_ms: u64::try_from(turn.since.elapsed().as_millis()).ok(),
            },
        );
        state.upsert(draft);
        state.turn = None;
        state.ended_messages.clear();
        state.status_after_input(reason == StopReason::Error);
    }

    async fn cancel_interactions(&self) {
        let ids = self
            .state
            .lock()
            .unwrap()
            .interactions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.respond(&id, RequestResolution::Cancelled).await;
        }
    }

    async fn respond_pending(&self, id: &str, resolution: RequestResolution) -> Result<()> {
        let _response = self.responses.lock().await;
        let (wire_id, reply) = {
            let state = self.state.lock().unwrap();
            state.ensure_open()?;
            let pending = state
                .interactions
                .get(id)
                .ok_or_else(|| AgentError::new(ErrorCode::NotFound, "Unknown ACP interaction"))?;
            let ItemKind::Request(request) = &pending.draft.kind else {
                unreachable!()
            };
            (
                pending.wire_id.clone(),
                pending.protocol.reply(&request.body, &resolution)?,
            )
        };
        self.connection.respond(wire_id, Ok(reply)).await?;
        let mut state = self.state.lock().unwrap();
        if let Some(mut pending) = state.interactions.remove(id) {
            if let ItemKind::Request(request) = &mut pending.draft.kind {
                request.resolution = Some(resolution);
            }
            state.upsert(pending.draft.clone());
            state.status_after_input(false);
        }
        Ok(())
    }
}

#[async_trait]
impl Session for AcpSession {
    fn native_id(&self) -> &str {
        &self.id
    }

    fn initial_options(&self) -> Option<AgentOptions> {
        self.initial_options.get().cloned()
    }

    async fn send(&self, content: Vec<ContentBlock>) -> Result<()> {
        let prompt = mapping::prompt(&content, &self.cwd, self.image)?;
        let _outbound = self.outbound.lock().await;
        let requires_auth =
            *self.connection.auth.lock().unwrap() == vetro_proto::AuthState::Required;
        if requires_auth && !self.connection.recover_auth().await? {
            return Err(AgentError::new(
                ErrorCode::AuthRequired,
                "Sign in to the agent first",
            ));
        }
        let session = self
            .connection
            .session(&self.id)
            .ok_or_else(|| agent_error(Error::Closed))?;
        let turn_id = {
            let mut state = self.state.lock().unwrap();
            state.ensure_open()?;
            if state.turn.is_some() && !self.steering {
                return Err(AgentError::new(
                    ErrorCode::Busy,
                    "An ACP turn is already running",
                ));
            }
            state.flush_user();
            state.end_message();
            if let Some(turn) = state.turn.as_mut() {
                turn.pending += 1;
                turn.user = Some(content);
            } else {
                state.turn = Some(Turn {
                    id: state.id("turn"),
                    since: Instant::now(),
                    pending: 1,
                    user: Some(content),
                    usage: None,
                });
            }
            state.status_after_input(false);
            state.turn.as_ref().unwrap().id.clone()
        };
        session
            .submit_prompt(
                json!({ "sessionId": self.id, "prompt": prompt }),
                turn_id,
                true,
            )
            .await
    }

    async fn interrupt(&self) -> Result<()> {
        let _outbound = self.outbound.lock().await;
        let turn_id = {
            let state = self.state.lock().unwrap();
            state.ensure_open()?;
            state.turn.as_ref().map(|turn| turn.id.clone())
        };
        self.connection
            .notify("session/cancel", json!({ "sessionId": self.id }))
            .await?;
        if let (Some(turn_id), Some(session)) = (turn_id, self.connection.session(&self.id)) {
            let session = Arc::downgrade(&session);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if let Some(session) = session.upgrade() {
                    let mut state = session.state.lock().unwrap();
                    if state.turn.as_ref().is_some_and(|turn| turn.id == turn_id) {
                        Self::finish_prompt(&mut state, StopReason::Cancelled);
                    }
                }
            });
        }
        self.cancel_interactions().await;
        Ok(())
    }

    async fn set_option(&self, id: &str, value: &str) -> Result<Vec<ConfigOption>> {
        let _outbound = self.outbound.lock().await;
        let (method, mut params) = {
            let state = self.state.lock().unwrap();
            state.ensure_open()?;
            state.config.request(id, value)?
        };
        params["sessionId"] = json!(self.id);
        let session = Arc::downgrade(
            &self
                .connection
                .session(&self.id)
                .ok_or_else(|| agent_error(Error::Closed))?,
        );
        let id = id.to_owned();
        let value = value.to_owned();
        let reply = self
            .connection
            .call_ordered(method, params, move |reply| {
                if let (Ok(result), Some(session)) = (reply, session.upgrade()) {
                    let mut state = session.state.lock().unwrap();
                    if state.config.update(result) || state.config.set_current(&id, &value) {
                        state.emit_options();
                    }
                }
            })
            .await;
        self.connection.observe_auth(method, &reply);
        reply.map_err(agent_error)?;
        Ok(self.options().options)
    }

    async fn respond(&self, id: &str, resolution: RequestResolution) -> Result<()> {
        let session = self
            .connection
            .session(&self.id)
            .ok_or_else(|| agent_error(Error::Closed))?;
        let id = id.to_owned();
        tokio::spawn(async move { session.respond_pending(&id, resolution).await })
            .await
            .map_err(|error| AgentError::new(ErrorCode::Internal, error.to_string()))?
    }

    async fn detach(&self) {
        let _lifecycle = self.connection.lifecycle.lock().await;
        let _outbound = self.outbound.lock().await;
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return;
            }
            state.detaching = true;
        }
        self.cancel_interactions().await;
        if self.can_close {
            let _ = self
                .connection
                .call("session/close", json!({ "sessionId": self.id }))
                .await;
        } else if self.state.lock().unwrap().turn.is_some() {
            let _ = self
                .connection
                .notify("session/cancel", json!({ "sessionId": self.id }))
                .await;
        }
        self.connection.unregister(&self.id);
        self.closed(None);
    }

    fn wire_log(&self) -> Arc<WireLog> {
        self.log.clone()
    }
}

impl State {
    fn ensure_open(&self) -> Result<()> {
        if self.closed {
            Err(agent_error(Error::Closed))
        } else {
            Ok(())
        }
    }

    fn id(&mut self, kind: &str) -> String {
        self.counter += 1;
        format!("acp:{}:{kind}:{}", self.instance, self.counter)
    }

    fn draft(&self, id: String, kind: ItemKind) -> ItemDraft {
        ItemDraft {
            id,
            ts: None,
            turn_id: self.turn.as_ref().map(|turn| turn.id.clone()),
            kind,
        }
    }

    fn emit(&self, event: SessionEvent) {
        if let Some(sink) = &self.sink {
            let _ = sink.send(event);
        }
    }

    fn upsert(&mut self, draft: ItemDraft) {
        if let Some(replay) = &mut self.replay {
            if let Some(index) = replay.index.get(&draft.id) {
                replay.transcript.items[*index] = draft.clone();
            } else {
                replay
                    .index
                    .insert(draft.id.clone(), replay.transcript.items.len());
                replay.transcript.items.push(draft.clone());
            }
        }
        self.emit(SessionEvent::Item(draft));
    }

    fn emit_options(&self) {
        let options = self.config.options();
        let model = options
            .iter()
            .find(|option| option.category == ConfigCategory::Model)
            .map(|option| option.current_value.clone());
        if model.is_some() {
            self.emit(SessionEvent::Model(model));
        }
        self.emit(SessionEvent::Options(options));
    }

    fn set_commands(&mut self, commands: &Value) {
        self.commands = array(commands).filter_map(mapping::command).collect();
        self.emit(SessionEvent::Commands(self.commands.clone()));
    }

    fn status_after_input(&self, error: bool) {
        let status = if !self.interactions.is_empty() {
            ThreadStatus::AwaitingInput
        } else if self.turn.is_some() {
            ThreadStatus::Running
        } else if error {
            ThreadStatus::Error
        } else {
            ThreadStatus::Idle
        };
        self.emit(SessionEvent::Status(status));
    }

    fn flush_user(&mut self) {
        if let Some(content) = self.turn.as_mut().and_then(|turn| turn.user.take()) {
            self.end_message();
            let id = self.id("user");
            let draft = self.draft(id, ItemKind::UserMessage { content });
            self.upsert(draft);
        }
    }

    fn message(&mut self, role: Role, value: &Value) {
        let Some(block) = mapping::content(&value["content"]) else {
            return;
        };
        if role == Role::User {
            if let Some(turn) = &mut self.turn {
                turn.user = None;
            }
        } else {
            self.flush_user();
            if !matches!(block, ContentBlock::Text { .. }) {
                let id = self.id("content");
                let draft = self.draft(
                    id,
                    ItemKind::Notice {
                        level: NoticeLevel::Info,
                        text: "Agent content".into(),
                        detail: Some(value["content"].clone()),
                    },
                );
                self.end_message();
                self.upsert(draft);
                return;
            }
        }
        let native_id = string(value, "messageId");
        let is_same_message = self
            .open
            .as_ref()
            .is_some_and(|message| message.role == role && message.native_id == native_id);
        if !is_same_message {
            self.end_message();
            let id = native_id
                .as_ref()
                .map(|id| format!("message:{id}"))
                .unwrap_or_else(|| self.id("message"));
            if role == Role::User && self.replay.is_some() && !self.ended_messages.contains_key(&id)
            {
                self.replay_turn += 1;
            }
            let kind = match role {
                Role::User => ItemKind::UserMessage {
                    content: Vec::new(),
                },
                Role::Agent => ItemKind::AgentMessage {
                    text: String::new(),
                    streaming: true,
                },
                Role::Thought => ItemKind::Reasoning {
                    text: String::new(),
                    streaming: true,
                },
            };
            let mut draft = self
                .ended_messages
                .remove(&id)
                .unwrap_or_else(|| self.draft(id, kind));
            if let ItemKind::AgentMessage { streaming, .. }
            | ItemKind::Reasoning { streaming, .. } = &mut draft.kind
            {
                *streaming = true;
            }
            draft.ts = draft.ts.or_else(|| mapping::timestamp(&value["timestamp"]));
            self.open = Some(Message {
                role,
                native_id,
                draft,
            });
        }
        let message = self.open.as_mut().unwrap();
        let delta = match (&mut message.draft.kind, block) {
            (ItemKind::UserMessage { content }, block) => {
                if let (Some(ContentBlock::Text { text }), ContentBlock::Text { text: delta }) =
                    (content.last_mut(), &block)
                {
                    text.push_str(delta);
                } else {
                    content.push(block);
                }
                None
            }
            (
                ItemKind::AgentMessage { text, .. } | ItemKind::Reasoning { text, .. },
                ContentBlock::Text { text: delta },
            ) => {
                text.push_str(&delta);
                Some(delta)
            }
            _ => return,
        };
        let draft = message.draft.clone();
        if let Some(delta) = delta.filter(|_| is_same_message && self.replay.is_none()) {
            self.emit(SessionEvent::TextDelta {
                item_id: draft.id,
                delta,
            });
        } else {
            self.upsert(draft);
        }
    }

    fn end_message(&mut self) {
        let Some(mut message) = self.open.take() else {
            return;
        };
        if let ItemKind::AgentMessage { streaming, .. } | ItemKind::Reasoning { streaming, .. } =
            &mut message.draft.kind
        {
            *streaming = false;
            self.upsert(message.draft.clone());
        }
        if message.native_id.is_some() {
            self.ended_messages
                .insert(message.draft.id.clone(), message.draft);
        }
    }

    fn tool(&mut self, value: &Value) {
        let Some(call_id) = string(value, "toolCallId") else {
            return;
        };
        let id = format!("tool:{call_id}");
        let mut draft = self.tools.remove(&id).unwrap_or_else(|| {
            self.draft(
                id.clone(),
                ItemKind::ToolCall(ToolCall {
                    call_id,
                    kind: ToolKind::Other,
                    name: None,
                    title: String::new(),
                    status: ToolStatus::Pending,
                    input: None,
                    output: None,
                    content: Vec::new(),
                    locations: Vec::new(),
                    child_thread_ids: Vec::new(),
                }),
            )
        });
        draft.ts = draft.ts.or_else(|| mapping::timestamp(&value["timestamp"]));
        if let ItemKind::ToolCall(tool) = &mut draft.kind {
            mapping::patch_tool(tool, value);
        }
        self.tools.insert(id, draft.clone());
        self.upsert(draft);
    }

    fn update_usage(&mut self, value: &Value) {
        let usage = TokenUsage {
            context_used: value["used"].as_u64(),
            context_window: value["size"].as_u64(),
            input: value["inputTokens"].as_u64(),
            cached_input: value["cachedInputTokens"].as_u64(),
            output: value["outputTokens"].as_u64(),
            reasoning: value["reasoningTokens"].as_u64(),
        };
        if usage == TokenUsage::default() {
            return;
        }
        self.usage.total = Some(usage.clone());
        if let Some(turn) = &mut self.turn {
            turn.usage = Some(usage.clone());
            self.usage.last_turn = Some(usage);
        }
        self.emit(SessionEvent::Usage(self.usage.clone()));
    }
}
