//! Mapping of trajectory steps to timeline items. Field numbers are documented in
//! `docs/agents/antigravity-store.md`.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use verre_agent_api::ItemDraft;
use verre_proto::{
    ContentBlock, ItemKind, NoticeLevel, StopReason, ToolCall, ToolContent, ToolKind, ToolLocation,
    ToolStatus,
};

use crate::{
    util::{base64, file_uri_to_path},
    wire::{timestamp_ms, Msg},
};

pub const STEP_USER_INPUT: i64 = 14;
pub const STEP_PLANNER: i64 = 15;
pub const STEP_EPHEMERAL: i64 = 17;
pub const STEP_COMPACTION: i64 = 23;
pub const STEP_SYSTEM_MESSAGE: i64 = 101;
pub const STEP_TOOL: i64 = 132;

/// Tool names of unknown step types start with this prefix.
pub const UNKNOWN_STEP_PREFIX: &str = "antigravity_step_";

/// Images in tool results above this size (raw bytes) are left out of the transcript.
const TOOL_IMAGE_LIMIT: usize = 256 * 1024;
/// Images the user attached are kept up to this size.
const USER_IMAGE_LIMIT: usize = 8 * 1024 * 1024;
const TITLE_LIMIT: usize = 120;

#[derive(Default)]
pub struct Builder {
    items: Vec<ItemDraft>,
    calls: HashMap<String, usize>,
    turn: Option<String>,
    started_at: Option<i64>,
    turn_end: Option<ItemDraft>,
    started: bool,
}

fn status_of(step_status: i64, failed: bool) -> ToolStatus {
    match step_status {
        _ if failed => ToolStatus::Failed,
        6 | 7 => ToolStatus::Failed,
        0 => ToolStatus::Pending,
        1 | 2 | 4 => ToolStatus::InProgress,
        _ => ToolStatus::Completed,
    }
}

fn kind_of(name: &str) -> ToolKind {
    match name {
        "run_command" => ToolKind::Execute,
        "read_file" | "view_file" | "list_dir" => ToolKind::Read,
        "edit_file" | "write_file" => ToolKind::Edit,
        "search_web" | "grep_search" | "find_by_name" | "search_marketplace" => ToolKind::Search,
        "webview_browser" | "read_url_content" => ToolKind::Fetch,
        "call_mcp_tool" => ToolKind::Mcp,
        "invoke_subagent" | "define_subagent" | "manage_subagents" | "send_message" => {
            ToolKind::Agent
        }
        "update_focus" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

fn clip(s: &str) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(TITLE_LIMIT) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

fn display_path(raw: &str) -> String {
    file_uri_to_path(raw).map_or_else(|| raw.to_owned(), |p| p.display().to_string())
}

fn file_name(raw: &str) -> String {
    let path = display_path(raw);
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(&path)
        .to_owned()
}

/// The text a tool returned to the model, without the `Created At:` / `Completed At:` header.
fn result_text(raw: &str) -> String {
    let mut rest = raw;
    while let Some(line) = rest.lines().next() {
        if line.starts_with("Created At:") || line.starts_with("Completed At:") {
            rest = rest[line.len()..].strip_prefix('\n').unwrap_or("");
        } else {
            break;
        }
    }
    rest.to_owned()
}

fn arg<'a>(input: Option<&'a Value>, key: &str) -> Option<&'a str> {
    input?.get(key)?.as_str().filter(|s| !s.is_empty())
}

fn parse_args(call: Option<Msg<'_>>, args: Option<Msg<'_>>) -> Option<Value> {
    if let Some(v) = call
        .and_then(|c| c.str(3))
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
    {
        return Some(v);
    }
    let map: Map<String, Value> = args?
        .msgs(1)
        .filter_map(|a| Some((a.str(1)?.to_owned(), Value::String(a.str(2)?.to_owned()))))
        .collect();
    (!map.is_empty()).then_some(Value::Object(map))
}

