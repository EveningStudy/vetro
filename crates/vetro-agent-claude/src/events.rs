use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use serde_json::{json, Map, Value};
use vetro_agent_api::{ItemDraft, SessionEvent};
use vetro_proto::{
    ContentBlock, ItemKind, NoticeLevel, StopReason, ToolCall, ToolContent, ToolKind, ToolLocation,
    ToolStatus,
};

use crate::string;

#[derive(Default)]
pub(crate) struct Timeline {
    pub turn: Option<String>,
    last_assistant_uuid: Option<String>,
    last_assistant_ts: Option<i64>,
    historical: bool,
    notice_sequence: usize,
    calls: HashMap<String, Call>,
    streams: HashMap<Option<String>, Stream>,
    message_indices: HashMap<String, usize>,
    users: HashSet<String>,
}

struct Call {
    draft: ItemDraft,
    parent: Option<String>,
    output: Option<Value>,
    children: Vec<ItemDraft>,
    positions: HashMap<String, usize>,
}

#[derive(Default)]
struct Stream {
    message: String,
    blocks: HashMap<usize, StreamBlock>,
}

struct StreamBlock {
    draft: ItemDraft,
    emitted: bool,
}

impl Timeline {
    pub fn for_history() -> Self {
        Self {
            historical: true,
            ..Self::default()
        }
    }

    pub fn take_last_assistant_uuid(&mut self) -> Option<String> {
        self.last_assistant_ts = None;
        self.last_assistant_uuid.take()
    }

    pub fn finish_history_turn(&mut self, events: &mut Vec<SessionEvent>) {
        let ts = self.last_assistant_ts;
        if let Some(uuid) = self.take_last_assistant_uuid() {
            events.push(SessionEvent::Item(ItemDraft {
                id: format!("turn:{uuid}"),
                ts,
                turn_id: Some(uuid),
                kind: ItemKind::TurnEnd {
                    stop_reason: StopReason::EndTurn,
                    usage: None,
                    duration_ms: None,
                },
            }));
        }
    }

    pub fn tool_turn(&self, id: &str) -> Option<String> {
        self.calls.get(id)?.draft.turn_id.clone()
    }

    pub fn user(&mut self, id: &str, content: Vec<ContentBlock>, events: &mut Vec<SessionEvent>) {
        self.message_user(id, content, None, None, events);
    }

    pub fn message(&mut self, row: &Value, parent: Option<&str>, events: &mut Vec<SessionEvent>) {
        let id = string(row, "uuid");
        let message = &row["message"];
        let content = &message["content"];
        let ts = timestamp(row);
        if row["type"] == "user" {
            for result in content.as_array().into_iter().flatten() {
                if result["type"] == "tool_result" {
                    self.complete_call(row, result, events);
                }
            }
            if synthetic_user(row) {
                return;
            }
            let blocks = user_content(content);
            if let Some(injected) = injected_user(content, &blocks) {
                self.injected_notice(injected, id, parent, ts, events);
            } else if let Some(id) = id {
                self.message_user(id, blocks, parent, ts, events);
            }
        } else if row["type"] == "assistant" {
            let Some(id) = id else {
                return;
            };
            if parent.is_none() {
                self.last_assistant_uuid = Some(id.into());
                self.last_assistant_ts = ts;
            }
            let message_id = string(message, "id").unwrap_or(id);
            let mut index = row["apiBlockIndex"]
                .as_u64()
                .map(|index| index as usize)
                .unwrap_or_else(|| *self.message_indices.get(message_id).unwrap_or(&0));
            for block in content.as_array().into_iter().flatten() {
                let block_index = index;
                index += 1;
                let partial = self
                    .streams
                    .get_mut(&parent.map(str::to_owned))
                    .filter(|stream| stream.message == message_id)
                    .and_then(|stream| stream.blocks.remove(&block_index))
                    .filter(|block| block.emitted)
                    .map(|block| block.draft);
                if let Some(mut kind) = text_kind(block, false) {
                    if let ItemKind::Reasoning { text, .. } = &mut kind {
                        if text.trim().is_empty() {
                            let Some(ItemDraft {
                                kind: ItemKind::Reasoning { text: streamed, .. },
                                ..
                            }) = &partial
                            else {
                                continue;
                            };
                            text.clone_from(streamed);
                        }
                    }
                    let mut draft = partial.unwrap_or_else(|| {
                        self.draft(format!("{message_id}:{block_index}"), ts, kind.clone())
                    });
                    draft.kind = kind;
                    draft.ts = ts.or(draft.ts);
                    self.emit(draft, parent, events);
                } else if block["type"] == "tool_use" {
                    self.open_call(block, parent, ts, events);
                }
            }
            self.message_indices
                .entry(message_id.into())
                .and_modify(|known| *known = (*known).max(index))
                .or_insert(index);
        }
    }

