use std::{
    collections::{HashMap, HashSet},
    env,
    path::PathBuf,
};

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use verre_agent_api::{AgentError, ItemDraft, Result, SessionPage, SessionRecord, Transcript};
use verre_proto::{
    rpc::ErrorCode, ContentBlock, ItemKind, StopReason, ThreadSource, TokenUsage, ToolContent,
    ToolStatus,
};
use verre_store_sqlite::open_readonly;

use crate::{mapping, SpawnSpec};

async fn snapshot<T: Send + 'static>(
    spec: &SpawnSpec,
    read: impl FnOnce(&Connection) -> rusqlite::Result<Option<T>> + Send + 'static,
) -> Result<Option<T>> {
    let Some(app_data) = spec
        .env
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("APPDATA"))
        .map(|(_, value)| value.clone())
        .or_else(|| env::var_os("APPDATA"))
    else {
        return Ok(None);
    };
    let path = PathBuf::from(app_data).join("Devin/cli/sessions.db");
    if !path.is_absolute() {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let read = || {
            if !path
                .try_exists()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
            {
                return Ok(None);
            }
            read(&open_readonly(&path)?)
        };
        read().map_err(|error: rusqlite::Error| {
            let code = match error.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                    ErrorCode::Busy
                }
                _ => ErrorCode::Internal,
            };
            AgentError::new(code, format!("{}: {error}", path.display()))
        })
    })
    .await
    .map_err(|error| AgentError::new(ErrorCode::Internal, error.to_string()))?
}

pub(crate) async fn list(spec: &SpawnSpec) -> Result<Option<SessionPage>> {
    snapshot(spec, |db| {
        let mut query = db.prepare(
            "SELECT id, title, working_directory, created_at, last_activity_at, model
             FROM sessions WHERE hidden=0 ORDER BY last_activity_at DESC",
        )?;
        let rows = query.query_map([], |row| {
            let id: String = row.get(0)?;
            Ok(SessionRecord {
                title: row
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| id.clone()),
                native_id: id,
                preview: None,
                cwd: Some(PathBuf::from(row.get::<_, String>(2)?)),
                created_at: row.get::<_, i64>(3)?.checked_mul(1000),
                updated_at: row.get::<_, i64>(4)?.saturating_mul(1000),
                source: ThreadSource::Unknown,
                origin: None,
                model: row.get(5)?,
                resumable: true,
                parent_native_id: None,
                internal: false,
            }
            .classify("devin"))
        })?;
        Ok(Some(SessionPage {
            sessions: rows.collect::<rusqlite::Result<_>>()?,
            incomplete: true,
            ..Default::default()
        }))
    })
    .await
}

pub(crate) async fn read(spec: &SpawnSpec, id: &str) -> Result<Option<Transcript>> {
    let id = id.to_owned();
    snapshot(spec, move |db| read_session(db, &id)).await
}

