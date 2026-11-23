use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use vetro_agent_api::{
    async_trait, jsonrpc::Id, AgentError, EventSink, ItemDraft, Result, Session, SessionEvent,
    WireLog,
};
use vetro_proto::{
    rpc::ErrorCode, ConfigOption, ContentBlock, ItemKind, RateLimit, RequestResolution,
    ThreadStatus, ThreadUsage, ToolContent,
};

use crate::{
    bad_request,
    catalog::{approval_value, Catalog},
    interaction::{self, Pending},
    mapping::{self, array, string},
    transport_error, unavailable, App,
};

pub(crate) struct Live {
    app: Arc<App>,
    id: String,
    sink: EventSink,
    catalog: Catalog,
    state: Mutex<State>,
    operations: AsyncMutex<()>,
    responses: AsyncMutex<()>,
    log: Arc<WireLog>,
}

#[derive(Default)]
struct State {
    closed: bool,
    active: Option<String>,
    last_completed: Option<String>,
    revision: u64,
    settings_revision: u64,
    counter: u64,
    usage: ThreadUsage,
    usage_turn: Option<String>,
    selected: BTreeMap<String, String>,
    items: HashMap<String, ItemDraft>,
    reasoning: HashMap<String, BTreeMap<(bool, u64), String>>,
    pending: HashMap<String, Pending>,
}

impl Live {
    pub(crate) fn register(
        app: Arc<App>,
        id: String,
        sink: EventSink,
        catalog: Catalog,
    ) -> Result<Arc<Self>> {
        let live = Arc::new(Self {
            app: Arc::clone(&app),
            id: id.clone(),
            sink,
            catalog,
            state: Mutex::default(),
            operations: AsyncMutex::default(),
            responses: AsyncMutex::default(),
            log: app.peer.wire_log().session(&id),
        });
        {
            let mut state = app.state.lock().unwrap();
            if state.closed {
                return Err(unavailable("Codex app-server is closed"));
            }
            if state
                .threads
                .get(&id)
                .and_then(std::sync::Weak::upgrade)
                .is_some()
            {
                return Err(AgentError::new(
                    ErrorCode::Busy,
                    "Codex thread is already attached",
                ));
            }
            state.threads.insert(id, Arc::downgrade(&live));
            if !state.limits.is_empty() {
                live.limits(&mapping::rate_limits(&state.limits));
            }
        }
        let app = Arc::downgrade(&app);
        tokio::spawn(async move {
            if let Some(app) = app.upgrade() {
                app.refresh_limits().await;
            }
        });
        Ok(live)
    }

