use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use vetro_agent_api::{
    async_trait,
    process::{Process, SpawnSpec, Stdout},
    AgentError, AgentOptions, EventSink, ItemDraft, Result, Session, SessionEvent, WireLog,
};
use vetro_proto::{
    rpc::ErrorCode, ConfigCategory, ConfigChoice, ConfigOption, ContentBlock, InteractionRequest,
    ItemKind, NoticeLevel, PermissionOption, PermissionOptionKind, Question, QuestionChoice,
    RateLimit, RequestBody, RequestResolution, SlashCommand, StopReason, ThreadStatus, ThreadUsage,
    TokenUsage,
};

use crate::{bad_request, events::Timeline, io_error, string, uuid};

pub(crate) struct ClaudeSession {
    id: String,
    process: Process,
    sink: EventSink,
    state: Mutex<State>,
    writes: AsyncMutex<()>,
}

#[derive(Default)]
struct State {
    timeline: Timeline,
    prompts: VecDeque<String>,
    interrupted: bool,
    closed: bool,
    controls: HashMap<String, oneshot::Sender<Result<Value>>>,
    permissions: HashMap<String, Permission>,
    initialize: Value,
    model: String,
    mode: String,
    effort: String,
    commands: Vec<SlashCommand>,
    usage: ThreadUsage,
}

struct Permission {
    request: InteractionRequest,
    input: Value,
    suggestions: Vec<Value>,
    draft: ItemDraft,
}

impl ClaudeSession {
    pub fn launch(
        mut spec: SpawnSpec,
        id: String,
        cwd: Option<PathBuf>,
        resume: bool,
        choices: BTreeMap<String, String>,
        sink: EventSink,
    ) -> Result<Arc<Self>> {
        spec.cwd = cwd.or(spec.cwd);
        let extra_args = std::mem::take(&mut spec.args);
        spec.args.extend(
            [
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-prompts",
                "host",
                "--permission-prompt-tool=stdio",
                "--include-hook-events",
                "--replay-user-messages",
                "--forward-subagent-text",
                if resume { "--resume" } else { "--session-id" },
                &id,
            ]
            .into_iter()
            .map(Into::into),
        );
        for (key, value) in &choices {
            let flag = match key.as_str() {
                "model" => "--model",
                "mode" => "--permission-mode",
                "effort" => "--effort",
                _ => return Err(bad_request(format!("Unknown Claude option: {key}"))),
            };
            if value.is_empty() || value.starts_with('-') {
                return Err(bad_request(format!(
                    "Invalid Claude option value for {key}"
                )));
            }
            spec.args.push(flag.into());
            spec.args.push(value.into());
        }
        spec.args.extend(extra_args);
        let (process, stdout) = Process::spawn(spec, Arc::new(WireLog::default()))
            .map_err(|error| AgentError::new(ErrorCode::AgentUnavailable, error.to_string()))?;
        let state = State {
            model: choices
                .get("model")
                .cloned()
                .unwrap_or_else(|| "default".into()),
            mode: choices.get("mode").cloned().unwrap_or_default(),
            effort: choices.get("effort").cloned().unwrap_or_default(),
            ..State::default()
        };
        let session = Arc::new(Self {
            id,
            process,
            sink,
            state: Mutex::new(state),
            writes: AsyncMutex::new(()),
        });
        tokio::spawn(Self::receive(Arc::downgrade(&session), stdout));
        Ok(session)
    }

    pub async fn initialize(&self) -> Result<()> {
        let reply = self
            .control(json!({"subtype":"initialize"}), Duration::from_secs(60))
            .await?;
        let mut state = self.state.lock().unwrap();
        if let Some(mode) = string(&reply, "current_permission_mode") {
            state.mode = mode.into();
        }
        state.commands = slash_commands(&reply["commands"]);
        state.initialize = reply;
        self.emit(SessionEvent::Options(state.options()));
        self.emit(SessionEvent::Commands(state.commands.clone()));
        Ok(())
    }

    pub fn metadata(&self, selected: &BTreeMap<String, String>) -> (Value, AgentOptions) {
        let state = self.state.lock().unwrap();
        (
            json!({
                "claude_code_version": state.initialize["claude_code_version"],
                "account": {
                    "subscriptionType": state.initialize["account"]["subscriptionType"],
                    "apiProvider": state.initialize["account"]["apiProvider"],
                },
            }),
            AgentOptions {
                options: state.options_for(selected),
                commands: state.commands.clone(),
            },
        )
    }