    fn message_user(
        &mut self,
        id: &str,
        content: Vec<ContentBlock>,
        parent: Option<&str>,
        ts: Option<i64>,
        events: &mut Vec<SessionEvent>,
    ) {
        if content.is_empty() || !self.users.insert(id.into()) {
            return;
        }
        if parent.is_none() {
            if self.historical {
                self.finish_history_turn(events);
            } else {
                self.take_last_assistant_uuid();
            }
            self.turn = Some(id.into());
        }
        self.emit(
            self.draft(id.to_owned(), ts, ItemKind::UserMessage { content }),
            parent,
            events,
        );
    }

    pub fn stream(&mut self, row: &Value, events: &mut Vec<SessionEvent>) {
        let parent = string(row, "parent_tool_use_id");
        let parent_key = parent.map(str::to_owned);
        let event = &row["event"];
        if event["type"] == "message_start" {
            if let Some(message) = string(&event["message"], "id") {
                self.streams.insert(
                    parent_key,
                    Stream {
                        message: message.to_owned(),
                        ..Stream::default()
                    },
                );
            }
            return;
        }
        let Some(index) = event["index"].as_u64().map(|index| index as usize) else {
            return;
        };
        if event["type"] == "content_block_start" {
            let Some(kind) = text_kind(&event["content_block"], true) else {
                return;
            };
            let Some(stream) = self.streams.get(&parent_key) else {
                return;
            };
            let draft = self.draft(format!("{}:{index}", stream.message), None, kind);
            let emitted =
                !matches!(&draft.kind, ItemKind::Reasoning { text, .. } if text.trim().is_empty());
            self.streams.get_mut(&parent_key).unwrap().blocks.insert(
                index,
                StreamBlock {
                    draft: draft.clone(),
                    emitted,
                },
            );
            if emitted {
                self.emit(draft, parent, events);
            }
        } else if event["type"] == "content_block_delta" {
            let delta = &event["delta"];
            let Some(delta) = (match string(delta, "type") {
                Some("text_delta") => string(delta, "text"),
                Some("thinking_delta") => string(delta, "thinking"),
                _ => None,
            })
            .filter(|delta| !delta.is_empty()) else {
                return;
            };
            let Some(block) = self
                .streams
                .get_mut(&parent_key)
                .and_then(|stream| stream.blocks.get_mut(&index))
            else {
                return;
            };
            if let ItemKind::AgentMessage { text, .. } | ItemKind::Reasoning { text, .. } =
                &mut block.draft.kind
            {
                text.push_str(delta);
                if !block.emitted && text.trim().is_empty() {
                    return;
                }
            }
            if !block.emitted || parent.is_some() {
                block.emitted = true;
                let draft = block.draft.clone();
                self.emit(draft, parent, events);
            } else {
                events.push(SessionEvent::TextDelta {
                    item_id: block.draft.id.clone(),
                    delta: delta.into(),
                });
            }
        }
    }

    pub fn finish_streams(&mut self, events: &mut Vec<SessionEvent>) {
        for (parent, stream) in std::mem::take(&mut self.streams) {
            for block in stream.blocks.into_values().filter(|block| block.emitted) {
                let mut draft = block.draft;
                if let ItemKind::AgentMessage { streaming, .. }
                | ItemKind::Reasoning { streaming, .. } = &mut draft.kind
                {
                    *streaming = false;
                }
                self.emit(draft, parent.as_deref(), events);
            }
        }
    }

    pub fn draft(&self, id: String, ts: Option<i64>, kind: ItemKind) -> ItemDraft {
        ItemDraft {
            id,
            ts,
            turn_id: self.turn.clone(),
            kind,
        }
    }