    pub(crate) fn hydrate(
        &self,
        response: &Value,
        revision: u64,
        options: &BTreeMap<String, String>,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        let thread = &response["thread"];
        state.selected.clone_from(options);
        for (id, value) in [
            ("model", &response["model"]),
            ("effort", &response["reasoningEffort"]),
            ("mode", &response["collaborationMode"]["mode"]),
            ("permissions", &response["activePermissionProfile"]["id"]),
        ] {
            if let Some(value) = value.as_str().filter(|_| !options.contains_key(id)) {
                state.selected.insert(id.into(), value.into());
            }
        }
        if let Some(approval) = response
            .get("approvalPolicy")
            .filter(|value| !value.is_null())
        {
            let value = approval
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| approval.to_string());
            state.selected.insert("approvalPolicy".into(), value);
        }
        if let Some(sandbox) = response["sandbox"]["type"]
            .as_str()
            .and_then(|kind| match kind {
                "readOnly" => Some("read-only"),
                "workspaceWrite" => Some("workspace-write"),
                "dangerFullAccess" => Some("danger-full-access"),
                _ => None,
            })
        {
            state.selected.insert("sandbox".into(), sandbox.into());
        }
        if state.revision == revision {
            let turns = response["initialTurnsPage"]["data"]
                .as_array()
                .map_or_else(|| array(&thread["turns"]), Vec::as_slice);
            state.active = turns
                .iter()
                .rev()
                .find(|turn| turn["status"] == "inProgress")
                .and_then(|turn| string(turn, "id"));
            self.emit(SessionEvent::Status(status(&state, &thread["status"])));
        }
        if let Some(title) = string(thread, "name").filter(|title| !title.is_empty()) {
            self.emit(SessionEvent::Title(title));
        }
        self.emit(SessionEvent::Model(state.selected.get("model").cloned()));
        self.emit(SessionEvent::Options(self.catalog.options(&state.selected)));
    }

    pub(crate) async fn start_turn(
        &self,
        input: Value,
        extra: Value,
        mode: Option<&str>,
    ) -> Result<()> {
        let collaboration = {
            let state = self.state.lock().unwrap();
            check_open(&state)?;
            mode.map(|mode| self.catalog.collaboration(mode, &state.selected))
                .transpose()?
        };
        let mut params = extra;
        params["threadId"] = json!(self.id);
        params["input"] = input;
        if let Some(collaboration) = collaboration {
            params["collaborationMode"] = collaboration;
        }
        let live = self.app.live(&self.id).map(|live| Arc::downgrade(&live));
        let response = self
            .app
            .call_ordered("turn/start", params, move |reply| {
                if let (Ok(reply), Some(live)) = (reply, live.and_then(|live| live.upgrade())) {
                    let turn = &reply["turn"];
                    let mut state = live.state.lock().unwrap();
                    if let Some(id) = string(turn, "id") {
                        if !state.closed
                            && state.active.is_none()
                            && state.last_completed.as_deref() != Some(&id)
                            && turn["status"] == "inProgress"
                        {
                            state.active = Some(id);
                            live.emit(SessionEvent::Status(status(&state, &Value::Null)));
                        }
                    }
                }
            })
            .await?;
        string(&response["turn"], "id")
            .ok_or_else(|| unavailable("Codex returned a turn without an id"))?;
        Ok(())
    }

    fn emit(&self, event: SessionEvent) {
        let _ = self.sink.send(event);
    }

    pub(crate) fn limits(&self, limits: &[RateLimit]) {
        let mut state = self.state.lock().unwrap();
        if !state.closed && state.usage.rate_limits != limits {
            state.usage.rate_limits = limits.to_vec();
            self.emit(SessionEvent::Usage(state.usage.clone()));
        }
    }

    pub(crate) fn close(&self, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.closed = true;
            state.active = None;
            for (_, pending) in state.pending.drain() {
                self.emit(SessionEvent::Item(
                    pending.resolved(RequestResolution::Cancelled),
                ));
            }
            state.items.clear();
            state.reasoning.clear();
            self.emit(SessionEvent::Closed { error });
        }
    }

    pub(crate) fn request(&self, id: Id, method: &str, params: &Value) -> bool {
        let Some(mut pending) = Pending::new(id, method, params) else {
            return false;
        };
        let mut state = self.state.lock().unwrap();
        if state.closed || self.sink.is_closed() {
            return false;
        }
        if method == "item/commandExecution/requestApproval" && params["command"].is_null() {
            if let ItemKind::Request(request) = &mut pending.item.kind {
                let item = request
                    .call_id
                    .as_deref()
                    .and_then(|id| state.items.get(id));
                if let Some(ItemDraft {
                    kind: ItemKind::ToolCall(tool),
                    ..
                }) = item
                {
                    if let Some(ToolContent::Terminal {
                        command: Some(command),
                        ..
                    }) = tool.content.first()
                    {
                        request.detail = Some(match request.detail.take() {
                            Some(reason) => format!("{reason}\n\n{command}"),
                            None => command.clone(),
                        });
                    }
                }
            }
        }
        self.emit(SessionEvent::Item(pending.item.clone()));
        state.pending.insert(interaction::key(&pending.id), pending);
        self.emit(SessionEvent::Status(ThreadStatus::AwaitingInput));
        true
    }

    pub(crate) fn notification(&self, method: &str, params: &Value) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        let turn = params["turnId"].as_str();
        match method {
            "turn/started" => {
                state.revision += 1;
                state.active = string(&params["turn"], "id");
                self.emit(SessionEvent::Status(status(&state, &Value::Null)));
            }
            "turn/completed" => {
                let turn = &params["turn"];
                let id = turn["id"].as_str();
                state.last_completed = id.map(str::to_owned);
                let usage = (state.usage_turn.as_deref() == id)
                    .then(|| state.usage.last_turn.clone())
                    .flatten();
                if let Some(end) = mapping::turn_end(turn, usage) {
                    self.emit(SessionEvent::Item(end));
                }
                state.revision += 1;
                if state.active.as_deref() == id {
                    state.active = None;
                }
                state.items.retain(|_, item| item.turn_id.as_deref() != id);
                let State {
                    items, reasoning, ..
                } = &mut *state;
                reasoning.retain(|id, _| items.contains_key(id));
                if !state.pending.is_empty() || state.active.is_some() {
                    self.emit(SessionEvent::Status(status(&state, &Value::Null)));
                } else {
                    self.emit(SessionEvent::Status(if turn["status"] == "failed" {
                        ThreadStatus::Error
                    } else {
                        ThreadStatus::Idle
                    }));
                }
            }
            "thread/status/changed" => {
                self.emit(SessionEvent::Status(status(&state, &params["status"])))
            }
            "thread/settings/updated" => {
                let settings = &params["threadSettings"];
                state.settings_revision += 1;
                for (id, value) in [
                    ("model", &settings["model"]),
                    ("effort", &settings["effort"]),
                    ("mode", &settings["collaborationMode"]["mode"]),
                    ("permissions", &settings["activePermissionProfile"]["id"]),
                ] {
                    if let Some(value) = value.as_str() {
                        state.selected.insert(id.into(), value.into());
                    } else if id == "effort" && settings.get("effort").is_some() {
                        state.selected.remove(id);
                    }
                }
                self.emit(SessionEvent::Model(state.selected.get("model").cloned()));
                self.emit(SessionEvent::Options(self.catalog.options(&state.selected)));
            }
            "thread/name/updated" => {
                if let Some(name) = string(params, "threadName") {
                    self.emit(SessionEvent::Title(name));
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                let window = usage["modelContextWindow"].as_u64();
                state.usage.last_turn = mapping::tokens(&usage["last"], window);
                state.usage.total = mapping::tokens(&usage["total"], window);
                state.usage_turn = turn.map(str::to_owned);
                self.emit(SessionEvent::Usage(state.usage.clone()));
            }
            "item/started" | "item/completed" => {
                self.item_update(&mut state, &params["item"], turn, method == "item/started")
            }
            "item/agentMessage/delta" => self.message_delta(&mut state, params, turn),
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                self.reasoning_delta(
                    &mut state,
                    params,
                    turn,
                    method == "item/reasoning/textDelta",
                );
            }
            "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "item/mcpToolCall/progress" => self.tool_delta(&mut state, params),
            "item/fileChange/patchUpdated" => {
                if let Some(item) = state
                    .items
                    .get_mut(params["itemId"].as_str().unwrap_or_default())
                {
                    if let ItemKind::ToolCall(tool) = &mut item.kind {
                        tool.locations = mapping::locations(&params["changes"]);
                        tool.content = mapping::diffs(&params["changes"]);
                        self.emit(SessionEvent::Item(item.clone()));
                    }
                }
            }
            "item/plan/delta" => {
                let Some(id) = params["itemId"].as_str() else {
                    return;
                };
                let Some(delta) = params["delta"].as_str() else {
                    return;
                };
                let item = state.items.entry(id.into()).or_insert_with(|| {
                    draft(
                        id,
                        turn,
                        ItemKind::Plan {
                            entries: Vec::new(),
                        },
                    )
                });
                if let ItemKind::Plan { entries } = &mut item.kind {
                    if let Some(entry) = entries.first_mut() {
                        entry.content.push_str(delta);
                    } else {
                        *entries = mapping::plan_text(delta);
                    }
                    self.emit(SessionEvent::Item(item.clone()));
                }
            }
            "turn/plan/updated" => {
                if let Some(turn) = turn {
                    self.emit(SessionEvent::Item(draft(
                        &format!("{turn}:plan"),
                        Some(turn),
                        ItemKind::Plan {
                            entries: mapping::plan_steps(&params["plan"]),
                        },
                    )));
                }
            }
            "serverRequest/resolved" => {
                if let Ok(id) = serde_json::from_value::<Id>(params["requestId"].clone()) {
                    let id = interaction::key(&id);
                    if state
                        .pending
                        .get(&id)
                        .is_some_and(|pending| !pending.responding)
                    {
                        let pending = state.pending.remove(&id).unwrap();
                        self.emit(SessionEvent::Item(
                            pending.resolved(RequestResolution::Cancelled),
                        ));
                        self.emit(SessionEvent::Status(status(&state, &Value::Null)));
                    }
                }
            }
            _ => {
                if let Some(kind) = mapping::notification_notice(method, params) {
                    state.counter += 1;
                    self.emit(SessionEvent::Item(draft(
                        &format!("notice:{}", state.counter),
                        turn,
                        kind,
                    )));
                }
            }
        }
    }

    fn item_update(&self, state: &mut State, raw: &Value, turn: Option<&str>, started: bool) {
        let Some(mut item) = mapping::item(raw, turn, started) else {
            if !started && raw["type"] == "agentMessage" {
                if let Some(mut item) = raw["id"].as_str().and_then(|id| state.items.remove(id)) {
                    if let ItemKind::AgentMessage { streaming, .. } = &mut item.kind {
                        *streaming = false;
                        self.emit(SessionEvent::Item(item));
                    }
                }
            }
            return;
        };
        if started {
            if state.items.contains_key(&item.id) {
                return;
            }
            if matches!(item.kind, ItemKind::Reasoning { .. }) {
                let mut parts = BTreeMap::new();
                for (key, full_text) in [("summary", false), ("content", true)] {
                    for (index, part) in array(&raw[key]).iter().enumerate() {
                        if let Some(text) = part.as_str() {
                            parts.insert((full_text, index as u64), text.into());
                        }
                    }
                }
                state.reasoning.insert(item.id.clone(), parts);
            }
            state.items.insert(item.id.clone(), item.clone());
        } else {
            if let Some(previous) = state.items.remove(&item.id) {
                reconcile(&mut item, previous);
            }
            state.reasoning.remove(&item.id);
        }
        self.emit(SessionEvent::Item(item));
    }

    fn message_delta(&self, state: &mut State, params: &Value, turn: Option<&str>) {
        let Some(id) = params["itemId"].as_str() else {
            return;
        };
        let Some(delta) = params["delta"].as_str().filter(|delta| !delta.is_empty()) else {
            return;
        };
        if let Some(item) = state.items.get_mut(id) {
            if let ItemKind::AgentMessage { text, .. } = &mut item.kind {
                text.push_str(delta);
                self.emit(SessionEvent::TextDelta {
                    item_id: id.into(),
                    delta: delta.into(),
                });
            }
        } else {
            let item = draft(
                id,
                turn,
                ItemKind::AgentMessage {
                    text: delta.into(),
                    streaming: true,
                },
            );
            state.items.insert(id.into(), item.clone());
            self.emit(SessionEvent::Item(item));
        }
    }

    fn reasoning_delta(
        &self,
        state: &mut State,
        params: &Value,
        turn: Option<&str>,
        full_text: bool,
    ) {
        let Some(id) = params["itemId"].as_str() else {
            return;
        };
        let Some(delta) = params["delta"].as_str() else {
            return;
        };
        let index = params[if full_text {
            "contentIndex"
        } else {
            "summaryIndex"
        }]
        .as_u64()
        .unwrap_or_default();
        let parts = state.reasoning.entry(id.into()).or_default();
        parts.entry((full_text, index)).or_default().push_str(delta);
        let reasoning = parts
            .values()
            .filter(|part| !part.is_empty())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n");
        let item = self.text_item(state, id, turn, true);
        if let ItemKind::Reasoning { text, .. } = &mut item.kind {
            let suffix = reasoning.strip_prefix(text.as_str()).map(str::to_owned);
            *text = reasoning;
            if let Some(delta) = suffix {
                self.emit(SessionEvent::TextDelta {
                    item_id: id.into(),
                    delta,
                });
            } else {
                self.emit(SessionEvent::Item(item.clone()));
            }
        }
    }

    fn text_item<'a>(
        &self,
        state: &'a mut State,
        id: &str,
        turn: Option<&str>,
        reasoning: bool,
    ) -> &'a mut ItemDraft {
        state.items.entry(id.into()).or_insert_with(|| {
            let kind = if reasoning {
                ItemKind::Reasoning {
                    text: String::new(),
                    streaming: true,
                }
            } else {
                ItemKind::AgentMessage {
                    text: String::new(),
                    streaming: true,
                }
            };
            let item = draft(id, turn, kind);
            self.emit(SessionEvent::Item(item.clone()));
            item
        })
    }

    fn tool_delta(&self, state: &mut State, params: &Value) {
        let id = params["itemId"].as_str().unwrap_or_default();
        let Some(delta) = params["delta"]
            .as_str()
            .or_else(|| params["message"].as_str())
        else {
            return;
        };
        let Some(item) = state.items.get_mut(id) else {
            return;
        };
        if let ItemKind::ToolCall(tool) = &mut item.kind {
            if let Some(ToolContent::Terminal { output, .. }) = tool.content.first_mut() {
                output.push_str(delta);
            } else if let Some(ToolContent::Text { text }) = tool.content.last_mut() {
                text.push_str(delta);
            } else {
                tool.content.push(ToolContent::Text { text: delta.into() });
            }
            self.emit(SessionEvent::Item(item.clone()));
        }
    }
}