    async fn write(&self, value: Value) -> Result<()> {
        self.process
            .send_line(value.to_string())
            .await
            .map_err(io_error)
    }

    async fn control(&self, request: Value, deadline: Duration) -> Result<Value> {
        let id = uuid()?;
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(AgentError::new(
                    ErrorCode::AgentUnavailable,
                    "Claude process closed",
                ));
            }
            state.controls.insert(id.clone(), sender);
        }
        let pending = ControlWait { session: self, id };
        let operation = async {
            self.write(json!({"type":"control_request","request_id":pending.id,"request":request}))
                .await?;
            receiver.await.map_err(|_| {
                AgentError::new(ErrorCode::AgentUnavailable, "Claude process closed")
            })?
        };
        tokio::time::timeout(deadline, operation)
            .await
            .map_err(|_| {
                AgentError::new(
                    ErrorCode::AgentUnavailable,
                    format!("Claude did not answer {}", request["subtype"]),
                )
            })?
    }

    async fn receive(weak: Weak<Self>, mut stdout: Stdout) {
        let mut failure = None;
        while let Some(line) = stdout.recv().await {
            let Some(session) = weak.upgrade() else {
                return;
            };
            match line {
                Ok(line) => {
                    if let Ok(row) = serde_json::from_str(&line) {
                        session.accept(&row).await;
                    }
                }
                Err(error) => {
                    failure = Some(error.to_string());
                    break;
                }
            }
        }
        if let Some(session) = weak.upgrade() {
            if failure.is_none() {
                failure = match session.process.stop().await {
                    Ok(status) if status.success() => None,
                    Ok(status) => Some(format!("Claude Code exited with {status}")),
                    Err(error) => Some(error.to_string()),
                };
            }
            let mut state = session.state.lock().unwrap();
            state.closed = true;
            state.controls.clear();
            let mut events = Vec::new();
            state.timeline.finish_streams(&mut events);
            state.cancel_permissions(&mut events);
            for event in events {
                session.emit(event);
            }
            session.emit(SessionEvent::Closed { error: failure });
        }
    }

    async fn accept(&self, row: &Value) {
        let mut events = Vec::new();
        let mut unsupported = None;
        {
            let mut state = self.state.lock().unwrap();
            match string(row, "type") {
                Some("control_response") => {
                    let response = &row["response"];
                    if let Some(sender) =
                        string(response, "request_id").and_then(|id| state.controls.remove(id))
                    {
                        let result = if response["subtype"] == "success" {
                            Ok(response["response"].clone())
                        } else {
                            Err(control_error(response))
                        };
                        let _ = sender.send(result);
                    }
                }
                Some("control_request") => {
                    let request = &row["request"];
                    if let Some(id) = string(row, "request_id") {
                        if request["subtype"] == "can_use_tool" {
                            state.permission(id, request, &mut events);
                        } else {
                            unsupported = Some(id);
                        }
                    }
                }
                Some("control_cancel_request") => {
                    if let Some(permission) =
                        string(row, "request_id").and_then(|id| state.permissions.remove(id))
                    {
                        events.push(resolved(permission, RequestResolution::Cancelled));
                        events.push(SessionEvent::Status(state.status()));
                    }
                }
                Some("assistant" | "user") => {
                    state.match_prompt(row);
                    state
                        .timeline
                        .message(row, string(row, "parent_tool_use_id"), &mut events);
                }
                Some("stream_event") => {
                    state.match_prompt(row);
                    state.timeline.stream(row, &mut events);
                }
                Some("result") => state.result(row, &mut events),
                Some("system") => match string(row, "subtype") {
                    Some("init") => {
                        if let Some(model) = string(row, "model") {
                            if state.model == "default" {
                                state.model = model.into();
                            }
                            events.push(SessionEvent::Model(Some(model.into())));
                        }
                        if let Some(mode) = string(row, "permissionMode") {
                            state.mode = mode.into();
                        }
                        if row["slash_commands"].is_array() {
                            let commands = slash_commands(&row["slash_commands"])
                                .into_iter()
                                .map(|command| {
                                    state
                                        .commands
                                        .iter()
                                        .find(|known| known.name == command.name)
                                        .cloned()
                                        .unwrap_or(command)
                                })
                                .collect::<Vec<_>>();
                            if commands != state.commands {
                                state.commands = commands;
                                events.push(SessionEvent::Commands(state.commands.clone()));
                            }
                        }
                        events.push(SessionEvent::Options(state.options()));
                    }
                    Some("commands_changed") => {
                        state.commands = slash_commands(&row["commands"]);
                        events.push(SessionEvent::Commands(state.commands.clone()));
                    }
                    _ => state
                        .timeline
                        .notice(row, string(row, "parent_tool_use_id"), &mut events),
                },
                Some("rate_limit_event") => state.rate_limit(row, &mut events),
                Some("auth_status") if row["error"].is_string() => {
                    events.push(SessionEvent::Status(ThreadStatus::Error));
                    let ids: Vec<_> = state.controls.drain().map(|(_, sender)| sender).collect();
                    for reply in ids {
                        let _ = reply.send(Err(AgentError::new(
                            ErrorCode::AuthRequired,
                            "Claude Code requires authentication",
                        )));
                    }
                }
                _ => {}
            }
            for event in events {
                self.emit(event);
            }
        }
        if let Some(id) = unsupported {
            let _ = self
                .write(json!({"type":"control_response","response":{
                    "subtype":"error","request_id":id,"error":"Unsupported host control request"
                }}))
                .await;
        }
    }

    fn emit(&self, event: SessionEvent) {
        let _ = self.sink.send(event);
    }
}

