use std::{collections::HashSet, io::Read};

use rusqlite::{Connection, OptionalExtension, Row};
use serde_json::json;
use vetro_agent_api::{codex_internal_source, codex_web_source, ItemDraft, Transcript};
use vetro_proto::{
    ContentBlock, ItemKind, ThreadUsage, TokenUsage, ToolCall, ToolContent, ToolKind, ToolStatus,
};
use vetro_store_antigravity::sqlite::open_readonly;

use super::*;
use crate::history::record_mark;

pub(super) fn inventory(root: &Path) -> Result<Inventory> {
    let mut inventory = Inventory::default();
    for (folder, archived) in [("sessions", false), ("archived_sessions", true)] {
        for (id, mut parts) in rollouts(&root.join(folder))? {
            parts.sort_unstable();
            let mut mark = parts
                .iter()
                .filter_map(|path| file_mark(path))
                .collect::<String>();
            mark.push_str(":source");
            if parts
                .iter()
                .any(|path| path.extension().is_some_and(|ext| ext == "zst"))
            {
                mark.push_str(":compressed");
            }
            insert_file(&mut inventory.files, id, &parts[0], mark, archived);
        }
    }
    let mut web_ids = HashSet::new();
    let state = database(root, "state_");
    if let Some(path) = state {
        match read_state(&path, &mut inventory.files, &mut web_ids) {
            Ok(()) => inventory.authoritative = true,
            Err(error) => inventory
                .errors
                .push(format!("{}: {error}", path.display())),
        }
    }
    if let Some(path) = database(root, "thread_history_") {
        if let Err(error) = read_projection_inventory(&path, &mut inventory.files) {
            inventory
                .errors
                .push(format!("{}: {error}", path.display()));
        }
    }
    for name in ["codex.db", "codex-dev.db"] {
        let path = root.join("sqlite").join(name);
        if path.is_file() {
            if let Err(error) = read_catalog(&path, &mut inventory.files) {
                inventory
                    .errors
                    .push(format!("{}: {error}", path.display()));
            }
        }
    }
    inventory.files.retain(|id, _| !web_ids.contains(id));
    for file in inventory.files.values_mut() {
        if file.record.as_ref().is_none_or(|record| {
            record.resumable || record.origin.as_deref() == Some("history projection")
        }) {
            file.mark.push_str(":spawn-activity");
        }
    }
    Ok(inventory)
}

fn read_state(
    path: &Path,
    files: &mut HashMap<String, NativeFile>,
    web_ids: &mut HashSet<String>,
) -> Result<()> {
    let conn = open_readonly(path).map_err(internal)?;
    let mut stmt = conn.prepare("SELECT * FROM threads").map_err(internal)?;
    let mut rows = stmt.query([]).map_err(internal)?;
    while let Some(row) = rows.next().map_err(internal)? {
        let id: String = row.get("id").map_err(internal)?;
        let source: String = row.get("source").map_err(internal)?;
        let src = serde_json::from_str(&source).unwrap_or(Value::String(source));
        if codex_web_source(&src) {
            web_ids.insert(id);
            continue;
        }
        let title = column(row, "name").or_else(|| column(row, "title"));
        let preview = column(row, "preview").or_else(|| column(row, "first_user_message"));
        let archived = row.get::<_, bool>("archived").unwrap_or_default();
        let rollout = column(row, "rollout_path").map(PathBuf::from);
        let origin = column(row, "originator");
        let parent = column(row, "attached_thread_parent_id").or_else(|| parent(&src));
        let record = SessionRecord {
            native_id: id.clone(),
            title: title
                .or_else(|| preview.clone())
                .unwrap_or_else(|| "Codex thread".into()),
            preview,
            cwd: column(row, "cwd").map(PathBuf::from),
            created_at: times(row, "created_at_ms", "created_at"),
            updated_at: times(row, "updated_at_ms", "updated_at").unwrap_or_default(),
            source: source_kind(&src, origin.as_deref()),
            origin: origin.or_else(|| src.as_str().map(str::to_owned)),
            model: column(row, "model"),
            resumable: true,
            parent_native_id: parent,
            internal: codex_internal_source(&src),
        }
        .classify("codex");
        let file = files.remove(&id);
        let path = file.as_ref().and_then(|file| file.path.clone()).or(rollout);
        let disk_mark = file.map(|file| file.mark).unwrap_or_default();
        let mark = format!(
            "{archived}{}{disk_mark}{}",
            record_mark(&record),
            path.as_deref().and_then(file_mark).unwrap_or_default()
        );
        files.insert(
            id,
            NativeFile {
                record: Some(record),
                path,
                mark,
                archived,
            },
        );
    }
    Ok(())
}