fn read_session(db: &Connection, id: &str) -> rusqlite::Result<Option<Transcript>> {
    let snapshot = db.unchecked_transaction()?;
    let Some(head) = snapshot
        .query_row(
            "SELECT main_chain_id FROM sessions WHERE id = ?1",
            [id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };

    let mut nodes = snapshot.prepare(
        "WITH RECURSIVE chain(node_id, parent_node_id) AS (
            SELECT node_id, parent_node_id FROM message_nodes
            WHERE session_id = ?1 AND node_id = ?2
            UNION
            SELECT parent.node_id, parent.parent_node_id
            FROM chain JOIN message_nodes parent ON parent.node_id = chain.parent_node_id
            WHERE parent.session_id = ?1
        )
        SELECT node.node_id, node.created_at, node.chat_message
        FROM chain JOIN message_nodes node ON node.node_id = chain.node_id
        WHERE node.session_id = ?1 ORDER BY node.node_id",
    )?;
    let mut tool_states = snapshot.prepare(
        "SELECT tool_call_json, tool_call_update_json FROM tool_call_state
         WHERE session_id = ?1 AND tool_call_id = ?2",
    )?;
    let mut transcript = Transcript::default();
    let mut tools = HashMap::new();
    let mut turn = None;
    let mut rows = nodes.query(rusqlite::params![id, head])?;
    while let Some(row) = rows.next()? {
        let node_id: i64 = row.get(0)?;
        let message = row.get_ref(2)?.as_str()?;
        let Ok(message) = serde_json::from_str::<Value>(message) else {
            verre_agent_api::drift::report("devin", "SQLite chat_message", Some("invalid-json"));
            continue;
        };
        let seconds: i64 = row.get(1)?;
        let metadata = &message["metadata"];
        let ts = mapping::timestamp(&metadata["created_at"]).or_else(|| seconds.checked_mul(1000));
        let first = transcript.items.len();
        match message["role"].as_str() {
            Some("user") if metadata["is_user_input"].as_bool() == Some(true) => {
                let content = user_content(&message);
                if !content.is_empty() {
                    turn = Some(Turn {
                        id: format!("devin:turn:{node_id}"),
                        started_at: ts,
                        ..Turn::default()
                    });
                    transcript.items.push(draft(
                        format!("devin:user:{node_id}"),
                        ts,
                        ItemKind::UserMessage { content },
                    ));
                }
            }
            Some("assistant") => {
                if let Some(turn) = &mut turn {
                    turn.observe_usage(metadata);
                }
                if let Some(text) = text(&message["thinking"]["thinking"]) {
                    transcript.items.push(draft(
                        format!("devin:reasoning:{node_id}"),
                        ts,
                        ItemKind::Reasoning {
                            text,
                            streaming: false,
                        },
                    ));
                }
                if let Some(text) = text(&message["content"]) {
                    transcript.items.push(draft(
                        format!("devin:assistant:{node_id}"),
                        ts,
                        ItemKind::AgentMessage {
                            text,
                            streaming: false,
                        },
                    ));
                }
                for call in mapping::array(&message["tool_calls"]) {
                    let Some(call_id) = call["id"].as_str() else {
                        continue;
                    };
                    if tools.contains_key(call_id) {
                        continue;
                    }
                    let mut tool = mapping::new_tool(call_id.to_owned());
                    tool.name = mapping::string(call, "name");
                    tool.title = tool.name.clone().unwrap_or_default();
                    tool.input = call.get("arguments").cloned();
                    let mut updated = false;
                    let mut stored = tool_states.query([id, call_id])?;
                    if let Some(row) = stored.next()? {
                        for column in 0..2 {
                            let raw: Option<String> = row.get(column)?;
                            if let Some(fields) =
                                raw.and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                            {
                                mapping::patch_tool(&mut tool, &fields);
                                updated |= column == 1;
                            }
                        }
                    }
                    tools.insert(call_id.to_owned(), (transcript.items.len(), updated));
                    transcript.items.push(draft(
                        format!("tool:{call_id}"),
                        ts,
                        ItemKind::ToolCall(tool),
                    ));
                }
            }
            Some("tool") => {
                if let Some(&(index, updated)) = message["tool_call_id"]
                    .as_str()
                    .and_then(|call_id| tools.get(call_id))
                {
                    if let ItemKind::ToolCall(tool) = &mut transcript.items[index].kind {
                        if !updated {
                            tool.status = ToolStatus::Completed;
                        }
                        if let Some(text) = text(&message["content"]) {
                            let has_result = tool.output.is_some()
                                || tool.content.iter().any(|content| {
                                    matches!(
                                        content,
                                        ToolContent::Text { .. } | ToolContent::Terminal { .. }
                                    )
                                });
                            if !has_result {
                                tool.content.push(ToolContent::Text { text });
                            }
                        }
                        if let Some(input) = tool
                            .input
                            .get_or_insert_with(|| serde_json::json!({}))
                            .as_object_mut()
                        {
                            if let Some(ts) = ts {
                                input.entry("completedAtMs").or_insert(Value::from(ts));
                            }
                        }
                    }
                }
            }
            Some("user" | "system" | "developer") => {}
            role => verre_agent_api::drift::report("devin", "SQLite chat_message role", role),
        }
        if let Some(active) = &turn {
            for item in &mut transcript.items[first..] {
                item.turn_id = Some(active.id.clone());
            }
        }
        let reason = match (message["role"].as_str(), metadata["finish_reason"].as_str()) {
            (Some("assistant"), Some("stop")) => Some(StopReason::EndTurn),
            (Some("assistant"), Some("length")) => Some(StopReason::MaxTokens),
            _ => None,
        };
        if let Some((reason, finished)) = reason
            .filter(|_| mapping::array(&message["tool_calls"]).next().is_none())
            .and_then(|reason| turn.take().map(|turn| (reason, turn)))
        {
            let duration = ts
                .zip(finished.started_at)
                .and_then(|(end, start)| end.checked_sub(start))
                .and_then(|duration| u64::try_from(duration).ok());
            transcript.items.push(ItemDraft::turn_end(
                &finished.id,
                reason,
                ts,
                duration,
                finished
                    .usage
                    .filter(|usage| *usage != TokenUsage::default()),
            ));
        }
    }
    Ok(Some(transcript))
}