struct ControlWait<'session> {
    session: &'session ClaudeSession,
    id: String,
}

impl Drop for ControlWait<'_> {
    fn drop(&mut self) {
        self.session.state.lock().unwrap().controls.remove(&self.id);
    }
}

#[async_trait]
impl Session for ClaudeSession {
    fn native_id(&self) -> &str {
        &self.id
    }

    async fn send(&self, content: Vec<ContentBlock>) -> Result<()> {
        let blocks = prompt_blocks(&content)?;
        let id = uuid()?;
        let _writes = self.writes.lock().await;
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(AgentError::new(
                    ErrorCode::AgentUnavailable,
                    "Claude process closed",
                ));
            }
            let active = state.timeline.turn.clone();
            state.prompts.push_back(id.clone());
            let mut events = Vec::new();
            state.timeline.user(&id, content, &mut events);
            state.timeline.turn = state.prompts.front().cloned().or(active);
            for event in events {
                self.emit(event);
            }
            self.emit(SessionEvent::Status(state.status()));
        }
        let sent = self
            .write(json!({
                "type":"user","uuid":id,"session_id":self.id,"parent_tool_use_id":null,
                "message":{"role":"user","content":blocks}
            }))
            .await;
        if sent.is_err() {
            let mut state = self.state.lock().unwrap();
            state.prompts.retain(|prompt| *prompt != id);
            state.timeline.turn = state.prompts.front().cloned();
            self.emit(SessionEvent::Status(ThreadStatus::Error));
        }
        sent
    }

    async fn interrupt(&self) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap();
            state.interrupted = !state.prompts.is_empty();
        }
        if let Err(error) = self
            .control(json!({"subtype":"interrupt"}), Duration::from_secs(15))
            .await
        {
            self.state.lock().unwrap().interrupted = false;
            return Err(error);
        }
        Ok(())
    }

    async fn set_option(&self, option_id: &str, value: &str) -> Result<Vec<ConfigOption>> {
        if value.is_empty() {
            return Err(bad_request("Claude option value cannot be empty"));
        }
        let request = match option_id {
            "model" => json!({"subtype":"set_model","model":value}),
            "mode" => json!({"subtype":"set_permission_mode","mode":value}),
            "effort" => return Err(bad_request("Claude Code supports --effort at launch")),
            _ => return Err(bad_request(format!("Unknown Claude option: {option_id}"))),
        };
        let _writes = self.writes.lock().await;
        self.control(request, Duration::from_secs(15)).await?;
        let mut state = self.state.lock().unwrap();
        if option_id == "model" {
            value.clone_into(&mut state.model);
            self.emit(SessionEvent::Model(Some(value.to_owned())));
        } else {
            value.clone_into(&mut state.mode);
        }
        let options = state.options();
        self.emit(SessionEvent::Options(options.clone()));
        Ok(options)
    }

    async fn respond(&self, request_id: &str, resolution: RequestResolution) -> Result<()> {
        let _writes = self.writes.lock().await;
        let (permission, answer) = {
            let mut state = self.state.lock().unwrap();
            let permission = state.permissions.get(request_id).ok_or_else(|| {
                AgentError::new(
                    ErrorCode::NotFound,
                    "Claude permission request is no longer pending",
                )
            })?;
            let answer = permission.answer(&resolution)?;
            (state.permissions.remove(request_id).unwrap(), answer)
        };
        let sent = self
            .write(json!({"type":"control_response","response":{
                "subtype":"success","request_id":request_id,"response":answer
            }}))
            .await;
        let mut state = self.state.lock().unwrap();
        if sent.is_ok() {
            self.emit(resolved(permission, resolution));
            self.emit(SessionEvent::Status(state.status()));
        } else if !state.closed && !state.prompts.is_empty() {
            state.permissions.insert(request_id.into(), permission);
        }
        sent
    }

    async fn detach(&self) {
        let _ = self.process.stop().await;
    }

    fn wire_log(&self) -> Arc<WireLog> {
        self.process.wire_log()
    }
}