fn read_projection_inventory(path: &Path, files: &mut HashMap<String, NativeFile>) -> Result<()> {
    let conn = open_readonly(path).map_err(internal)?;
    let mut stmt = conn
        .prepare("SELECT thread_id, next_rollout_ordinal FROM thread_history_projection_state")
        .map_err(internal)?;
    let mut rows = stmt.query([]).map_err(internal)?;
    while let Some(row) = rows.next().map_err(internal)? {
        let id: String = row.get(0).map_err(internal)?;
        let ordinal: i64 = row.get(1).map_err(internal)?;
        if let Some(file) = files.get_mut(&id) {
            file.mark.push_str(&format!(":{ordinal}"));
            continue;
        }
        let span = conn.query_row(
                "SELECT min(started_at), max(coalesce(completed_at,started_at)), count(*) FROM thread_turns WHERE thread_id=?1",
                [&id], |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, i64>(2)?))
            ).map_err(internal)?;
        if span.2 == 0 {
            continue;
        }
        let mut record = blank(id.clone(), path, "Codex thread");
        record.created_at = span.0;
        record.updated_at = span.1.unwrap_or_default();
        record.origin = Some("history projection".into());
        files.insert(
            id,
            NativeFile {
                record: Some(record.classify("codex")),
                mark: format!("projection:{ordinal}"),
                archived: false,
                path: None,
            },
        );
    }
    Ok(())
}

fn read_catalog(path: &Path, files: &mut HashMap<String, NativeFile>) -> Result<()> {
    let conn = open_readonly(path).map_err(internal)?;
    let exists = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='local_thread_catalog')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(internal)?;
    if !exists {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "SELECT thread_id, display_title, source_created_at, source_updated_at, cwd
             FROM local_thread_catalog
             WHERE missing_candidate=0 AND host_id='local' AND source_kind != 'chatgpt'
             ORDER BY source_updated_at DESC",
        )
        .map_err(internal)?;
    let mut rows = stmt.query([]).map_err(internal)?;
    while let Some(row) = rows.next().map_err(internal)? {
        let id: String = row.get(0).map_err(internal)?;
        if files.contains_key(&id) {
            continue;
        }
        let title: String = row.get(1).map_err(internal)?;
        let created: f64 = row.get(2).map_err(internal)?;
        let updated: f64 = row.get(3).map_err(internal)?;
        let raw_cwd: Option<String> = row.get(4).map_err(internal)?;
        let record = SessionRecord {
            native_id: id.clone(),
            title,
            preview: None,
            cwd: raw_cwd.map(PathBuf::from),
            created_at: Some((created * 1000.0) as i64),
            updated_at: (updated * 1000.0) as i64,
            source: ThreadSource::Desktop,
            origin: Some("Codex desktop catalog (local)".into()),
            model: None,
            resumable: false,
            parent_native_id: None,
            internal: false,
        }
        .classify("codex");
        files.insert(
            id,
            NativeFile {
                mark: record_mark(&record),
                record: Some(record),
                path: None,
                archived: false,
            },
        );
    }
    Ok(())
}

fn database(root: &Path, prefix: &str) -> Option<PathBuf> {
    paths(root)
        .ok()?
        .into_iter()
        .filter_map(|path| {
            let version = path
                .file_name()?
                .to_str()?
                .strip_prefix(prefix)?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            Some((version, path))
        })
        .max()
        .map(|(_, path)| path)
}

fn column(row: &Row<'_>, name: &str) -> Option<String> {
    row.get::<_, Option<String>>(name)
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
}

fn times(row: &Row<'_>, ms: &str, seconds: &str) -> Option<i64> {
    row.get::<_, Option<i64>>(ms).ok().flatten().or_else(|| {
        row.get::<_, i64>(seconds)
            .ok()
            .map(|seconds| seconds.saturating_mul(1000))
    })
}

