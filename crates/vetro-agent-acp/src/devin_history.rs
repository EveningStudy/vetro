use std::{collections::HashMap, env, path::PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use vetro_agent_api::{ItemDraft, Transcript};
use vetro_proto::{ContentBlock, ItemKind, ToolContent, ToolStatus};
use vetro_store_sqlite::open_readonly;

use crate::{mapping, SpawnSpec};

pub(crate) async fn read(spec: &SpawnSpec, id: &str) -> Option<Transcript> {
    let app_data = spec
        .env
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("APPDATA"))
        .map(|(_, value)| value.clone())
        .or_else(|| env::var_os("APPDATA"))?;
    let path = PathBuf::from(app_data).join("Devin/cli/sessions.db");
    if !path.is_absolute() {
        return None;
    }
    let id = id.to_owned();
    tokio::task::spawn_blocking(move || {
        let db = open_readonly(&path).ok()?;
        read_session(&db, &id).ok().flatten()
    })
    .await
    .ok()
    .flatten()
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
    let mut rows = nodes.query(rusqlite::params![id, head])?;
    while let Some(row) = rows.next()? {
        let node_id: i64 = row.get(0)?;
        let message = row.get_ref(2)?.as_str()?;
        let Ok(message) = serde_json::from_str::<Value>(message) else {
            continue;
        };
        let seconds: i64 = row.get(1)?;
        let ts = mapping::timestamp(&message["metadata"]["created_at"])
            .or_else(|| seconds.checked_mul(1000));
        match message["role"].as_str() {
            Some("user") if message["metadata"]["is_user_input"].as_bool() == Some(true) => {
                let content = user_content(&message);
                if !content.is_empty() {
                    transcript.items.push(draft(
                        format!("devin:user:{node_id}"),
                        ts,
                        ItemKind::UserMessage { content },
                    ));
                }
            }
            Some("assistant") => {
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
                if let Some((index, false)) = message["tool_call_id"]
                    .as_str()
                    .and_then(|call_id| tools.get(call_id))
                {
                    if let ItemKind::ToolCall(tool) = &mut transcript.items[*index].kind {
                        tool.status = ToolStatus::Completed;
                        if let Some(text) = text(&message["content"]) {
                            tool.content = vec![ToolContent::Text { text }];
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Some(transcript))
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