    fn injected_notice(
        &mut self,
        injected: InjectedUser,
        id: Option<&str>,
        parent: Option<&str>,
        ts: Option<i64>,
        events: &mut Vec<SessionEvent>,
    ) {
        let (text, detail) = match injected {
            InjectedUser::Task(body) => {
                let detail: Map<String, Value> =
                    ["task-id", "tool-use-id", "status", "summary", "output-file"]
                        .into_iter()
                        .filter_map(|tag| {
                            let (_, field) = body.split_once(&format!("<{tag}>"))?;
                            let (field, _) = field.split_once(&format!("</{tag}>"))?;
                            let field = field.trim();
                            (!field.is_empty()).then(|| (tag.into(), json!(field)))
                        })
                        .collect();
                let status = detail.get("status").and_then(Value::as_str);
                let (mut text, tool_status) = match status {
                    Some("failed" | "error" | "killed") => {
                        ("Background task failed".into(), Some(ToolStatus::Failed))
                    }
                    Some("completed" | "success") => (
                        "Background task completed".into(),
                        Some(ToolStatus::Completed),
                    ),
                    Some(status) => (format!("Background task ({status})"), None),
                    None => ("Background task".into(), None),
                };
                if let Some((call_id, status)) = detail
                    .get("tool-use-id")
                    .and_then(Value::as_str)
                    .zip(tool_status)
                {
                    if let Some(call) = self.calls.get_mut(call_id) {
                        if let ItemKind::ToolCall(tool) = &mut call.draft.kind {
                            if tool.status == ToolStatus::InProgress {
                                tool.status = status;
                                self.emit_call(call_id, events);
                            }
                        }
                    }
                }
                if let Some(summary) = detail.get("summary").and_then(Value::as_str) {
                    if status.is_some() {
                        text.push_str(": ");
                        text.push_str(summary);
                    } else {
                        text = summary.into();
                    }
                }
                (text, Some(Value::Object(detail)))
            }
            InjectedUser::Stdout(body) if !body.trim().is_empty() => (body, None),
            InjectedUser::Stdout(_) | InjectedUser::Silent => return,
        };
        let id = id.map(str::to_owned).unwrap_or_else(|| {
            self.notice_sequence += 1;
            format!("claude-notice:{}", self.notice_sequence)
        });
        self.emit(
            self.draft(
                id,
                ts,
                ItemKind::Notice {
                    level: NoticeLevel::Info,
                    text,
                    detail,
                },
            ),
            parent,
            events,
        );
    }

    pub fn notice(&mut self, row: &Value, parent: Option<&str>, events: &mut Vec<SessionEvent>) {
        let Some(id) = string(row, "uuid") else {
            return;
        };
        let subtype = string(row, "subtype").unwrap_or_default();
        let (text, detail) = match subtype {
            "hook_started" | "hook_progress" | "hook_response" => {
                let name = string(row, "hook_name").unwrap_or("Hook");
                let output = ["output", "stdout", "stderr"]
                    .into_iter()
                    .filter_map(|key| string(row, key))
                    .find(|text| !text.trim().is_empty());
                let text = match output {
                    Some(output) => format!("{name}: {}", output.trim_end()),
                    None => format!("{name} ({})", subtype.trim_start_matches("hook_")),
                };
                (
                    text,
                    Some(
                        json!({"event": row["hook_event"], "outcome": row["outcome"], "exitCode": row["exit_code"]}),
                    ),
                )
            }
            "compact_boundary" | "microcompact_boundary" => (
                "Claude Code compacted the conversation".into(),
                row.get("compact_metadata")
                    .or_else(|| row.get("compactMetadata"))
                    .cloned(),
            ),
            "status" if row["status"] == "compacting" => {
                ("Claude Code is compacting the conversation".into(), None)
            }
            "local_command_output" | "informational" | "stop_hook_summary" | "api_error" => {
                let Some(text) = string(row, "content").or_else(|| string(row, "output")) else {
                    return;
                };
                (text.to_owned(), None)
            }
            _ => return,
        };
        let level = match string(row, "level").or_else(|| string(row, "outcome")) {
            Some("error") => NoticeLevel::Error,
            Some("warning" | "cancelled") => NoticeLevel::Warning,
            _ => NoticeLevel::Info,
        };
        self.emit(
            self.draft(
                id.into(),
                timestamp(row),
                ItemKind::Notice {
                    level,
                    text,
                    detail,
                },
            ),
            parent,
            events,
        );
    }