impl State {
    fn status(&self) -> ThreadStatus {
        if !self.permissions.is_empty() {
            ThreadStatus::AwaitingInput
        } else if !self.prompts.is_empty() {
            ThreadStatus::Running
        } else {
            ThreadStatus::Idle
        }
    }

    fn match_prompt(&mut self, row: &Value) {
        if !row["parent_tool_use_id"].is_null() {
            return;
        }
        let id = if row["type"] == "user" && row["isReplay"] == true {
            string(row, "uuid")
        } else {
            string(row, "user_message_uuid")
        };
        if let Some(index) = id.and_then(|id| self.prompts.iter().position(|prompt| prompt == id)) {
            self.prompts.drain(1..index + 1);
        }
    }

    fn options(&self) -> Vec<ConfigOption> {
        self.options_for(&BTreeMap::new())
    }

    fn options_for(&self, selected: &BTreeMap<String, String>) -> Vec<ConfigOption> {
        let current_model = selected.get("model").unwrap_or(&self.model);
        let mode = selected.get("mode").unwrap_or(&self.mode);
        let mut options = Vec::new();
        let models = self.initialize["models"].as_array();
        if let Some(models) = models {
            let choices = models
                .iter()
                .filter_map(|model| {
                    let value = string(model, "value")?;
                    Some(ConfigChoice {
                        value: value.into(),
                        name: string(model, "displayName").unwrap_or(value).into(),
                        description: string(model, "description").map(str::to_owned),
                        group: None,
                        danger: false,
                    })
                })
                .collect();
            options.push(option(
                "model",
                "Model",
                ConfigCategory::Model,
                current_model,
                choices,
            ));
            let model = models
                .iter()
                .find(|model| model["value"] == *current_model)
                .or_else(|| {
                    models
                        .iter()
                        .find(|model| model["resolvedModel"] == *current_model)
                });
            let levels = model.and_then(|model| model["supportedEffortLevels"].as_array());
            if let Some(levels) = levels.filter(|levels| !levels.is_empty()) {
                let current = selected.get("effort").unwrap_or(&self.effort);
                let default = model.and_then(|model| model["defaultEffortLevel"].as_str());
                let effort_value = levels
                    .iter()
                    .find(|level| level.as_str() == Some(current))
                    .and_then(Value::as_str)
                    .or(default)
                    .unwrap_or("default");
                let mut choices: Vec<_> = levels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(choice)
                    .collect();
                if !choices.iter().any(|choice| choice.value == effort_value) {
                    choices.insert(0, choice(effort_value));
                }
                let mut effort = option(
                    "effort",
                    "Effort",
                    ConfigCategory::ThoughtLevel,
                    effort_value,
                    choices,
                );
                effort.description = Some("Applies when starting a session".into());
                options.push(effort);
            }
        }
        if !mode.is_empty() {
            let modes = [
                "default",
                "manual",
                "acceptEdits",
                "plan",
                "dontAsk",
                "auto",
                "bypassPermissions",
            ];
            let choices = modes
                .into_iter()
                .map(|value| ConfigChoice {
                    danger: value == "bypassPermissions",
                    ..choice(value)
                })
                .collect();
            options.push(option(
                "mode",
                "Permission mode",
                ConfigCategory::Mode,
                mode,
                choices,
            ));
        }
        options
    }