fn fallback_title(name: &str, input: Option<&Value>) -> String {
    let main = match name {
        "run_command" => arg(input, "CommandLine"),
        "read_file" | "edit_file" | "write_file" => {
            arg(input, "Path").or(arg(input, "AbsolutePath"))
        }
        "view_file" => arg(input, "AbsolutePath"),
        "search_web" => arg(input, "query"),
        "invoke_subagent" => arg(input, "Role").or(arg(input, "Prompt")),
        "call_mcp_tool" => {
            return match (arg(input, "ServerName"), arg(input, "ToolName")) {
                (Some(s), Some(t)) => format!("{s}: {t}"),
                (None, Some(t)) => t.to_owned(),
                _ => name.to_owned(),
            }
        }
        _ => None,
    };
    match main {
        Some(m) if matches!(name, "read_file" | "edit_file" | "write_file" | "view_file") => {
            format!("{name} {}", file_name(m))
        }
        Some(m) => clip(m),
        None => name.to_owned(),
    }
}

fn render_title(render: Msg<'_>) -> Option<String> {
    let verb = render.text(5)?;
    let chip = render.msgs(9).find_map(|c| {
        c.text(1)
            .map(clip)
            .or_else(|| c.msg(2).and_then(|f| f.text(1)).map(file_name))
            .or_else(|| c.msg(4).and_then(|f| f.text(1)).map(file_name))
            .or_else(|| c.msg(5).and_then(|s| s.text(2)).map(clip))
    });
    Some(match chip {
        Some(c) if !c.is_empty() => format!("{verb} {c}"),
        _ => verb.to_owned(),
    })
}

fn image(
    m: Msg<'_>,
    mime: u32,
    data: u32,
    limit: usize,
) -> Option<Result<(String, String), String>> {
    let bytes = m.bytes(data)?;
    let mime = m.text(mime).unwrap_or("image/png").to_owned();
    if bytes.len() > limit {
        return Some(Err(format!(
            "[{mime} image, {} KiB, not shown]",
            bytes.len().div_ceil(1024)
        )));
    }
    Some(Ok((mime, base64(bytes))))
}

fn unified_diff(path: &str, lines: &[(u64, &str)]) -> Option<String> {
    const CONTEXT: usize = 3;
    let changes: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].0 != 3).collect();
    if changes.is_empty() {
        return None;
    }
    let mut numbers = Vec::with_capacity(lines.len());
    let (mut old, mut new) = (1usize, 1usize);
    for (op, _) in lines {
        numbers.push((old, new));
        match op {
            1 => new += 1,
            2 => old += 1,
            _ => {
                old += 1;
                new += 1;
            }
        }
    }
    let mut out = format!("--- {path}\n+++ {path}\n");
    let mut i = 0;
    while i < changes.len() {
        let start = changes[i].saturating_sub(CONTEXT);
        let mut last = changes[i];
        while i + 1 < changes.len() && changes[i + 1] <= last + 2 * CONTEXT + 1 {
            i += 1;
            last = changes[i];
        }
        let end = (last + CONTEXT + 1).min(lines.len());
        let hunk = &lines[start..end];
        let old_count = hunk.iter().filter(|(op, _)| *op != 1).count();
        let new_count = hunk.iter().filter(|(op, _)| *op != 2).count();
        let (old_start, new_start) = numbers[start];
        let old_start = if old_count == 0 {
            old_start - 1
        } else {
            old_start
        };
        let new_start = if new_count == 0 {
            new_start - 1
        } else {
            new_start
        };
        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for (op, text) in hunk {
            out.push(match op {
                1 => '+',
                2 => '-',
                _ => ' ',
            });
            out.push_str(text.trim_end_matches(['\r', '\n']));
            out.push('\n');
        }
        i += 1;
    }
    Some(out)
}

/// The first short readable string inside an unknown message, for a title.
fn first_text(m: Msg<'_>, depth: usize) -> Option<String> {
    for (_, v) in m.fields() {
        let crate::wire::Value::Bytes(b) = v else {
            continue;
        };
        if depth < 4 && !b.is_empty() && Msg(b).is_valid() {
            if let Some(t) = first_text(Msg(b), depth + 1) {
                return Some(t);
            }
        }
        if let Ok(s) = std::str::from_utf8(b) {
            if s.chars().any(char::is_alphabetic)
                && !s.chars().any(|c| c.is_control() && !c.is_whitespace())
            {
                return Some(clip(s));
            }
        }
    }
    None
}

impl Builder {
    fn push(&mut self, id: String, ts: Option<i64>, kind: ItemKind) {
        self.items.push(ItemDraft {
            id,
            ts,
            turn_id: self.turn.clone(),
            kind,
        });
    }

