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
use verre_agent_api::{
    async_trait,
    jsonrpc::{Error, Id, RemoteError},
    AgentError, AgentOptions, EventSink, ItemDraft, Result, Session, SessionEvent, Transcript,
    WireLog,
};
use verre_proto::{
    rpc::ErrorCode, ConfigCategory, ConfigOption, ContentBlock, InteractionRequest, ItemKind,
    NoticeLevel, PermissionOptionKind, PlanEntry, PlanStatus, RequestBody, RequestResolution,
    SlashCommand, StopReason, ThreadStatus, ThreadUsage, TokenUsage,
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
    kiro_steering: bool,
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
    antigravity: bool,
    turbo: bool,
    permission_preset: Option<ConfigOption>,
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
    user_item_id: Option<String>,
    usage: Option<TokenUsage>,
    steering: Steering,
}

#[derive(Default)]
struct Steering {
    queued: HashMap<String, Vec<ContentBlock>>,
    early: Option<HashMap<String, bool>>,
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
        let antigravity = connection.agent_id == "antigravity";
        Arc::new(Self {
            connection,
            id,
            cwd,
            image: capabilities.image,
            steering: capabilities.steering,
            kiro_steering: capabilities.kiro_steering,
            can_close: capabilities.close,
            outbound: AsyncMutex::new(()),
            responses: AsyncMutex::new(()),
            initial_options: OnceLock::new(),
            log,
            state: Mutex::new(State {
                sink,
                replay: capture_replay.then(Replay::default),
                antigravity,
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
            state.sync_turbo();
            state.emit_options();
        }
        if value["availableCommands"].is_array() {
            state.set_commands(&value["availableCommands"]);
        }
    }

    pub fn options(&self) -> AgentOptions {
        let state = self.state.lock().unwrap();
        AgentOptions {
            options: state.options(),
            commands: state.commands.clone(),
        }
    }

    pub fn set_permission_preset(&self, permission_preset: Option<ConfigOption>) {
        self.state.lock().unwrap().permission_preset = permission_preset;
    }

    pub fn capture_initial_options(&self) {
        self.initial_options.get_or_init(|| {
            let mut options = self.options();
            if let Some(preset) = options
                .options
                .iter_mut()
                .find(|option| option.id == crate::KIRO_PERMISSIONS)
            {
                preset.current_value = crate::KIRO_DEFAULT_PERMISSIONS.into();
            }
            options
        });
    }

    pub fn emit_notice(&self, level: NoticeLevel, text: String) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        let id = state.id("notice");
        let draft = state.draft(
            id,
            ItemKind::Notice {
                level,
                text,
                detail: None,
            },
        );
        state.upsert(draft);
    }

    async fn submit_prompt(
        self: &Arc<Self>,
        params: Value,
        turn_id: String,
        retry: bool,
    ) -> Result<()> {
        let session = Arc::downgrade(self);
        let retry_params = retry.then(|| params.clone());
        self.connection.begin_user_work();
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
                                && matches!(session.connection.recover_auth(None).await, Ok(true))
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
            .map(|mut replay| {
                replay.transcript.items = replay_turns(replay.transcript.items);
                replay.transcript
            })
            .unwrap_or_default()
    }

    pub fn closed(&self, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.flush_user();
            state.end_message();
            state.interactions.clear();
            state.clear_steering();
            state.turn.take();
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
        let update = value["sessionUpdate"].as_str();
        match update {
            Some("user_message_chunk") => {
                state.message(Role::User, value, &self.connection.agent_id)
            }
            Some("agent_message_chunk") => {
                state.message(Role::Agent, value, &self.connection.agent_id)
            }
            Some("agent_thought_chunk") => {
                state.message(Role::Thought, value, &self.connection.agent_id)
            }
            Some("tool_call" | "tool_call_update") => {
                state.flush_user();
                state.tool(
                    value,
                    update == Some("tool_call"),
                    &self.connection.agent_id,
                );
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
                        state.sync_turbo();
                        state.emit_options();
                    }
                }
            }
            Some("config_option_update") => {
                if state.config.update(value) {
                    state.sync_turbo();
                    state.emit_options();
                }
            }
            Some("session_info_update") => {
                if self.kiro_steering {
                    let update = &value["_meta"]["kiro"];
                    match update["kind"].as_str() {
                        Some("steering_injected") => {
                            if let Some(id) = update["messageId"].as_str() {
                                state.steering_receipt(id, true);
                            }
                        }
                        Some("steering_cleared") => {
                            for id in array(&update["messageIds"]).filter_map(Value::as_str) {
                                state.steering_receipt(id, false);
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(title) = string(value, "title") {
                    state.emit(SessionEvent::Title(title.clone()));
                    state.title = Some(title);
                }
            }
            Some("usage_update") => state.update_usage(value),
            _ => verre_agent_api::drift::report(
                &self.connection.agent_id,
                "ACP sessionUpdate",
                update,
            ),
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
        let (protocol, request) = Interaction::parse(
            method,
            params,
            id.clone(),
            self.connection.agent_id == "kiro",
        )?;
        if matches!(&request.body, verre_proto::RequestBody::Permission { options } if options.is_empty())
        {
            return Ok(Some(cancelled(method)));
        }
        if state.turbo && method == "session/request_permission" {
            if let Some(option_id) = auto_approve_permission_option(&request) {
                if let Ok(reply) =
                    protocol.reply(&request.body, &RequestResolution::Selected { option_id })
                {
                    return Ok(Some(reply));
                }
            }
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
        state.clear_steering();
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

    async fn steer(self: &Arc<Self>, turn_id: String, message: String) -> Result<()> {
        let session = Arc::downgrade(self);
        let reply = self
            .connection
            .call_ordered(
                "_session/steer",
                json!({ "sessionId": self.id, "message": message }),
                move |reply| {
                    let Some(session) = session.upgrade() else {
                        return;
                    };
                    let mut state = session.state.lock().unwrap();
                    let queued_id = reply.as_ref().ok().and_then(steering_id);
                    let content = vec![ContentBlock::Text { text: message }];
                    let Some(turn) = state.turn.as_mut().filter(|turn| turn.id == turn_id) else {
                        if queued_id.is_some() && !state.closed {
                            state.undelivered_steering(content);
                        }
                        return;
                    };
                    let early = turn.steering.early.take().unwrap_or_default();
                    if let Some(id) = queued_id {
                        turn.steering.queued.insert(id.to_owned(), content);
                        if let Some(injected) = early.get(id) {
                            state.steering_receipt(id, *injected);
                        }
                    }
                },
            )
            .await
            .map_err(agent_error)?;
        if steering_id(&reply).is_none() {
            return Err(AgentError::new(
                ErrorCode::AgentUnavailable,
                "Kiro did not acknowledge steering input",
            ));
        }
        Ok(())
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

    async fn approve_pending_permissions(&self) {
        let pending: Vec<(String, String)> = {
            let state = self.state.lock().unwrap();
            state
                .interactions
                .iter()
                .filter_map(|(id, pending)| {
                    let ItemKind::Request(request) = &pending.draft.kind else {
                        return None;
                    };
                    let option_id = auto_approve_permission_option(request)?;
                    Some((id.clone(), option_id))
                })
                .collect()
        };
        for (id, option_id) in pending {
            let _ = self
                .respond_pending(&id, RequestResolution::Selected { option_id })
                .await;
        }
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

    fn current_options(&self) -> Option<Vec<ConfigOption>> {
        Some(self.options().options)
    }

    fn can_send_during_turn(&self, content: &[ContentBlock], awaiting_input: bool) -> bool {
        if self.kiro_steering {
            matches!(content, [ContentBlock::Text { .. }])
        } else {
            !awaiting_input
        }
    }

    async fn send(&self, content: Vec<ContentBlock>) -> Result<()> {
        let prompt = mapping::prompt(&content, &self.cwd, self.image)?;
        let _outbound = self.outbound.lock().await;
        let requires_auth =
            *self.connection.auth.lock().unwrap() == verre_proto::AuthState::Required;
        if requires_auth && !self.connection.recover_auth(None).await? {
            return Err(AgentError::new(
                ErrorCode::AuthRequired,
                "Sign in to the agent first",
            ));
        }
        let session = self
            .connection
            .session(&self.id)
            .ok_or_else(|| agent_error(Error::Closed))?;
        let (turn_id, steering) = {
            let mut state = self.state.lock().unwrap();
            state.ensure_open()?;
            let session_running = state.turn.is_some();
            if let Some(turn) = state.turn.as_mut().filter(|_| self.kiro_steering) {
                let [ContentBlock::Text { text }] = content.as_slice() else {
                    return Err(AgentError::new(
                        ErrorCode::Busy,
                        "Kiro steering requires text",
                    ));
                };
                if turn.steering.queued.len() >= 32 {
                    return Err(AgentError::new(
                        ErrorCode::Busy,
                        "Kiro steering queue is full",
                    ));
                }
                turn.steering.early = Some(HashMap::new());
                (turn.id.clone(), Some(text.clone()))
            } else {
                if session_running && !self.steering {
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
                    turn.user_item_id = None;
                } else {
                    state.turn = Some(Turn {
                        id: state.id("turn"),
                        since: Instant::now(),
                        pending: 1,
                        user: Some(content),
                        user_item_id: None,
                        usage: None,
                        steering: Steering::default(),
                    });
                }
                state.flush_user();
                state.status_after_input(false);
                (state.turn.as_ref().unwrap().id.clone(), None)
            }
        };
        if let Some(message) = steering {
            return session.steer(turn_id, message).await;
        }
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
        enum Action {
            SwitchTurboOnAlreadyDefault,
            SwitchTurboOffAlreadyNative,
            Send {
                method: &'static str,
                params: Value,
                wire_value: String,
                next_turbo: Option<bool>,
            },
        }
        let action = {
            let state = self.state.lock().unwrap();
            state.ensure_open()?;
            let synthetic_turbo_mode = state.antigravity
                && state.config.mode_option_id() == Some(id)
                && !state
                    .config
                    .has_native_mode_value(mapping::ANTIGRAVITY_TURBO);
            if synthetic_turbo_mode && value == mapping::ANTIGRAVITY_TURBO {
                if !state
                    .config
                    .has_native_mode_value(mapping::ANTIGRAVITY_DEFAULT_MODE)
                {
                    return Err(AgentError::new(
                        ErrorCode::BadRequest,
                        "Unknown ACP session option",
                    ));
                }
                if state.config.native_mode_value() == Some(mapping::ANTIGRAVITY_DEFAULT_MODE) {
                    Action::SwitchTurboOnAlreadyDefault
                } else {
                    let (method, mut params) = state
                        .config
                        .request(id, mapping::ANTIGRAVITY_DEFAULT_MODE)?;
                    params["sessionId"] = json!(self.id);
                    Action::Send {
                        method,
                        params,
                        wire_value: mapping::ANTIGRAVITY_DEFAULT_MODE.to_owned(),
                        next_turbo: Some(true),
                    }
                }
            } else if synthetic_turbo_mode
                && value != mapping::ANTIGRAVITY_TURBO
                && state.turbo
                && state.config.native_mode_value() == Some(value)
            {
                Action::SwitchTurboOffAlreadyNative
            } else {
                let (method, mut params) = state.config.request(id, value)?;
                params["sessionId"] = json!(self.id);
                Action::Send {
                    method,
                    params,
                    wire_value: value.to_owned(),
                    next_turbo: synthetic_turbo_mode.then_some(false),
                }
            }
        };
        match action {
            Action::SwitchTurboOnAlreadyDefault => {
                {
                    let mut state = self.state.lock().unwrap();
                    state.ensure_open()?;
                    state.turbo = true;
                    state.emit_options();
                }
                self.approve_pending_permissions().await;
            }
            Action::SwitchTurboOffAlreadyNative => {
                let mut state = self.state.lock().unwrap();
                state.ensure_open()?;
                state.turbo = false;
                state.emit_options();
            }
            Action::Send {
                method,
                params,
                wire_value,
                next_turbo,
            } => {
                let session = Arc::downgrade(
                    &self
                        .connection
                        .session(&self.id)
                        .ok_or_else(|| agent_error(Error::Closed))?,
                );
                let id = id.to_owned();
                let reply = self
                    .connection
                    .call_ordered(method, params, move |reply| {
                        if let (Ok(result), Some(session)) = (reply, session.upgrade()) {
                            let mut state = session.state.lock().unwrap();
                            let updated = state.config.update(result)
                                || state.config.set_current(&id, &wire_value);
                            if let Some(turbo) = next_turbo {
                                state.turbo = turbo;
                            }
                            state.sync_turbo();
                            if updated || next_turbo.is_some() {
                                state.emit_options();
                            }
                        }
                    })
                    .await;
                self.connection.observe_auth(method, &reply);
                reply.map_err(agent_error)?;
                if next_turbo == Some(true) {
                    self.approve_pending_permissions().await;
                }
            }
        }
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

    fn background_processes(&self) -> usize {
        self.connection.background_processes()
    }
}

fn steering_id(reply: &Value) -> Option<&str> {
    (reply["queued"].as_bool() == Some(true))
        .then(|| reply["messageId"].as_str())
        .flatten()
}

fn auto_approve_permission_option(request: &InteractionRequest) -> Option<String> {
    if request
        .call_id
        .as_deref()
        .is_some_and(|id| id.starts_with("interaction_"))
    {
        return None;
    }
    let RequestBody::Permission { options } = &request.body else {
        return None;
    };
    options
        .iter()
        .find(|option| option.kind == PermissionOptionKind::AllowOnce)
        .or_else(|| {
            options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::AllowAlways)
        })
        .map(|option| option.id.clone())
}

impl State {
    fn sync_turbo(&mut self) {
        if self.turbo
            && (self
                .config
                .has_native_mode_value(mapping::ANTIGRAVITY_TURBO)
                || self.config.native_mode_value() != Some(mapping::ANTIGRAVITY_DEFAULT_MODE))
        {
            self.turbo = false;
        }
    }

    fn options(&self) -> Vec<ConfigOption> {
        let mut options = self.config.options();
        if self.antigravity {
            mapping::ensure_antigravity_turbo_option(&mut options);
            if self.turbo
                && !self
                    .config
                    .has_native_mode_value(mapping::ANTIGRAVITY_TURBO)
            {
                if let Some(mode) = options
                    .iter_mut()
                    .find(|option| option.category == ConfigCategory::Mode)
                {
                    mode.current_value = mapping::ANTIGRAVITY_TURBO.into();
                }
            }
        }
        options.extend(self.permission_preset.clone());
        options
    }

    fn steering_receipt(&mut self, id: &str, injected: bool) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        if let Some(content) = turn.steering.queued.remove(id) {
            if injected {
                self.end_message();
                let draft = self.draft(format!("message:{id}"), ItemKind::UserMessage { content });
                self.upsert(draft);
            } else {
                self.undelivered_steering(content);
            }
        } else if let Some(early) = &mut turn.steering.early {
            if early.len() < 32 {
                early.entry(id.to_owned()).or_insert(injected);
            }
        }
    }

    fn clear_steering(&mut self) {
        if let Some(turn) = self.turn.as_mut() {
            let queued = std::mem::take(&mut turn.steering.queued);
            for content in queued.into_values() {
                self.undelivered_steering(content);
            }
        }
    }

    fn undelivered_steering(&mut self, content: Vec<ContentBlock>) {
        let id = self.id("notice");
        let draft = self.draft(
            id,
            ItemKind::Notice {
                level: NoticeLevel::Warning,
                text: "Kiro did not confirm delivery of the steering message".into(),
                detail: Some(json!(content)),
            },
        );
        self.upsert(draft);
    }

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
        let options = self.options();
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
            if let Some(turn) = self.turn.as_mut() {
                turn.user_item_id = Some(id.clone());
            }
            let draft = self.draft(id, ItemKind::UserMessage { content });
            self.upsert(draft);
        }
    }

    fn message(&mut self, role: Role, value: &Value, agent: &str) {
        let Some(block) = mapping::content(&value["content"]) else {
            verre_agent_api::drift::report(
                agent,
                "ACP message content",
                value["content"]["type"].as_str(),
            );
            return;
        };
        if role == Role::User {
            if let Some(turn) = &mut self.turn {
                turn.user = None;
            }
        } else {
            self.flush_user();
            if let Some(turn) = &mut self.turn {
                turn.user_item_id = None;
            }
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
            let echoed_id = if role == Role::User && self.replay.is_none() {
                self.turn.as_mut().and_then(|turn| turn.user_item_id.take())
            } else {
                None
            };
            let id = echoed_id.unwrap_or_else(|| {
                native_id
                    .as_ref()
                    .map(|id| format!("message:{id}"))
                    .unwrap_or_else(|| self.id("message"))
            });
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
            draft.ts = draft.ts.or_else(|| mapping::update_timestamp(value));
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

    fn tool(&mut self, value: &Value, announced: bool, agent: &str) {
        let Some(call_id) = string(value, "toolCallId") else {
            verre_agent_api::drift::report(
                agent,
                "ACP tool without id",
                value["sessionUpdate"].as_str(),
            );
            return;
        };
        let id = format!("tool:{call_id}");
        let previous = self.tools.remove(&id);
        if announced || previous.is_none() {
            self.end_message();
        }
        let mut draft = previous.unwrap_or_else(|| {
            self.draft(id.clone(), ItemKind::ToolCall(mapping::new_tool(call_id)))
        });
        draft.ts = draft.ts.or_else(|| mapping::update_timestamp(value));
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

fn replay_turns(items: Vec<ItemDraft>) -> Vec<ItemDraft> {
    let mut replay = Vec::with_capacity(items.len());
    let mut turn = None::<String>;
    let mut answered = false;
    let mut pending = false;
    for mut item in items {
        match &item.kind {
            ItemKind::UserMessage { .. } => {
                if let Some(previous) = turn.filter(|_| answered && !pending) {
                    replay.push(ItemDraft::turn_end(
                        &previous,
                        StopReason::EndTurn,
                        None,
                        None,
                        None,
                    ));
                }
                turn = Some(format!("replay:{}", item.id));
                answered = false;
                pending = false;
            }
            ItemKind::AgentMessage { text, streaming } => {
                answered = !streaming && !text.trim().is_empty();
            }
            ItemKind::Reasoning { .. } => answered = false,
            ItemKind::ToolCall(tool) => {
                answered = false;
                pending |= matches!(
                    tool.status,
                    verre_proto::ToolStatus::Pending | verre_proto::ToolStatus::InProgress
                );
            }
            ItemKind::Request(request) => pending |= request.resolution.is_none(),
            ItemKind::TurnEnd { .. } => answered = false,
            _ => {}
        }
        item.turn_id = item.turn_id.or_else(|| turn.clone());
        replay.push(item);
    }
    replay
}