    fn permission(&mut self, id: &str, source: &Value, events: &mut Vec<SessionEvent>) {
        let tool = string(source, "tool_name").unwrap_or("Tool");
        let input = source.get("input").cloned().unwrap_or_else(|| json!({}));
        let suggestions = source["permission_suggestions"]
            .as_array()
            .filter(|suggestions| {
                !suggestions.is_empty() && source["suppress_always_allow_rule"] != true
            })
            .cloned()
            .unwrap_or_default();
        let is_question = tool == "AskUserQuestion";
        let is_plan = matches!(tool, "ExitPlanMode" | "EnterPlanMode");
        let title = match tool {
            "AskUserQuestion" => "Claude needs an answer".to_owned(),
            "ExitPlanMode" => "Approve Claude's plan".to_owned(),
            "EnterPlanMode" => "Allow Claude to enter plan mode".to_owned(),
            _ => format!("Allow {tool}?"),
        };
        let detail = if is_question {
            string(source, "decision_reason").map(str::to_owned)
        } else {
            let payload = ["plan", "command", "file_path"]
                .into_iter()
                .find_map(|key| string(&input, key))
                .map(str::to_owned)
                .unwrap_or_else(|| input.to_string());
            Some(
                match string(source, "decision_reason").or_else(|| string(source, "blocked_path")) {
                    Some(reason) => format!("{reason}\n\n{payload}"),
                    None => payload,
                },
            )
        };
        let body = if is_question {
            let questions = input["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|question| {
                    let text = string(question, "question")?;
                    Some(Question {
                        id: text.into(),
                        header: string(question, "header").map(str::to_owned),
                        text: text.into(),
                        choices: question["options"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|option| {
                                Some(QuestionChoice {
                                    label: string(option, "label")?.into(),
                                    description: string(option, "description").map(str::to_owned),
                                })
                            })
                            .collect(),
                        multi_select: question["multiSelect"] == true,
                        allow_free_text: true,
                    })
                })
                .collect();
            RequestBody::Questions { questions }
        } else {
            let mut options = vec![permission_option(
                "allow",
                if is_plan { "Approve" } else { "Allow once" },
                PermissionOptionKind::AllowOnce,
            )];
            if !suggestions.is_empty() {
                options.push(permission_option(
                    "always",
                    "Always allow (Claude's suggested rules)",
                    PermissionOptionKind::AllowAlways,
                ));
            }
            options.push(permission_option(
                "deny",
                "Deny",
                PermissionOptionKind::RejectOnce,
            ));
            RequestBody::Permission { options }
        };
        let request = InteractionRequest {
            request_id: id.into(),
            call_id: string(source, "tool_use_id").map(str::to_owned),
            title,
            detail,
            body,
            resolution: None,
        };
        let draft = self.timeline.draft(
            format!("request:{id}"),
            None,
            ItemKind::Request(request.clone()),
        );
        self.permissions.insert(
            id.into(),
            Permission {
                request,
                input,
                suggestions,
                draft: draft.clone(),
            },
        );
        events.push(SessionEvent::Item(draft));
        events.push(SessionEvent::Status(ThreadStatus::AwaitingInput));
    }

    fn cancel_permissions(&mut self, events: &mut Vec<SessionEvent>) {
        for (_, permission) in self.permissions.drain() {
            events.push(resolved(permission, RequestResolution::Cancelled));
        }
    }

    fn result(&mut self, row: &Value, events: &mut Vec<SessionEvent>) {
        self.timeline.finish_streams(events);
        self.cancel_permissions(events);
        let error = row["is_error"] == true
            || string(row, "subtype").is_some_and(|kind| kind.starts_with("error"));
        let cancelled = std::mem::take(&mut self.interrupted) || row["stop_reason"] == "cancelled";
        let reason = if cancelled {
            StopReason::Cancelled
        } else {
            match string(row, "stop_reason") {
                Some("max_tokens" | "model_context_window_exceeded") => StopReason::MaxTokens,
                Some("refusal") => StopReason::Refusal,
                _ if error => StopReason::Error,
                _ => StopReason::EndTurn,
            }
        };
        let usage_this_turn = row["usage"].as_object().map(|_| token_usage(&row["usage"]));
        let total = row["modelUsage"]
            .as_object()
            .filter(|models| !models.is_empty())
            .map(|models| {
                models.values().fold(TokenUsage::default(), |total, model| {
                    let cached = model["cacheReadInputTokens"].as_u64();
                    let created = model["cacheCreationInputTokens"].as_u64();
                    accumulate_usage(
                        &total,
                        &TokenUsage {
                            input: add_tokens(
                                add_tokens(model["inputTokens"].as_u64(), cached),
                                created,
                            ),
                            cached_input: cached,
                            output: model["outputTokens"].as_u64(),
                            reasoning: model["thinkingTokens"].as_u64(),
                            ..TokenUsage::default()
                        },
                    )
                })
            });
        if total.is_some() || usage_this_turn.is_some() {
            self.usage.total = total.or_else(|| {
                let turn = usage_this_turn.as_ref()?;
                Some(accumulate_usage(
                    self.usage.total.as_ref().unwrap_or(&TokenUsage::default()),
                    turn,
                ))
            });
            if usage_this_turn.is_some() {
                self.usage.last_turn.clone_from(&usage_this_turn);
            }
            events.push(SessionEvent::Usage(self.usage.clone()));
        }
        let id = string(row, "uuid")
            .or(self.timeline.turn.as_deref())
            .unwrap_or("claude");
        if error && !cancelled {
            let errors = row["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let text = if errors.is_empty() {
                string(row, "result")
                    .unwrap_or("Claude Code turn failed")
                    .into()
            } else {
                errors.join("\n")
            };
            events.push(SessionEvent::Item(self.timeline.draft(
                format!("error:{id}"),
                None,
                ItemKind::Notice {
                    level: NoticeLevel::Error,
                    text,
                    detail: None,
                },
            )));
        }
        events.push(SessionEvent::Item(self.timeline.draft(
            format!("turn:{id}"),
            None,
            ItemKind::TurnEnd {
                stop_reason: reason,
                usage: usage_this_turn,
                duration_ms: row["duration_ms"].as_u64(),
            },
        )));
        self.prompts.pop_front();
        self.timeline.turn = self.prompts.front().cloned();
        events.push(SessionEvent::Status(
            if error && !cancelled && self.prompts.is_empty() {
                ThreadStatus::Error
            } else {
                self.status()
            },
        ));
    }

    fn rate_limit(&mut self, row: &Value, events: &mut Vec<SessionEvent>) {
        let rate = &row["rate_limit_info"];
        let Some(label) = string(rate, "rateLimitType") else {
            return;
        };
        let Some(used_percent) = rate["utilization"]
            .as_f64()
            .map(|fraction| fraction * 100.0)
            .or_else(|| (rate["status"] == "rejected").then_some(100.0))
        else {
            return;
        };
        self.usage.rate_limits.retain(|entry| entry.label != label);
        self.usage.rate_limits.push(RateLimit {
            label: label.into(),
            used_percent,
            resets_at: rate["resetsAt"]
                .as_i64()
                .map(|seconds| seconds.saturating_mul(1000)),
        });
        events.push(SessionEvent::Usage(self.usage.clone()));
    }
}