#[async_trait]
impl Session for Live {
    fn native_id(&self) -> &str {
        &self.id
    }

    async fn send(&self, content: Vec<ContentBlock>) -> Result<()> {
        let input = mapping::input(content)?;
        let _operation = self.operations.lock().await;
        let active = {
            let state = self.state.lock().unwrap();
            check_open(&state)?;
            state.active.clone()
        };
        if let Some(turn) = active {
            self.app
                .call(
                    "turn/steer",
                    json!({
                        "threadId": self.id, "expectedTurnId": turn, "input": input,
                    }),
                )
                .await?;
            Ok(())
        } else {
            self.start_turn(input, json!({}), None).await
        }
    }

    async fn interrupt(&self) -> Result<()> {
        let _operation = self.operations.lock().await;
        let active = {
            let state = self.state.lock().unwrap();
            check_open(&state)?;
            state.active.clone()
        };
        if let Some(turn) = active {
            self.app
                .call(
                    "turn/interrupt",
                    json!({"threadId": self.id, "turnId": turn}),
                )
                .await?;
        }
        Ok(())
    }

    async fn set_option(&self, option_id: &str, value: &str) -> Result<Vec<ConfigOption>> {
        let _operation = self.operations.lock().await;
        let (mut selected, revision) = {
            let state = self.state.lock().unwrap();
            check_open(&state)?;
            (state.selected.clone(), state.settings_revision)
        };
        self.catalog.validate(option_id, value, &selected)?;
        let mut params = json!({"threadId": self.id});
        match option_id {
            "mode" => params["collaborationMode"] = self.catalog.collaboration(value, &selected)?,
            "approvalPolicy" => params[option_id] = approval_value(value),
            "sandbox" => {
                selected.remove("permissions");
                params["sandboxPolicy"] = match value {
                    "read-only" => json!({"type": "readOnly", "networkAccess": false}),
                    "workspace-write" => json!({
                        "type": "workspaceWrite", "networkAccess": false, "writableRoots": [],
                        "excludeTmpdirEnvVar": false, "excludeSlashTmp": false,
                    }),
                    "danger-full-access" => json!({"type": "dangerFullAccess"}),
                    _ => return Err(bad_request("Unsupported Codex sandbox")),
                };
            }
            "permissions" => {
                selected.remove("sandbox");
                params[option_id] = json!(value);
            }
            _ => params[option_id] = json!(value),
        }
        self.app.call("thread/settings/update", params).await?;
        selected.insert(option_id.into(), value.into());
        if option_id == "model" {
            let supported = self.catalog.options(&selected).into_iter().any(|option| {
                option.id == "effort"
                    && option
                        .choices
                        .iter()
                        .any(|choice| choice.value == option.current_value)
            });
            if !supported {
                selected.remove("effort");
            }
        }
        let mut state = self.state.lock().unwrap();
        check_open(&state)?;
        if state.settings_revision != revision {
            return Ok(self.catalog.options(&state.selected));
        }
        let options = self.catalog.options(&selected);
        state.selected = selected;
        if option_id == "model" {
            self.emit(SessionEvent::Model(Some(value.into())));
        }
        self.emit(SessionEvent::Options(options.clone()));
        Ok(options)
    }

