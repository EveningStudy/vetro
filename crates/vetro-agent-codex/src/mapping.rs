use std::{collections::BTreeMap, path::PathBuf};

use serde_json::{json, Value};
use vetro_agent_api::{codex_internal_source, ItemDraft, Result, SessionRecord, Transcript};
use vetro_proto::{
    ContentBlock, ItemKind, NoticeLevel, PlanEntry, PlanStatus, RateLimit, StopReason,
    ThreadSource, TokenUsage, ToolCall, ToolContent, ToolKind, ToolLocation, ToolStatus,
};

use crate::bad_request;

pub(crate) fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

pub(crate) fn display_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
    }
}

pub(crate) fn command_text(command: &str) -> String {
    let Some((program, args)) = command
        .strip_prefix('"')
        .and_then(|text| text.split_once('"'))
    else {
        return command.to_owned();
    };
    if program.as_bytes().get(1) == Some(&b':')
        || program.starts_with(r"\\\\")
        || program.starts_with(r"\\?\")
    {
        let program = if program.starts_with(r"\\?\") {
            program.to_owned()
        } else {
            program.replace(r"\\", r"\")
        };
        return format!("\"{}\"{args}", display_path(&program));
    }
    command.to_owned()
}

pub(crate) fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

pub(crate) fn millis(value: &Value) -> Option<i64> {
    value.as_i64().and_then(|seconds| seconds.checked_mul(1000))
}

pub(crate) fn input(content: Vec<ContentBlock>) -> Result<Value> {
    let items: Result<Vec<_>> = content
        .into_iter()
        .map(|block| {
            Ok(match block {
                ContentBlock::Text { text } => {
                    json!({"type": "text", "text": text, "text_elements": []})
                }
                ContentBlock::Image {
                    data: Some(data),
                    mime_type,
                    ..
                } => {
                    json!({"type": "image", "url": format!("data:{mime_type};base64,{data}")})
                }
                ContentBlock::Image { uri: Some(url), .. } => {
                    json!({"type": "image", "url": url})
                }
                ContentBlock::Image { .. } => {
                    return Err(bad_request("Codex image requires data or a URI"))
                }
                ContentBlock::File { path, name } => {
                    let name = name.unwrap_or_else(|| {
                        path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned()
                    });
                    json!({"type": "mention", "path": path, "name": name})
                }
            })
        })
        .collect();
    Ok(json!(items?))
}

fn user_content(input: &Value) -> Option<ContentBlock> {
    Some(match input["type"].as_str()? {
        "text" => ContentBlock::Text {
            text: string(input, "text")?,
        },
        "image" => {
            let url = string(input, "url").or_else(|| string(input, "fileId"))?;
            if let Some((mime, data)) = url
                .strip_prefix("data:")
                .and_then(|value| value.split_once(";base64,"))
            {
                ContentBlock::Image {
                    mime_type: mime.into(),
                    data: Some(data.into()),
                    uri: None,
                }
            } else {
                ContentBlock::Image {
                    mime_type: String::new(),
                    data: None,
                    uri: Some(url),
                }
            }
        }
        "localImage" => ContentBlock::Image {
            mime_type: String::new(),
            data: None,
            uri: string(input, "path"),
        },
        "mention" | "skill" => ContentBlock::File {
            path: display_path(input["path"].as_str()?),
            name: string(input, "name"),
        },
        _ => return None,
    })
}

pub(crate) fn record(thread: &Value) -> Option<SessionRecord> {
    let source = &thread["source"];
    let originator = string(thread, "originator");
    let origin_key = originator
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let source_kind = match (origin_key.as_str(), source.as_str()) {
        ("vetro", _) => ThreadSource::Vetro,
        ("codex desktop" | "codex_desktop" | "codex_app" | "codex app", _) => ThreadSource::Desktop,
        (_, Some("exec")) | ("codex_exec", _) => ThreadSource::Exec,
        (_, Some("cli")) | ("codex_cli_rs", _) => ThreadSource::Cli,
        (_, Some("vscode")) | ("codex_vscode", _) => ThreadSource::Ide,
        _ => ThreadSource::Unknown,
    };
    let origin = originator.or_else(|| {
        source
            .as_str()
            .map(str::to_owned)
            .or_else(|| string(source, "custom"))
            .or_else(|| source.as_object().map(|_| source.to_string()))
    });
    let preview = string(thread, "preview").filter(|value| !value.is_empty());
    let record = SessionRecord {
        native_id: string(thread, "id")?,
        title: string(thread, "name")
            .filter(|value| !value.is_empty())
            .or_else(|| preview.clone())
            .unwrap_or_else(|| "Codex thread".into()),
        preview,
        cwd: thread["cwd"].as_str().map(display_path).map(PathBuf::from),
        created_at: millis(&thread["createdAt"]),
        updated_at: millis(&thread["updatedAt"]).unwrap_or_default(),
        source: source_kind,
        origin,
        model: string(thread, "model"),
        resumable: thread["ephemeral"] != true,
        parent_native_id: string(thread, "parentThreadId")
            .or_else(|| string(&source["subagent"]["thread_spawn"], "parent_thread_id")),
        internal: codex_internal_source(source),
    };
    Some(record.classify("codex"))
}

pub(crate) fn transcript(thread: &Value) -> Transcript {
    let mut items = Vec::new();
    for turn in array(&thread["turns"]) {
        let Some(id) = turn["id"].as_str() else {
            continue;
        };
        for raw in array(&turn["items"]) {
            if let Some(item) = item(raw, Some(id), false) {
                items.push(item);
            }
        }
        if let Some(end) = turn_end(turn, None) {
            items.push(end);
        }
    }
    Transcript { items }
}

pub(crate) fn item(raw: &Value, turn_id: Option<&str>, streaming: bool) -> Option<ItemDraft> {
    let id = raw["id"].as_str()?;
    let kind = match raw["type"].as_str()? {
        "userMessage" => ItemKind::UserMessage {
            content: array(&raw["content"])
                .iter()
                .filter_map(user_content)
                .collect(),
        },
        "agentMessage" => ItemKind::AgentMessage {
            text: string(raw, "text").filter(|text| !text.is_empty())?,
            streaming,
        },
        "reasoning" => ItemKind::Reasoning {
            text: reasoning_text(raw),
            streaming,
        },
        "plan" => ItemKind::Plan {
            entries: plan_text(raw["text"].as_str().unwrap_or_default()),
        },
        "hookPrompt" => notice(NoticeLevel::Info, "Codex hook prompt", raw),
        "contextCompaction" => notice(NoticeLevel::Info, "Codex compacted the context", raw),
        "enteredReviewMode" | "exitedReviewMode" => notice(NoticeLevel::Info, "Codex review", raw),
        item_type => ItemKind::ToolCall(tool(item_type, id, raw, streaming)?),
    };
    Some(ItemDraft {
        id: id.into(),
        ts: raw["startedAtMs"]
            .as_i64()
            .or_else(|| raw["completedAtMs"].as_i64()),
        turn_id: turn_id.map(str::to_owned),
        kind,
    })
}

pub(crate) fn reasoning_text(raw: &Value) -> String {
    array(&raw["summary"])
        .iter()
        .chain(array(&raw["content"]))
        .filter_map(Value::as_str)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn plan_text(text: &str) -> Vec<PlanEntry> {
    (!text.is_empty())
        .then(|| PlanEntry {
            content: text.into(),
            status: PlanStatus::Pending,
        })
        .into_iter()
        .collect()
}

pub(crate) fn plan_steps(raw: &Value) -> Vec<PlanEntry> {
    array(raw)
        .iter()
        .filter_map(|entry| {
            Some(PlanEntry {
                content: string(entry, "step")?,
                status: match entry["status"].as_str() {
                    Some("completed") => PlanStatus::Completed,
                    Some("inProgress") => PlanStatus::InProgress,
                    _ => PlanStatus::Pending,
                },
            })
        })
        .collect()
}

fn tool(item_type: &str, id: &str, raw: &Value, streaming: bool) -> Option<ToolCall> {
    let mut tool = ToolCall {
        call_id: id.into(),
        kind: ToolKind::Other,
        name: string(raw, "tool"),
        title: item_type.into(),
        status: match raw["status"].as_str() {
            Some("inProgress") => ToolStatus::InProgress,
            Some("failed" | "declined" | "interrupted") => ToolStatus::Failed,
            Some("completed") => ToolStatus::Completed,
            _ if streaming => ToolStatus::InProgress,
            _ => ToolStatus::Completed,
        },
        input: non_null(&raw["arguments"]),
        output: non_null(&raw["result"]).or_else(|| non_null(&raw["error"])),
        content: Vec::new(),
        locations: Vec::new(),
        child_thread_ids: Vec::new(),
    };
    match item_type {
        "commandExecution" => {
            tool.kind = ToolKind::Execute;
            let command = raw["command"].as_str().map(command_text);
            let cwd = raw["cwd"].as_str().map(display_path);
            tool.title = command.clone().unwrap_or_else(|| "Run command".into());
            tool.input = Some(json!({"command": command, "cwd": cwd}));
            tool.content.push(ToolContent::Terminal {
                command,
                cwd,
                output: string(raw, "aggregatedOutput").unwrap_or_default(),
                exit_code: raw["exitCode"]
                    .as_i64()
                    .and_then(|code| i32::try_from(code).ok()),
            });
        }
        "fileChange" => {
            tool.kind = ToolKind::Edit;
            tool.title = "Edit files".into();
            tool.content = diffs(&raw["changes"]);
            tool.locations = locations(&raw["changes"]);
        }
        "mcpToolCall" => {
            tool.kind = ToolKind::Mcp;
            let server = raw["server"].as_str().unwrap_or("MCP");
            let name = raw["tool"].as_str().unwrap_or("tool");
            tool.title = format!("{server}: {name}");
            tool.content = array(&raw["result"]["content"])
                .iter()
                .filter_map(mcp_content)
                .collect();
        }
        "dynamicToolCall" => {
            tool.title = string(raw, "tool").unwrap_or_else(|| "Tool call".into());
            tool.output = non_null(&raw["contentItems"]);
        }
        "webSearch" => {
            tool.kind = ToolKind::Search;
            tool.title = string(raw, "query")
                .filter(|query| !query.is_empty())
                .unwrap_or_else(|| "Web search".into());
            tool.input = Some(json!({"query": raw["query"], "action": raw["action"]}));
        }
        "collabAgentToolCall" => {
            tool.kind = ToolKind::Agent;
            tool.title = string(raw, "tool").unwrap_or_else(|| "Collaborate with agent".into());
            tool.input = non_null(&raw["prompt"]);
            tool.output = non_null(&raw["agentsStates"]);
            if raw["tool"] == "spawnAgent" {
                tool.child_thread_ids = array(&raw["receiverThreadIds"])
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|id| format!("codex:{id}"))
                    .collect();
            }
        }
        "subAgentActivity" => {
            tool.kind = ToolKind::Agent;
            tool.title = string(raw, "kind").unwrap_or_else(|| "Sub-agent activity".into());
            tool.output = Some(raw.clone());
            if let Some(id) = raw["agentThreadId"].as_str() {
                tool.child_thread_ids = vec![format!("codex:{id}")];
            }
        }
        "imageView" => {
            tool.kind = ToolKind::Read;
            tool.title = "View image".into();
            tool.locations = locations(&json!([raw]));
        }
        _ => return None,
    }
    Some(tool)
}

fn mcp_content(raw: &Value) -> Option<ToolContent> {
    match raw["type"].as_str()? {
        "text" => Some(ToolContent::Text {
            text: string(raw, "text")?,
        }),
        "image" => Some(ToolContent::Image {
            mime_type: string(raw, "mimeType")?,
            data: string(raw, "data")?,
        }),
        _ => None,
    }
}

fn non_null(raw: &Value) -> Option<Value> {
    (!raw.is_null()).then(|| raw.clone())
}

pub(crate) fn diffs(raw: &Value) -> Vec<ToolContent> {
    array(raw)
        .iter()
        .filter_map(|change| {
            let diff = string(change, "diff");
            let (old_text, new_text, unified) =
                match change["kind"]["type"].as_str().or(change["kind"].as_str()) {
                    Some("add") => (None, diff, None),
                    Some("delete") => (diff, None, None),
                    _ if change["oldText"].is_string() || change["newText"].is_string() => {
                        (string(change, "oldText"), string(change, "newText"), None)
                    }
                    _ if diff
                        .as_ref()
                        .is_some_and(|text| text.lines().any(|line| line.starts_with("@@ "))) =>
                    {
                        (None, None, diff)
                    }
                    _ => {
                        let (mut previous, mut next) = (String::new(), String::new());
                        for line in diff.as_deref().unwrap_or_default().split_inclusive('\n') {
                            if let Some(removed) = line.strip_prefix('-') {
                                previous.push_str(removed);
                            } else if let Some(added) = line.strip_prefix('+') {
                                next.push_str(added);
                            } else {
                                previous.push_str(line);
                                next.push_str(line);
                            }
                        }
                        (Some(previous), Some(next), None)
                    }
                };
            Some(ToolContent::Diff {
                path: display_path(change["path"].as_str()?),
                old_text,
                new_text,
                unified,
            })
        })
        .collect()
}

pub(crate) fn locations(raw: &Value) -> Vec<ToolLocation> {
    array(raw)
        .iter()
        .filter_map(|change| {
            Some(ToolLocation {
                path: display_path(change["path"].as_str()?),
                line: None,
            })
        })
        .collect()
}

pub(crate) fn notice(level: NoticeLevel, text: impl Into<String>, detail: &Value) -> ItemKind {
    ItemKind::Notice {
        level,
        text: text.into(),
        detail: non_null(detail),
    }
}

pub(crate) fn global_notice(method: &str) -> bool {
    matches!(
        method,
        "warning"
            | "configWarning"
            | "deprecationNotice"
            | "guardianWarning"
            | "windows/worldWritableWarning"
    )
}

pub(crate) fn notification_notice(method: &str, params: &Value) -> Option<ItemKind> {
    let (level, text) = match method {
        "error" => (
            NoticeLevel::Error,
            params["error"]["message"]
                .as_str()
                .unwrap_or("Codex error")
                .into(),
        ),
        "hook/started" | "hook/completed" => {
            let run = &params["run"];
            let name = run["eventName"].as_str().unwrap_or("hook");
            let status = run["status"]
                .as_str()
                .unwrap_or(if method == "hook/started" {
                    "started"
                } else {
                    "completed"
                });
            (NoticeLevel::Info, format!("Codex hook {name}: {status}"))
        }
        "model/rerouted" => {
            let from = params["fromModel"].as_str().unwrap_or("unknown");
            let to = params["toModel"].as_str().unwrap_or("unknown");
            (
                NoticeLevel::Warning,
                format!("Codex switched models from {from} to {to}"),
            )
        }
        _ if global_notice(method) => (
            NoticeLevel::Warning,
            string(params, "message")
                .or_else(|| string(params, "summary"))
                .unwrap_or_else(|| "Codex warning".into()),
        ),
        _ => return None,
    };
    Some(notice(level, text, params))
}

pub(crate) fn turn_end(turn: &Value, usage: Option<TokenUsage>) -> Option<ItemDraft> {
    let id = turn["id"].as_str()?;
    let stop_reason = match turn["status"].as_str()? {
        "completed" => StopReason::EndTurn,
        "interrupted" => StopReason::Cancelled,
        "failed" => StopReason::Error,
        _ => return None,
    };
    Some(ItemDraft {
        id: format!("{id}:end"),
        ts: millis(&turn["completedAt"]),
        turn_id: Some(id.into()),
        kind: ItemKind::TurnEnd {
            stop_reason,
            usage,
            duration_ms: turn["durationMs"].as_u64(),
        },
    })
}

pub(crate) fn tokens(raw: &Value, window: Option<u64>) -> Option<TokenUsage> {
    raw.as_object()?;
    Some(TokenUsage {
        input: raw["inputTokens"].as_u64(),
        cached_input: raw["cachedInputTokens"].as_u64(),
        output: raw["outputTokens"].as_u64(),
        reasoning: raw["reasoningOutputTokens"].as_u64(),
        context_used: None,
        context_window: window,
    })
}

pub(crate) fn limit_id(raw: &Value) -> String {
    string(raw, "limitId").unwrap_or_else(|| "codex".into())
}

pub(crate) fn rate_limits(buckets: &BTreeMap<String, Value>) -> Vec<RateLimit> {
    buckets
        .iter()
        .flat_map(|(id, bucket)| {
            let label = bucket["limitName"].as_str().unwrap_or(id);
            ["primary", "secondary"].into_iter().filter_map(move |key| {
                let window = &bucket[key];
                let duration = match window["windowDurationMins"].as_u64() {
                    Some(mins) if mins % 1440 == 0 => format!("{}d", mins / 1440),
                    Some(mins) if mins % 60 == 0 => format!("{}h", mins / 60),
                    Some(mins) => format!("{mins}m"),
                    None => key.into(),
                };
                Some(RateLimit {
                    label: format!("{label} ({duration})"),
                    used_percent: window["usedPercent"].as_f64()?,
                    resets_at: millis(&window["resetsAt"]),
                })
            })
        })
        .collect()
}