fn rollouts(dir: &Path) -> Result<HashMap<String, Vec<PathBuf>>> {
    let mut files: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut dirs = vec![dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for path in paths(&dir)? {
            if path.is_symlink() {
                continue;
            }
            if path.is_dir() {
                dirs.push(path);
            } else if let Some(id) = rollout_id(&path) {
                files.entry(id.to_owned()).or_default().push(path);
            }
        }
    }
    Ok(files)
}

fn rollout_id(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?.strip_prefix("rollout-")?;
    let name = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    name.get(20..56).filter(|id| uuid(id))
}

pub(super) fn rollout_record(path: &Path, id: String) -> Result<Option<SessionRecord>> {
    let mut record = blank(id, path, "Codex thread");
    record.resumable = true;
    for row in json_lines(path)?.take(64) {
        let payload = &row["payload"];
        match row["type"].as_str() {
            Some("session_meta") => {
                if codex_web_source(&payload["source"]) {
                    return Ok(None);
                }
                record.origin = string(payload, "originator");
                record.source = source_kind(&payload["source"], record.origin.as_deref());
                record.internal = codex_internal_source(&payload["source"]);
                record.parent_native_id = parent(&payload["source"]);
                record.created_at =
                    timestamp(&payload["timestamp"]).or_else(|| timestamp(&row["timestamp"]));
                record.cwd = string(payload, "cwd").map(PathBuf::from);
            }
            Some("event_msg") if payload["type"] == "user_message" => {
                record.preview = first_text(&payload["message"]);
                if let Some(preview) = &record.preview {
                    record.title = preview
                        .lines()
                        .next()
                        .unwrap_or(preview)
                        .chars()
                        .take(100)
                        .collect();
                }
                break;
            }
            _ => {}
        }
    }
    Ok(Some(record.classify("codex")))
}

fn source_kind(source: &Value, origin: Option<&str>) -> ThreadSource {
    match (
        source.as_str(),
        origin.unwrap_or_default().to_ascii_lowercase().as_str(),
    ) {
        (_, "vetro") => ThreadSource::Vetro,
        (Some("exec"), _) | (_, "codex_exec") => ThreadSource::Exec,
        (
            _,
            "codex desktop" | "codex_desktop" | "codex_app" | "codex app" | "codex_work_desktop",
        ) => ThreadSource::Desktop,
        (Some("cli"), _) | (_, "codex_cli_rs") => ThreadSource::Cli,
        (Some("vscode"), _) | (_, "codex_vscode") => ThreadSource::Ide,
        _ => ThreadSource::Unknown,
    }
}

fn parent(source: &Value) -> Option<String> {
    string(&source["subagent"]["thread_spawn"], "parent_thread_id")
        .or_else(|| string(&source["subAgent"]["threadSpawn"], "parentThreadId"))
}

fn json_lines(path: &Path) -> Result<impl Iterator<Item = Value>> {
    let file = File::open(path).map_err(internal)?;
    let reader: Box<dyn Read> = if path.extension().is_some_and(|ext| ext == "zst") {
        Box::new(zstd::Decoder::new(file).map_err(internal)?)
    } else {
        Box::new(file)
    };
    Ok(BufReader::new(reader)
        .split(b'\n')
        .map_while(std::io::Result::ok)
        .filter_map(|line| serde_json::from_slice::<Value>(&line).ok()))
}

pub(in crate::history) fn fallback(root: &Path, id: &str) -> Result<Option<Transcript>> {
    let mut parts = Vec::new();
    for folder in ["sessions", "archived_sessions"] {
        parts.extend(rollouts(&root.join(folder))?.remove(id).unwrap_or_default());
    }
    parts.sort_unstable();
    let compressed = parts
        .iter()
        .any(|path| path.extension().is_some_and(|ext| ext == "zst"));
    let rollout = if compressed {
        let transcript = read_rollouts(&parts, id, true)?;
        if !transcript.items.is_empty() {
            return Ok(Some(transcript));
        }
        Some(transcript)
    } else {
        None
    };
    let mut projection_error = None;
    let mut projection = None;
    if let Some(path) = database(root, "thread_history_") {
        match open_readonly(&path)
            .map_err(internal)
            .and_then(|conn| read_projection(&conn, id))
        {
            Ok(transcript) if !transcript.items.is_empty() => projection = Some(transcript),
            Ok(_) => {}
            Err(error) => projection_error = Some(error),
        }
    }
    let rollout = rollout.map_or_else(
        || {
            if parts.is_empty() {
                parts.extend(state_rollout(root, id)?);
            }
            read_rollouts(&parts, id, projection.is_none())
        },
        Ok,
    );
    if let Some(mut transcript) = projection {
        if let Ok(rollout) = rollout {
            transcript.usage = rollout.usage;
        }
        return Ok(Some(transcript));
    }
    let transcript = rollout?;
    if !transcript.items.is_empty() || transcript.usage != ThreadUsage::default() {
        return Ok(Some(transcript));
    }
    projection_error.map_or(Ok(None), Err)
}