    fn notice(
        &mut self,
        id: String,
        ts: Option<i64>,
        level: NoticeLevel,
        text: String,
        detail: Option<Value>,
    ) {
        self.push(
            id,
            ts,
            ItemKind::Notice {
                level,
                text,
                detail,
            },
        );
    }

    fn start_turn(&mut self, idx: i64, ts: Option<i64>) {
        self.finish_turn();
        self.turn = Some(format!("turn-{idx}"));
        self.started_at = ts;
        self.started = true;
    }

    fn finish_turn(&mut self) {
        self.items.extend(self.turn_end.take());
    }

    pub fn step(
        &mut self,
        idx: i64,
        step_type: i64,
        status: i64,
        payload: &[u8],
        error: Option<&[u8]>,
    ) {
        let p = Msg(payload);
        let meta = p.msg(5).unwrap_or(Msg(&[]));
        let ts = meta.msg(1).and_then(timestamp_ms);
        let error = error.filter(|e| !e.is_empty()).map(Msg);
        let required_body = match step_type {
            STEP_USER_INPUT => Some((19, "user-input")),
            STEP_PLANNER => Some((20, "planner")),
            STEP_SYSTEM_MESSAGE => Some((114, "system-message")),
            _ => None,
        };
        if let Some((_, shape)) = required_body.filter(|(field, _)| p.msg(*field).is_none()) {
            verre_agent_api::drift::report("antigravity", "step without body", Some(shape));
        }
        match step_type {
            STEP_USER_INPUT => self.user_input(idx, ts, p.msg(19).unwrap_or(Msg(&[]))),
            STEP_PLANNER => self.planner(idx, ts, status, p.msg(20).unwrap_or(Msg(&[])), meta),
            STEP_TOOL => {
                self.turn_end = None;
                return self.tool(idx, ts, status, meta, p.msg(140), error);
            }
            STEP_SYSTEM_MESSAGE => self.system_message(idx, ts, p.msg(114).unwrap_or(Msg(&[]))),
            STEP_EPHEMERAL => {
                let text = p
                    .path(&[24, 3])
                    .and_then(|m| m.text(2))
                    .unwrap_or("System message");
                self.notice(
                    format!("step-{idx}"),
                    ts,
                    NoticeLevel::Info,
                    text.to_owned(),
                    None,
                );
            }
            STEP_COMPACTION => {
                let summary = p.msg(30).and_then(|m| m.text(5));
                self.notice(
                    format!("step-{idx}"),
                    ts,
                    NoticeLevel::Info,
                    "Context compacted".into(),
                    summary.map(|s| json!({ "summary": s })),
                );
            }
            _ => self.unknown(idx, ts, step_type, status, p),
        }
        if let Some(e) = error {
            self.turn_end = None;
            let text = e.text(2).or(e.text(3)).unwrap_or("Step failed").to_owned();
            let detail = e.text(3).map(|d| json!({ "detail": d }));
            self.notice(
                format!("step-{idx}-error"),
                ts,
                NoticeLevel::Error,
                text,
                detail,
            );
        }
    }

    fn user_input(&mut self, idx: i64, ts: Option<i64>, m: Msg<'_>) {
        self.start_turn(idx, ts);
        let text = m.text(2).map(str::to_owned).unwrap_or_else(|| {
            m.msgs(3)
                .filter_map(|i| i.text(1))
                .collect::<Vec<_>>()
                .join("")
        });
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        for att in m.msgs(9) {
            let uri = att.text(5).map(str::to_owned);
            match image(att, 1, 6, USER_IMAGE_LIMIT) {
                Some(Ok((mime_type, data))) => content.push(ContentBlock::Image {
                    mime_type,
                    data: Some(data),
                    uri: None,
                }),
                _ if uri.is_some() => content.push(ContentBlock::Image {
                    mime_type: att.text(1).unwrap_or("image/png").to_owned(),
                    data: None,
                    uri,
                }),
                _ => {}
            }
        }
        if !content.is_empty() {
            self.push(format!("step-{idx}"), ts, ItemKind::UserMessage { content });
        }
    }