#[derive(Default)]
struct Turn {
    id: String,
    started_at: Option<i64>,
    requests: HashSet<String>,
    usage: Option<TokenUsage>,
}

impl Turn {
    fn observe_usage(&mut self, metadata: &Value) {
        let Some(request) = metadata["request_id"].as_str() else {
            return;
        };
        if !self.requests.insert(request.to_owned()) {
            return;
        }
        let metrics = &metadata["metrics"];
        let mut usage = TokenUsage {
            input: metrics["input_tokens"].as_u64(),
            cached_input: metrics["cache_read_tokens"].as_u64(),
            output: metrics["output_tokens"].as_u64(),
            ..Default::default()
        };
        if let Some(previous) = self.usage.take() {
            let sum = |left: Option<u64>, right: Option<u64>| left?.checked_add(right?);
            usage.input = sum(previous.input, usage.input);
            usage.cached_input = sum(previous.cached_input, usage.cached_input);
            usage.output = sum(previous.output, usage.output);
        }
        self.usage = Some(usage);
    }
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn draft(id: String, ts: Option<i64>, kind: ItemKind) -> ItemDraft {
    ItemDraft {
        id,
        ts,
        turn_id: None,
        kind,
    }
}

#[test]
fn sqlite_background_launch_and_receipt_keep_the_linker_evidence() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../tests/fixtures/devin_background.sql"))
        .unwrap();
    let history = read_session(&db, "fixture-parent").unwrap().unwrap();
    let calls: Vec<_> = history
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::ToolCall(tool) => Some(tool),
            _ => None,
        })
        .collect();
    let [launch, receipt] = calls.as_slice() else {
        panic!("expected launch and receipt")
    };
    assert_eq!(launch.kind, verre_proto::ToolKind::Execute);
    assert_eq!(launch.input.as_ref().unwrap()["workdir"], "/fixture");
    assert!(
        matches!(&launch.content[..], [ToolContent::Text { text }] if text.ends_with("fixture-shell"))
    );
    assert_eq!(receipt.input.as_ref().unwrap()["shell_id"], "fixture-shell");
    assert!(
        matches!(&receipt.content[..], [ToolContent::Text { text }] if text.starts_with("session_id=sess_fixture_child"))
    );
    assert!(matches!(
        &history.items.last().unwrap().kind,
        ItemKind::TurnEnd {
            duration_ms: Some(4000),
            usage: None,
            ..
        }
    ));
}

fn user_content(message: &Value) -> Vec<ContentBlock> {
    let blocks = &message["metadata"]["extensions"]["chisel/acp-content-blocks"];
    let mut content: Vec<_> = mapping::array(blocks)
        .filter_map(mapping::content)
        .collect();
    if content.is_empty() {
        if let Some(text) = text(&message["content"]) {
            content.push(ContentBlock::Text { text });
        }
    }
    for image in mapping::array(&message["images"]) {
        if let (Some(mime_type), Some(data)) =
            (text(&image["mime_type"]), text(&image["base64_data"]))
        {
            let block = ContentBlock::Image {
                mime_type,
                data: Some(data),
                uri: None,
            };
            if !content.contains(&block) {
                content.push(block);
            }
        }
    }
    content
}