    async fn respond(&self, request_id: &str, resolution: RequestResolution) -> Result<()> {
        let _response = self.responses.lock().await;
        let (id, payload) = {
            let mut state = self.state.lock().unwrap();
            check_open(&state)?;
            let pending = state.pending.get_mut(request_id).ok_or_else(|| {
                AgentError::new(ErrorCode::NotFound, "Codex request is no longer pending")
            })?;
            let payload = pending.payload(&resolution)?;
            pending.responding = true;
            (pending.id.clone(), payload)
        };
        let reply = self.app.peer.respond(id, Ok(payload)).await;
        let mut state = self.state.lock().unwrap();
        if let Err(error) = reply {
            if let Some(pending) = state.pending.get_mut(request_id) {
                pending.responding = false;
            }
            return Err(transport_error(error));
        }
        if let Some(pending) = state.pending.remove(request_id) {
            self.emit(SessionEvent::Item(pending.resolved(resolution)));
            self.emit(SessionEvent::Status(status(&state, &Value::Null)));
        }
        Ok(())
    }

    async fn detach(&self) {
        let _operation = self.operations.lock().await;
        let _response = self.responses.lock().await;
        if self.state.lock().unwrap().closed {
            return;
        }
        let _ = self
            .app
            .call("thread/unsubscribe", json!({"threadId": self.id}))
            .await;
        let mut app = self.app.state.lock().unwrap();
        app.threads.remove(&self.id);
        self.close(None);
    }