    fn planner(&mut self, idx: i64, ts: Option<i64>, status: i64, m: Msg<'_>, meta: Msg<'_>) {
        let answer = m.text(1).or(m.text(8));
        self.turn_end = self
            .turn
            .as_deref()
            .filter(|_| {
                matches!(status, 3 | 5)
                    && answer.is_some_and(|text| !text.trim().is_empty())
                    && m.msgs(7).next().is_none()
            })
            .map(|turn| {
                let ended_at = meta.msg(26).and_then(|history| {
                    history
                        .msgs(1)
                        .filter(|update| update.uint(1) == Some(3))
                        .filter_map(|update| update.msg(2).and_then(timestamp_ms))
                        .last()
                });
                let duration = ended_at
                    .zip(self.started_at)
                    .and_then(|(end, start)| end.checked_sub(start))
                    .and_then(|duration| u64::try_from(duration).ok());
                ItemDraft::turn_end(turn, StopReason::EndTurn, ended_at, duration, None)
            });
        if let Some(text) = m.text(3) {
            self.push(
                format!("step-{idx}-thinking"),
                ts,
                ItemKind::Reasoning {
                    text: text.to_owned(),
                    streaming: false,
                },
            );
        }
        if let Some(text) = answer {
            self.push(
                format!("step-{idx}-text"),
                ts,
                ItemKind::AgentMessage {
                    text: text.to_owned(),
                    streaming: false,
                },
            );
        }
        for call in m.msgs(7) {
            let Some(id) = call.text(1) else { continue };
            let name = call.text(2).unwrap_or("tool");
            let input = parse_args(Some(call), None);
            let tool = ToolCall {
                call_id: id.to_owned(),
                kind: kind_of(name),
                name: Some(name.to_owned()),
                title: fallback_title(name, input.as_ref()),
                status: if matches!(status, 6 | 7) {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Pending
                },
                input,
                output: None,
                content: Vec::new(),
                locations: Vec::new(),
                child_thread_ids: Vec::new(),
            };
            self.calls.insert(id.to_owned(), self.items.len());
            self.push(id.to_owned(), ts, ItemKind::ToolCall(tool));
        }
    }

    fn system_message(&mut self, idx: i64, ts: Option<i64>, m: Msg<'_>) {
        let kind = m.text(3).unwrap_or("");
        let title = m.path(&[2]).and_then(|r| r.text(1));
        let body = m
            .path(&[2, 10])
            .and_then(|b| b.text(1))
            .or_else(|| m.msg(4).and_then(|r| r.text(4)))
            .or_else(|| m.text(1));
        if kind == "agent_message" && !self.started {
            if let Some(text) = body {
                self.start_turn(idx, ts);
                let content = vec![ContentBlock::Text {
                    text: text.to_owned(),
                }];
                self.push(format!("step-{idx}"), ts, ItemKind::UserMessage { content });
                return;
            }
        }
        let text = title
            .map(str::to_owned)
            .or_else(|| (!kind.is_empty()).then(|| kind.replace('_', " ")))
            .unwrap_or_else(|| "System message".into());
        let detail = body.map(|b| json!({ "kind": kind, "text": b }));
        self.notice(format!("step-{idx}"), ts, NoticeLevel::Info, text, detail);
    }

    fn unknown(&mut self, idx: i64, ts: Option<i64>, step_type: i64, status: i64, p: Msg<'_>) {
        let body = p
            .fields()
            .find(|(n, _)| !matches!(n, 1 | 4 | 5))
            .and_then(|(_, v)| match v {
                crate::wire::Value::Bytes(b) => Some(Msg(b)),
                _ => None,
            });
        let title = body
            .and_then(|b| first_text(b, 0))
            .unwrap_or_else(|| format!("Step type {step_type}"));
        let tool = ToolCall {
            call_id: format!("step-{idx}"),
            kind: ToolKind::Other,
            name: Some(format!("{UNKNOWN_STEP_PREFIX}{step_type}")),
            title,
            status: status_of(status, false),
            input: None,
            output: None,
            content: Vec::new(),
            locations: Vec::new(),
            child_thread_ids: Vec::new(),
        };
        self.push(format!("step-{idx}"), ts, ItemKind::ToolCall(tool));
    }