impl Permission {
    fn answer(&self, resolution: &RequestResolution) -> Result<Value> {
        let tool_id = &self.request.call_id;
        if matches!(resolution, RequestResolution::Cancelled) {
            return Ok(
                json!({"behavior":"deny","message":"Cancelled by user","toolUseID":tool_id}),
            );
        }
        match (&self.request.body, resolution) {
            (RequestBody::Permission { options }, RequestResolution::Selected { option_id }) => {
                let selected = options
                    .iter()
                    .find(|option| &option.id == option_id)
                    .ok_or_else(|| bad_request("Unknown Claude permission option"))?;
                match selected.kind {
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways => {
                        let mut response = json!({"behavior":"allow","updatedInput":self.input,"toolUseID":tool_id});
                        if selected.kind == PermissionOptionKind::AllowAlways {
                            response["updatedPermissions"] = json!(self.suggestions);
                        }
                        Ok(response)
                    }
                    _ => Ok(
                        json!({"behavior":"deny","message":"Denied by user","toolUseID":tool_id}),
                    ),
                }
            }
            (RequestBody::Questions { questions }, RequestResolution::Answered { answers }) => {
                let mut input = self.input.as_object().cloned().unwrap_or_default();
                let mut output = serde_json::Map::new();
                for question in questions {
                    let values = answers
                        .get(&question.id)
                        .filter(|values| {
                            !values.is_empty() && (question.multi_select || values.len() == 1)
                        })
                        .ok_or_else(|| {
                            bad_request(format!("Missing or invalid answer for {}", question.text))
                        })?;
                    output.insert(question.text.clone(), json!(values.join(", ")));
                }
                input.insert("answers".into(), Value::Object(output));
                Ok(json!({"behavior":"allow","updatedInput":input,"toolUseID":tool_id}))
            }
            _ => Err(bad_request(
                "Resolution does not match Claude permission request",
            )),
        }
    }
}