    fn emit(&mut self, draft: ItemDraft, parent: Option<&str>, events: &mut Vec<SessionEvent>) {
        let Some(parent) = parent else {
            events.push(SessionEvent::Item(draft));
            return;
        };
        if !self.calls.contains_key(parent) {
            self.open_call(&json!({"id":parent,"name":"Agent"}), None, draft.ts, events);
        }
        let Some(call) = self.calls.get_mut(parent) else {
            return;
        };
        if let Some(position) = call.positions.get(&draft.id) {
            call.children[*position] = draft;
        } else {
            call.positions.insert(draft.id.clone(), call.children.len());
            call.children.push(draft);
        }
        self.emit_call(parent, events);
    }

    fn open_call(
        &mut self,
        block: &Value,
        parent: Option<&str>,
        ts: Option<i64>,
        events: &mut Vec<SessionEvent>,
    ) {
        let Some(id) = string(block, "id").filter(|id| Some(*id) != parent) else {
            return;
        };
        let name = string(block, "name").unwrap_or("Tool");
        let input = block.get("input").cloned();
        let title = input
            .as_ref()
            .and_then(|input| string(input, "description"))
            .unwrap_or(name)
            .into();
        let locations = input
            .as_ref()
            .and_then(|input| string(input, "file_path"))
            .map(|path| {
                vec![ToolLocation {
                    path: path.into(),
                    line: None,
                }]
            })
            .unwrap_or_default();
        let draft = self.draft(
            id.into(),
            ts,
            ItemKind::ToolCall(ToolCall {
                call_id: id.into(),
                kind: tool_kind(name),
                name: Some(name.into()),
                title,
                status: ToolStatus::InProgress,
                input,
                output: None,
                content: Vec::new(),
                locations,
                child_thread_ids: Vec::new(),
            }),
        );
        let call = self.calls.entry(id.into()).or_insert_with(|| Call {
            draft: draft.clone(),
            parent: None,
            output: None,
            children: Vec::new(),
            positions: HashMap::new(),
        });
        if let (ItemKind::ToolCall(previous), ItemKind::ToolCall(mut current)) =
            (&call.draft.kind, draft.kind)
        {
            current.status = previous.status;
            current.content.clone_from(&previous.content);
            call.draft.kind = ItemKind::ToolCall(current);
        }
        call.draft.ts = ts.or(call.draft.ts);
        call.parent = parent.map(str::to_owned);
        self.emit_call(id, events);
    }

    fn complete_call(&mut self, row: &Value, result: &Value, events: &mut Vec<SessionEvent>) {
        let Some(id) = string(result, "tool_use_id") else {
            return;
        };
        let Some(call) = self.calls.get_mut(id) else {
            return;
        };
        let ItemKind::ToolCall(tool) = &mut call.draft.kind else {
            return;
        };
        let native = row
            .get("tool_use_result")
            .or_else(|| row.get("toolUseResult"));
        call.output = native
            .filter(|value| !value.is_null())
            .or_else(|| result.get("content"))
            .cloned();
        tool.status = if result["is_error"] == true {
            ToolStatus::Failed
        } else if native.is_some_and(|native| {
            native["detachedToolCall"] == true || native["status"] == "async_launched"
        }) {
            ToolStatus::InProgress
        } else {
            ToolStatus::Completed
        };
        tool.content = tool_content(&result["content"]);
        self.emit_call(id, events);
    }

    fn emit_call(&mut self, id: &str, events: &mut Vec<SessionEvent>) {
        let call = &mut self.calls.get_mut(id).unwrap();
        if let ItemKind::ToolCall(tool) = &mut call.draft.kind {
            tool.output =
                if call.children.is_empty() {
                    call.output.clone()
                } else {
                    let children: Vec<_> = call.children.iter().map(|item| json!({
                    "id": item.id, "ts": item.ts, "turnId": item.turn_id, "kind": item.kind,
                })).collect();
                    Some(json!({"result": call.output, "subagentItems": children}))
                };
        }
        let mut ancestors = HashSet::from([id]);
        let mut parent = self.calls[id].parent.as_deref();
        while let Some(id) = parent {
            if !ancestors.insert(id) {
                return;
            }
            parent = self.calls.get(id).and_then(|call| call.parent.as_deref());
        }
        let call = &self.calls[id];
        let (draft, parent) = (call.draft.clone(), call.parent.clone());
        self.emit(draft, parent.as_deref(), events);
    }
}