    fn tool(
        &mut self,
        idx: i64,
        ts: Option<i64>,
        status: i64,
        meta: Msg<'_>,
        body: Option<Msg<'_>>,
        error: Option<Msg<'_>>,
    ) {
        let call = meta.msg(4);
        let call_id = call
            .and_then(|c| c.text(1))
            .map_or_else(|| format!("step-{idx}"), str::to_owned);
        let name = call.and_then(|c| c.text(2)).unwrap_or("tool").to_owned();
        let input = parse_args(call, body);
        let result = body.and_then(|b| b.msg(2)).unwrap_or(Msg(&[]));
        let render = result.msg(3).unwrap_or(Msg(&[]));
        let typed = result.msg(6).and_then(|a| a.msg(2)).unwrap_or(Msg(&[]));
        let text = result
            .text(1)
            .map(result_text)
            .filter(|t| !t.trim().is_empty());

        let mut tool = ToolCall {
            call_id: call_id.clone(),
            kind: kind_of(&name),
            title: render_title(render).unwrap_or_else(|| fallback_title(&name, input.as_ref())),
            status: status_of(status, error.is_some()),
            name: Some(name.clone()),
            input,
            output: None,
            content: Vec::new(),
            locations: Vec::new(),
            child_thread_ids: Vec::new(),
        };
        let input = tool.input.as_ref();
        let mut text_used = false;

        match name.as_str() {
            "run_command" => {
                let term = render.path(&[10, 3]);
                let command = term
                    .and_then(|t| t.text(1))
                    .or(typed.text(6))
                    .or(arg(input, "CommandLine"))
                    .map(str::to_owned);
                let cwd = term
                    .and_then(|t| t.text(2))
                    .or(typed.text(7))
                    .map(str::to_owned);
                let exit_code = term
                    .and_then(|t| t.int(3))
                    .or_else(|| typed.int(1))
                    .or_else(|| (!typed.0.is_empty()).then_some(0))
                    .map(|c| c as i32);
                let output = term.and_then(|t| t.str(4)).map(str::to_owned);
                text_used = output.is_none() && text.is_some();
                let output = output.or_else(|| text.clone()).unwrap_or_default();
                if command.is_some() || !output.is_empty() {
                    tool.content.push(ToolContent::Terminal {
                        command,
                        cwd,
                        output,
                        exit_code,
                    });
                }
            }
            "edit_file" | "write_file" => {
                let path = typed
                    .text(2)
                    .map(display_path)
                    .or_else(|| arg(input, "Path").map(display_path))
                    .or_else(|| {
                        render
                            .msgs(9)
                            .find_map(|c| c.msg(4).and_then(|d| d.text(1)).map(display_path))
                    });
                if let Some(path) = path {
                    let lines: Vec<(u64, &str)> = typed
                        .path(&[3, 3])
                        .into_iter()
                        .flat_map(|d| d.msgs(3))
                        .map(|l| (l.uint(2).unwrap_or(3), l.str(1).unwrap_or("")))
                        .collect();
                    let unified = unified_diff(&path, &lines);
                    let (old_text, new_text) = if unified.is_some() {
                        (None, None)
                    } else if name == "write_file" {
                        (None, arg(input, "Content").map(str::to_owned))
                    } else {
                        (
                            arg(input, "Target").map(str::to_owned),
                            arg(input, "Replacement").map(str::to_owned),
                        )
                    };
                    if unified.is_some() || old_text.is_some() || new_text.is_some() {
                        tool.content.push(ToolContent::Diff {
                            path: path.clone(),
                            old_text,
                            new_text,
                            unified,
                        });
                    } else if let Some(stats) = render.msgs(9).find_map(|c| c.path(&[4, 2])) {
                        tool.content.push(ToolContent::Text {
                            text: format!(
                                "+{} -{}",
                                stats.uint(1).unwrap_or(0),
                                stats.uint(2).unwrap_or(0)
                            ),
                        });
                    }
                    tool.locations.push(ToolLocation { path, line: None });
                }
            }
            "read_file" | "view_file" => {
                let path = typed
                    .text(3)
                    .map(display_path)
                    .or_else(|| {
                        arg(input, "Path")
                            .or(arg(input, "AbsolutePath"))
                            .map(display_path)
                    })
                    .or_else(|| {
                        render
                            .msgs(9)
                            .find_map(|c| c.msg(2).and_then(|f| f.text(1)).map(display_path))
                    });
                let line = typed
                    .uint(4)
                    .or_else(|| arg(input, "StartLine").and_then(|s| s.parse().ok()))
                    .and_then(|l| u32::try_from(l).ok());
                if let Some(path) = path {
                    tool.locations.push(ToolLocation { path, line });
                }
            }
            "search_web" => {
                if let Some(summary) = typed.text(4) {
                    let mut out = summary.to_owned();
                    let sources: Vec<String> = typed
                        .msgs(3)
                        .filter_map(|s| {
                            let url = s.text(1).or(s.text(3))?;
                            Some(match s.text(4) {
                                Some(site) => format!("- [{site}]({url})"),
                                None => format!("- {url}"),
                            })
                        })
                        .collect();
                    if !sources.is_empty() {
                        out.push_str("\n\n");
                        out.push_str(&sources.join("\n"));
                    }
                    tool.content.push(ToolContent::Text { text: out });
                    text_used = true;
                }
            }
            "call_mcp_tool" => {
                if let Some(raw) = typed.text(3).or(typed.text(4)) {
                    tool.output = Some(
                        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned())),
                    );
                }
            }
            "invoke_subagent" => {
                let child = typed.path(&[2]).and_then(|c| c.text(1)).or_else(|| {
                    render
                        .msgs(9)
                        .find_map(|c| c.msg(5).and_then(|s| s.text(1)))
                });
                if let Some(child) = child {
                    tool.child_thread_ids.push(format!("antigravity:{child}"));
                    tool.output = Some(json!({ "conversationId": child }));
                }
            }
            _ => {}
        }

        if !text_used {
            if let Some(md) = render.path(&[10]).and_then(|b| b.text(1)) {
                tool.content.push(ToolContent::Text {
                    text: md.to_owned(),
                });
            } else if let Some(text) = text {
                if tool.kind != ToolKind::Execute {
                    tool.content.push(ToolContent::Text { text });
                }
            }
        }
        for img in result.msgs(4) {
            match image(img, 1, 2, TOOL_IMAGE_LIMIT) {
                Some(Ok((mime_type, data))) => {
                    tool.content.push(ToolContent::Image { mime_type, data })
                }
                Some(Err(note)) => tool.content.push(ToolContent::Text { text: note }),
                None => {}
            }
        }
        if let Some(e) = error {
            let text = [e.text(2), e.text(3)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n\n");
            if !text.is_empty() {
                tool.content.push(ToolContent::Text { text });
            }
        }

        match self.calls.get(&call_id).copied() {
            Some(pos) => {
                let item = &mut self.items[pos];
                item.ts = item.ts.or(ts);
                item.kind = ItemKind::ToolCall(tool);
            }
            None => {
                self.calls.insert(call_id.clone(), self.items.len());
                self.push(call_id, ts, ItemKind::ToolCall(tool));
            }
        }
    }

    pub fn finish(mut self) -> Vec<ItemDraft> {
        self.finish_turn();
        self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_footer_uses_native_done_transition_and_leaves_active_tail_open() {
        let mut builder = Builder::default();
        builder.start_turn(1, Some(10_000));
        builder.planner(
            2,
            Some(11_000),
            3,
            Msg(b"\x0a\x06answer"),
            Msg(b"\xd2\x01\x08\x0a\x06\x08\x03\x12\x02\x08\x0d"),
        );
        builder.start_turn(3, None);
        builder.planner(4, None, 2, Msg(b"\x0a\x07partial"), Msg(&[]));
        let items = builder.finish();
        let [_, end, partial] = items.as_slice() else {
            panic!("expected one completed answer and an active tail");
        };
        assert_eq!(end.ts, Some(13_000));
        assert_eq!(end.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(partial.turn_id.as_deref(), Some("turn-3"));
        assert!(matches!(
            end.kind,
            ItemKind::TurnEnd {
                stop_reason: StopReason::EndTurn,
                duration_ms: Some(3_000),
                usage: None,
            }
        ));
    }

    #[test]
    fn unified_diff_groups_hunks_with_context() {
        let mut lines: Vec<(u64, &str)> = (0..20).map(|_| (3, "same\r")).collect();
        lines[2] = (2, "old");
        lines.insert(3, (1, "new"));
        lines[18] = (1, "added");
        let diff = unified_diff("f.txt", &lines).unwrap();
        let hunks: Vec<&str> = diff.lines().filter(|l| l.starts_with("@@")).collect();
        assert_eq!(hunks, ["@@ -1,6 +1,6 @@", "@@ -15,5 +15,6 @@"]);
        assert!(diff.contains("\n-old\n+new\n"));
        assert!(!diff.contains('\r'));
    }

    #[test]
    fn result_header_is_stripped() {
        assert_eq!(
            result_text("Created At: x\nCompleted At: y\nbody\nmore"),
            "body\nmore"
        );
        assert_eq!(result_text("plain"), "plain");
    }
}