fn resolved(mut permission: Permission, resolution: RequestResolution) -> SessionEvent {
    permission.request.resolution = Some(resolution);
    permission.draft.kind = ItemKind::Request(permission.request);
    SessionEvent::Item(permission.draft)
}

fn permission_option(id: &str, label: &str, kind: PermissionOptionKind) -> PermissionOption {
    PermissionOption {
        id: id.into(),
        label: label.into(),
        kind,
    }
}

fn control_error(response: &Value) -> AgentError {
    let text = string(response, "error").unwrap_or("Claude rejected the control request");
    let lower = text.to_ascii_lowercase();
    let code = if lower.contains("not logged in") || lower.contains("authentication required") {
        ErrorCode::AuthRequired
    } else {
        ErrorCode::BadRequest
    };
    AgentError::new(code, text)
}

fn prompt_blocks(content: &[ContentBlock]) -> Result<Vec<Value>> {
    if content.is_empty() {
        return Err(bad_request("Claude prompt has no content"));
    }
    content
        .iter()
        .filter(|block| !matches!(block, ContentBlock::File { .. }))
        .map(|block| match block {
            ContentBlock::Text { text } => Ok(json!({"type":"text","text":text})),
            ContentBlock::Image {
                mime_type,
                data: Some(data),
                ..
            } => Ok(json!({
                "type":"image","source":{"type":"base64","media_type":mime_type,"data":data}
            })),
            ContentBlock::Image { uri: Some(uri), .. }
                if uri.starts_with("https://") || uri.starts_with("http://") =>
            {
                Ok(json!({
                    "type":"image","source":{"type":"url","url":uri}
                }))
            }
            ContentBlock::Image { .. } => Err(bad_request(
                "Claude images require base64 data or an HTTP URL",
            )),
            ContentBlock::File { .. } => unreachable!(),
        })
        .collect()
}

fn slash_commands(value: &Value) -> Vec<SlashCommand> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|command| {
            Some(SlashCommand {
                name: string(command, "name")
                    .or_else(|| command.as_str())?
                    .trim_start_matches('/')
                    .into(),
                description: string(command, "description").map(str::to_owned),
                input_hint: string(command, "argumentHint").map(str::to_owned),
            })
        })
        .collect()
}

fn choice(value: &str) -> ConfigChoice {
    ConfigChoice {
        value: value.into(),
        name: value.into(),
        description: None,
        group: None,
        danger: false,
    }
}

fn option(
    id: &str,
    name: &str,
    category: ConfigCategory,
    current: &str,
    choices: Vec<ConfigChoice>,
) -> ConfigOption {
    ConfigOption {
        id: id.into(),
        name: name.into(),
        description: None,
        category,
        current_value: current.into(),
        choices,
    }
}

fn token_usage(value: &Value) -> TokenUsage {
    let input = value["input_tokens"].as_u64();
    let created = value["cache_creation_input_tokens"].as_u64();
    let cached = value["cache_read_input_tokens"].as_u64();
    TokenUsage {
        input: add_tokens(add_tokens(input, created), cached),
        cached_input: cached,
        output: value["output_tokens"].as_u64(),
        ..TokenUsage::default()
    }
}

fn accumulate_usage(total: &TokenUsage, additional: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input: add_tokens(total.input, additional.input),
        cached_input: add_tokens(total.cached_input, additional.cached_input),
        output: add_tokens(total.output, additional.output),
        reasoning: add_tokens(total.reasoning, additional.reasoning),
        ..TokenUsage::default()
    }
}

fn add_tokens(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        _ => left.or(right),
    }
}