pub(crate) fn synthetic_user(row: &Value) -> bool {
    [
        "isSynthetic",
        "isMeta",
        "isVisibleInTranscriptOnly",
        "isCompactSummary",
    ]
    .iter()
    .any(|key| row[*key] == true)
}

pub(crate) enum InjectedUser {
    Task(String),
    Stdout(String),
    Silent,
}

pub(crate) fn injected_user(content: &Value, blocks: &[ContentBlock]) -> Option<InjectedUser> {
    if blocks.is_empty()
        || content
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "file"))
    {
        return None;
    }
    let text = match blocks {
        [ContentBlock::Text { text }] => Cow::Borrowed(text.as_str()),
        _ => Cow::Owned(
            blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?
                .join("\n"),
        ),
    };
    let mut remaining = text.trim();
    if let Some((body, "")) = xml_element(remaining, "task-notification") {
        return Some(InjectedUser::Task(body.into()));
    }
    if let Some((body, "")) = xml_element(remaining, "local-command-stdout") {
        return Some(InjectedUser::Stdout(body.into()));
    }
    let silent_tags = [
        "local-command-caveat",
        "command-name",
        "command-message",
        "command-args",
        "system-reminder",
    ];
    loop {
        let (_, tail) = silent_tags
            .iter()
            .find_map(|tag| xml_element(remaining, tag))?;
        remaining = tail.trim_start();
        if remaining.is_empty() {
            return Some(InjectedUser::Silent);
        }
    }
}

fn xml_element<'a>(text: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    text.strip_prefix('<')?
        .strip_prefix(tag)?
        .strip_prefix('>')?
        .split_once(&format!("</{tag}>"))
}

pub(crate) fn user_content(content: &Value) -> Vec<ContentBlock> {
    if let Some(text) = content.as_str() {
        return vec![ContentBlock::Text { text: text.into() }];
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| match string(block, "type")? {
            "text" => Some(ContentBlock::Text {
                text: string(block, "text")?.into(),
            }),
            "image" => {
                let source = &block["source"];
                Some(ContentBlock::Image {
                    mime_type: string(source, "media_type").unwrap_or("image/*").into(),
                    data: string(source, "data").map(str::to_owned),
                    uri: string(source, "url").map(str::to_owned),
                })
            }
            _ => None,
        })
        .collect()
}

fn text_kind(block: &Value, streaming: bool) -> Option<ItemKind> {
    match string(block, "type")? {
        "text" => Some(ItemKind::AgentMessage {
            text: string(block, "text").unwrap_or_default().into(),
            streaming,
        }),
        "thinking" => Some(ItemKind::Reasoning {
            text: string(block, "thinking").unwrap_or_default().into(),
            streaming,
        }),
        _ => None,
    }
}

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Read" | "NotebookRead" => ToolKind::Read,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => ToolKind::Edit,
        "Grep" | "Glob" | "WebSearch" | "ToolSearch" => ToolKind::Search,
        "Bash" | "PowerShell" | "TaskOutput" => ToolKind::Execute,
        "WebFetch" => ToolKind::Fetch,
        "Agent" | "Task" => ToolKind::Agent,
        "EnterPlanMode" | "ExitPlanMode" | "TodoWrite" => ToolKind::Think,
        _ if name.starts_with("mcp__") => ToolKind::Mcp,
        _ => ToolKind::Other,
    }
}

fn tool_content(content: &Value) -> Vec<ToolContent> {
    if let Some(text) = content.as_str() {
        return vec![ToolContent::Text { text: text.into() }];
    }
    user_content(content)
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(ToolContent::Text { text }),
            ContentBlock::Image {
                mime_type,
                data: Some(data),
                ..
            } => Some(ToolContent::Image { mime_type, data }),
            _ => None,
        })
        .collect()
}

pub(crate) fn timestamp(row: &Value) -> Option<i64> {
    let text = string(row, "timestamp")?;
    let number = |start, end| text.get(start..end)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if !text.ends_with('Z') || !matches!(month, 1..=12) || !matches!(day, 1..=31) {
        return None;
    }
    let prior = year - 1;
    let years = prior * 365 + prior / 4 - prior / 100 + prior / 400;
    let months = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = month > 2 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = years - 719_162 + months[(month - 1) as usize] + day - 1 + i64::from(leap);
    let millis = if text.as_bytes().get(19) == Some(&b'.') {
        number(20, 23).unwrap_or_default()
    } else {
        0
    };
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis)
}