    fn wire_log(&self) -> Arc<WireLog> {
        self.log.clone()
    }
}

fn status(state: &State, native: &Value) -> ThreadStatus {
    if !state.pending.is_empty() || !array(&native["activeFlags"]).is_empty() {
        ThreadStatus::AwaitingInput
    } else if native["type"] == "systemError" {
        ThreadStatus::Error
    } else if state.active.is_some() || native["type"] == "active" {
        ThreadStatus::Running
    } else {
        ThreadStatus::Idle
    }
}

fn check_open(state: &State) -> Result<()> {
    if state.closed {
        Err(unavailable("Codex session is closed"))
    } else {
        Ok(())
    }
}

fn draft(id: &str, turn: Option<&str>, kind: ItemKind) -> ItemDraft {
    ItemDraft {
        id: id.into(),
        ts: None,
        turn_id: turn.map(str::to_owned),
        kind,
    }
}

fn reconcile(item: &mut ItemDraft, previous: ItemDraft) {
    item.ts = item.ts.or(previous.ts);
    item.turn_id = item.turn_id.take().or(previous.turn_id);
    match (&mut item.kind, previous.kind) {
        (ItemKind::AgentMessage { text, .. }, ItemKind::AgentMessage { text: previous, .. })
        | (ItemKind::Reasoning { text, .. }, ItemKind::Reasoning { text: previous, .. })
            if text.is_empty() =>
        {
            *text = previous;
        }
        (ItemKind::Plan { entries }, ItemKind::Plan { entries: previous })
            if entries.is_empty() =>
        {
            *entries = previous;
        }
        (ItemKind::ToolCall(tool), ItemKind::ToolCall(previous)) => {
            if let (
                Some(ToolContent::Terminal { output, .. }),
                Some(ToolContent::Terminal {
                    output: previous, ..
                }),
            ) = (tool.content.first_mut(), previous.content.first())
            {
                if output.is_empty() {
                    output.clone_from(previous);
                }
            } else {
                if tool.content.is_empty() {
                    tool.content = previous.content;
                }
                if tool.locations.is_empty() {
                    tool.locations = previous.locations;
                }
            }
        }
        _ => {}
    }
}