fn state_rollout(root: &Path, id: &str) -> Result<Option<PathBuf>> {
    let Some(path) = database(root, "state_") else {
        return Ok(None);
    };
    let conn = open_readonly(&path).map_err(internal)?;
    let path = conn
        .query_row(
            "SELECT rollout_path FROM threads WHERE id=?1",
            [id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(internal)?
        .map(PathBuf::from);
    Ok(path.filter(|path| path.is_file() && rollout_id(path) == Some(id)))
}

fn read_rollouts(parts: &[PathBuf], id: &str, include_items: bool) -> Result<Transcript> {
    let mut items: Vec<ItemDraft> = Vec::new();
    let mut positions = HashMap::new();
    let mut usage = ThreadUsage::default();
    for path in parts {
        let part = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        for (index, row) in json_lines(path)?.enumerate() {
            let payload = &row["payload"];
            if row["type"] == "event_msg" && payload["type"] == "token_count" {
                if let Some(snapshot) = rollout_usage(&payload["info"]) {
                    usage = snapshot;
                }
                continue;
            }
            if !include_items {
                continue;
            }
            let ts = timestamp(&row["timestamp"]);
            let kind = match payload["type"].as_str() {
                Some("item_started" | "item_completed") if row["type"] == "event_msg" => {
                    if let Some(item) = rollout_item(payload, ts) {
                        upsert_rollout_item(&mut items, &mut positions, item);
                    }
                    continue;
                }
                Some("user_message") if row["type"] == "event_msg" => ItemKind::UserMessage {
                    content: string(payload, "message")
                        .into_iter()
                        .map(|text| ContentBlock::Text { text })
                        .collect(),
                },
                Some("agent_message") if row["type"] == "event_msg" => ItemKind::AgentMessage {
                    text: string(payload, "message").unwrap_or_default(),
                    streaming: false,
                },
                Some("exec_command_begin" | "exec_command_end") => {
                    let Some(id) = string(payload, "call_id") else {
                        continue;
                    };
                    let command = payload["command"]
                        .as_array()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .or_else(|| string(payload, "command"));
                    let raw = json!({
                        "id": id, "type": "commandExecution", "command": command,
                        "cwd": payload["cwd"], "aggregatedOutput": payload["aggregated_output"],
                        "exitCode": payload["exit_code"],
                        "status": if payload["type"] == "exec_command_begin" { "inProgress" } else { "completed" },
                    });
                    if let Some(item) = projected_item(&raw, ts, string(payload, "turn_id")) {
                        upsert_rollout_item(&mut items, &mut positions, item);
                    }
                    continue;
                }
                _ => continue,
            };
            items.push(ItemDraft {
                id: format!("{id}:{part}:{index}"),
                ts,
                turn_id: None,
                kind,
            });
        }
    }
    Ok(Transcript { items, usage })
}

fn rollout_usage(info: &Value) -> Option<ThreadUsage> {
    let window = info["model_context_window"]
        .as_u64()
        .filter(|window| *window > 0);
    let tokens = |raw: &Value, active_context: bool| {
        let input = raw["input_tokens"].as_u64();
        let output = raw["output_tokens"].as_u64();
        let mut usage = TokenUsage {
            input,
            cached_input: raw["cached_input_tokens"].as_u64(),
            output,
            reasoning: raw["reasoning_output_tokens"].as_u64(),
            context_used: if active_context {
                raw["total_tokens"]
                    .as_u64()
                    .or_else(|| Some(input?.saturating_add(output?)))
            } else {
                None
            },
            context_window: None,
        };
        (usage != TokenUsage::default()).then(|| {
            usage.context_window = window;
            usage
        })
    };
    let last_turn = tokens(&info["last_token_usage"], true);
    let total = tokens(&info["total_token_usage"], false);
    (last_turn.is_some() || total.is_some()).then_some(ThreadUsage {
        last_turn,
        total,
        ..ThreadUsage::default()
    })
}

fn rollout_item(event: &Value, ts: Option<i64>) -> Option<ItemDraft> {
    let item = &event["item"];
    let native_id = string(item, "id")?;
    let done = event["type"] == "item_completed";
    let mapped = match item["type"].as_str()? {
        "UserMessage" if done => json!({
            "id": native_id, "type": "userMessage", "content": item["content"],
        }),
        "AgentMessage" if done => {
            let text = text_blocks(&item["content"]).collect::<Vec<_>>().join("\n");
            json!({ "id": native_id, "type": "agentMessage", "text": text })
        }
        "CommandExecution" => json!({
            "id": native_id, "type": "commandExecution", "command": item["command"],
            "cwd": item["cwd"], "aggregatedOutput": item["aggregated_output"], "exitCode": item["exit_code"],
            "status": if done { item["status"].as_str().unwrap_or("completed") } else { "inProgress" },
            "completedAtMs": if done { ts } else { None },
        }),
        _ => return None,
    };
    projected_item(&mapped, ts, string(event, "turn_id"))
}

fn upsert_rollout_item(
    items: &mut Vec<ItemDraft>,
    positions: &mut HashMap<String, usize>,
    mut item: ItemDraft,
) {
    if let Some(previous) = positions.get(&item.id).map(|index| &mut items[*index]) {
        let ts = item.ts;
        item.ts = previous.ts.or(ts);
        if let ItemKind::ToolCall(tool) = &mut item.kind {
            if let ItemKind::ToolCall(old) = &previous.kind {
                if let (
                    Some(ToolContent::Terminal { command, cwd, .. }),
                    Some(ToolContent::Terminal {
                        command: old_command,
                        cwd: old_cwd,
                        ..
                    }),
                ) = (tool.content.first_mut(), old.content.first())
                {
                    if command.is_none() {
                        command.clone_from(old_command);
                    }
                    if cwd.is_none() {
                        cwd.clone_from(old_cwd);
                    }
                }
            }
            if tool.status != ToolStatus::InProgress {
                tool.input = Some(json!({ "completedAtMs": ts }));
            }
        }
        *previous = item;
    } else {
        positions.insert(item.id.clone(), items.len());
        items.push(item);
    }
}

fn text_blocks(value: &Value) -> impl Iterator<Item = &str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
}

fn read_projection(conn: &Connection, id: &str) -> Result<Transcript> {
    let mut stmt = conn
        .prepare(
            "SELECT item_json, turn_id, coalesce(started_at_ms,created_at_ms), completed_at_ms
         FROM thread_items WHERE thread_id=?1 ORDER BY rollout_ordinal",
        )
        .map_err(internal)?;
    let mut rows = stmt.query([id]).map_err(internal)?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().map_err(internal)? {
        let raw = row
            .get_ref(0)
            .and_then(|value| Ok(value.as_str()?))
            .map_err(internal)?;
        let Ok(mut raw) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if let Some(completed) = row.get::<_, Option<i64>>(3).map_err(internal)? {
            raw["completedAtMs"] = json!(completed);
        }
        if let Some(item) = projected_item(
            &raw,
            row.get(2).map_err(internal)?,
            row.get(1).map_err(internal)?,
        ) {
            items.push(item);
        }
    }
    Ok(Transcript {
        items,
        ..Transcript::default()
    })
}

fn projected_item(raw: &Value, ts: Option<i64>, turn_id: Option<String>) -> Option<ItemDraft> {
    let id = string(raw, "id")?;
    let kind = match raw["type"].as_str()? {
        "userMessage" => ItemKind::UserMessage {
            content: raw["content"]
                .as_array()?
                .iter()
                .filter_map(|part| string(part, "text").map(|text| ContentBlock::Text { text }))
                .collect(),
        },
        "agentMessage" => ItemKind::AgentMessage {
            text: string(raw, "text")?,
            streaming: false,
        },
        "reasoning" => ItemKind::Reasoning {
            text: raw["summary"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            streaming: false,
        },
        "subAgentActivity" => ItemKind::ToolCall(ToolCall {
            call_id: id.clone(),
            kind: ToolKind::Agent,
            name: None,
            title: string(raw, "kind")?,
            status: ToolStatus::Completed,
            input: None,
            output: Some(json!({ "agentThreadId": raw["agentThreadId"] })),
            content: Vec::new(),
            locations: Vec::new(),
            child_thread_ids: Vec::new(),
        }),
        "commandExecution" | "collabAgentToolCall" => {
            let command = string(raw, "command");
            let collab = raw["type"] == "collabAgentToolCall";
            ItemKind::ToolCall(ToolCall {
                call_id: id.clone(),
                kind: if collab {
                    ToolKind::Agent
                } else {
                    ToolKind::Execute
                },
                name: string(raw, "tool"),
                title: command.clone().unwrap_or_else(|| "Codex tool".into()),
                status: match raw["status"].as_str() {
                    Some("inProgress" | "in_progress") => ToolStatus::InProgress,
                    Some("failed" | "declined") => ToolStatus::Failed,
                    _ => ToolStatus::Completed,
                },
                input: Some(
                    json!({ "prompt": raw["prompt"], "completedAtMs": raw["completedAtMs"] }),
                ),
                output: collab.then(|| json!({ "receiverThreadIds": raw["receiverThreadIds"] })),
                content: if collab {
                    Vec::new()
                } else {
                    vec![ToolContent::Terminal {
                        command,
                        cwd: string(raw, "cwd"),
                        output: string(raw, "aggregatedOutput").unwrap_or_default(),
                        exit_code: raw["exitCode"]
                            .as_i64()
                            .and_then(|code| i32::try_from(code).ok()),
                    }]
                },
                locations: Vec::new(),
                child_thread_ids: Vec::new(),
            })
        }
        _ => return None,
    };
    Some(ItemDraft {
        id,
        ts,
        turn_id,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_only_present_local_coding_catalog_rows() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        let catalog = Connection::open(&path).unwrap();
        catalog
            .execute_batch(
                "CREATE TABLE local_thread_catalog(thread_id, display_title, source_created_at,
             source_updated_at, cwd, host_id, source_kind, missing_candidate);
             INSERT INTO local_thread_catalog VALUES
                ('local','Local',1.0,2.0,'/project','local','cli',0),
                ('local-web','Web',1.0,2.0,NULL,'local','chatgpt',0),
                ('web','Web',1.0,2.0,NULL,'chatgpt','chatgpt',0),
                ('remote','Remote',1.0,2.0,'/private/tmp/vetro-probe','remote-control:mac','cli',0),
                ('missing','Missing',1.0,2.0,NULL,'local','cli',1)",
            )
            .unwrap();
        let mut files = HashMap::new();
        read_catalog(&path, &mut files).unwrap();
        assert_eq!(files.len(), 1);
        let record = files["local"].record.as_ref().unwrap();
        assert_eq!(record.source, ThreadSource::Desktop);
        assert_eq!(record.cwd.as_deref(), Some(Path::new("/project")));
        assert!(!record.resumable);
    }

    #[test]
    fn web_state_records_cannot_be_reintroduced_by_projections() {
        let root = tempfile::tempdir().unwrap();
        let state = Connection::open(root.path().join("state_1.sqlite")).unwrap();
        state
            .execute_batch(
                "CREATE TABLE threads(id, source);
             INSERT INTO threads VALUES ('local','cli'), ('web','chatgpt'), ('cloud','\"cloud\"');",
            )
            .unwrap();
        let history = Connection::open(root.path().join("thread_history_1.sqlite")).unwrap();
        history
            .execute_batch(
                "CREATE TABLE thread_history_projection_state(thread_id, next_rollout_ordinal);
             INSERT INTO thread_history_projection_state VALUES ('web',1), ('projected',1);
             CREATE TABLE thread_turns(thread_id, started_at, completed_at);
             INSERT INTO thread_turns VALUES ('web',1,2), ('projected',1,2);",
            )
            .unwrap();
        let result = inventory(root.path()).unwrap();
        assert!(result.authoritative);
        assert!(result.errors.is_empty());
        assert_eq!(result.files.len(), 2);
        assert!(result.files.contains_key("local"));
        assert!(result.files.contains_key("projected"));
    }

    #[test]
    fn bad_databases_do_not_hide_records_from_healthy_sources() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        fs::create_dir(path.join("sqlite")).unwrap();
        for name in [
            "state_1.sqlite",
            "thread_history_1.sqlite",
            "sqlite/codex.db",
        ] {
            fs::write(path.join(name), "corrupt").unwrap();
        }
        let catalog = Connection::open(path.join("sqlite/codex-dev.db")).unwrap();
        catalog.execute_batch(
            "CREATE TABLE local_thread_catalog(thread_id, display_title, source_created_at,
             source_updated_at, cwd, host_id, source_kind, missing_candidate);
             INSERT INTO local_thread_catalog VALUES ('healthy','Desktop',1.0,2.0,NULL,'local','cli',0)"
        ).unwrap();
        let result = inventory(path).unwrap();
        assert_eq!(result.errors.len(), 3);
        assert!(!result.authoritative);
        assert_eq!(
            result.files["healthy"].record.as_ref().unwrap().title,
            "Desktop"
        );
    }

    #[test]
    fn reads_all_segments_of_a_compressed_rollout() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let id = "01a07c95-7d45-7983-a759-ad414c92947e";
        for (clock, suffix, text) in [
            ("00-00-00", "", "first segment"),
            (
                "00-05-00",
                "_01a07cb2-cb63-7610-b7ff-ea388523104b",
                "second segment",
            ),
        ] {
            let path = sessions.join(format!("rollout-2026-09-07T{clock}-{id}{suffix}.jsonl.zst"));
            let body = if suffix.is_empty() {
                json!({"type": "event_msg", "payload": {"type": "user_message", "message": text}})
            } else {
                json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {
                    "type": "UserMessage", "id": "message", "content": [{"type": "text", "text": text}],
                }}})
            };
            let mut line = serde_json::to_vec(&body).unwrap();
            line.push(b'\n');
            let compressed = zstd::encode_all(line.as_slice(), 1).unwrap();
            fs::write(path, compressed).unwrap();
        }
        assert_eq!(inventory(root.path()).unwrap().files.len(), 1);
        let transcript = fallback(root.path(), id).unwrap().unwrap();
        assert_eq!(transcript.items.len(), 2);
        assert_ne!(transcript.items[0].id, transcript.items[1].id);
        let actual: Vec<_> = transcript
            .items
            .into_iter()
            .filter_map(|item| {
                let ItemKind::UserMessage { content } = item.kind else {
                    return None;
                };
                match content.into_iter().next() {
                    Some(ContentBlock::Text { text }) => Some(text),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(actual, ["first segment", "second segment"]);
    }

    #[test]
    fn reads_latest_token_count_with_rollout_or_projection_items() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let id = "01a07c95-7d45-7983-a759-ad414c92947e";
        fs::write(
            sessions.join(format!("rollout-2026-09-07T00-00-00-{id}.jsonl")),
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"rollout"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":9},"total_token_usage":{"input_tokens":9},"model_context_window":200000}}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":42,"cached_input_tokens":12,"output_tokens":8,"reasoning_output_tokens":3,"total_tokens":50},"total_token_usage":{"input_tokens":100,"cached_input_tokens":24,"output_tokens":20,"reasoning_output_tokens":6,"total_tokens":120},"model_context_window":828400}}}
{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{}}}
"#,
        )
        .unwrap();

        let transcript = fallback(root.path(), id).unwrap().unwrap();
        assert_eq!(transcript.items.len(), 1);
        assert_eq!(
            transcript.usage,
            ThreadUsage {
                last_turn: Some(TokenUsage {
                    input: Some(42),
                    cached_input: Some(12),
                    output: Some(8),
                    reasoning: Some(3),
                    context_used: Some(50),
                    context_window: Some(828_400),
                }),
                total: Some(TokenUsage {
                    input: Some(100),
                    cached_input: Some(24),
                    output: Some(20),
                    reasoning: Some(6),
                    context_used: None,
                    context_window: Some(828_400),
                }),
                rate_limits: Vec::new(),
            }
        );

        let projection = Connection::open(root.path().join("thread_history_1.sqlite")).unwrap();
        projection
            .execute_batch(
                "CREATE TABLE thread_items(thread_id, rollout_ordinal, item_json, turn_id,
             started_at_ms, created_at_ms, completed_at_ms)",
            )
            .unwrap();
        let item = json!({"id": "projected", "type": "agentMessage", "text": "from projection"});
        projection
            .execute(
                "INSERT INTO thread_items VALUES (?1,1,?2,'turn',NULL,0,NULL)",
                (id, item.to_string()),
            )
            .unwrap();
        let projected = fallback(root.path(), id).unwrap().unwrap();
        assert_eq!(projected.items[0].id, "projected");
        assert_eq!(projected.usage, transcript.usage);
    }
}
