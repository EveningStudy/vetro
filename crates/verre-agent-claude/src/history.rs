use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use memchr::memmem;
use serde_json::Value;
use verre_agent_api::{AgentError, ItemDraft, Result, SessionEvent, SessionRecord, Transcript};
use verre_proto::{rpc::ErrorCode, ContentBlock, ThreadSource, ThreadUsage, TokenUsage};

use crate::{
    bad_request,
    events::{injected_user, synthetic_user, timestamp, user_content, Timeline},
    io_error,
    session::{accumulate_usage, add_tokens, token_usage},
    string,
};

pub(crate) fn validate_id(id: &str) -> Result<()> {
    let valid = id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if valid {
        Ok(())
    } else {
        Err(bad_request("Invalid Claude session ID"))
    }
}

pub(crate) fn list(root: &Path) -> Result<Vec<SessionRecord>> {
    let mut records = HashMap::<String, SessionRecord>::new();
    for path in session_files(root, None).map_err(io_error)? {
        let id = path.file_stem().unwrap().to_string_lossy();
        if validate_id(&id).is_err() {
            continue;
        }
        let Some(record) = open_optional(&path)
            .and_then(|file| file.map(|file| record(file, &id)).transpose())
            .map_err(io_error)?
        else {
            continue;
        };
        if record.cwd.is_none() && record.preview.is_none() && record.created_at.is_none() {
            continue;
        }
        if records
            .get(&*id)
            .is_none_or(|existing| existing.updated_at < record.updated_at)
        {
            records.insert(id.into_owned(), record);
        }
    }
    let mut records: Vec<_> = records.into_values().collect();
    records.sort_unstable_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.native_id.cmp(&right.native_id))
    });
    Ok(records)
}

pub(crate) fn original_cwd(root: &Path, id: &str) -> Result<Option<PathBuf>> {
    let Some(path) = locate(root, id)? else {
        return Ok(None);
    };
    let Some(file) = open_optional(&path).map_err(io_error)? else {
        return Ok(None);
    };
    let mut cwd = None;
    scan(file, |line| {
        if has_type(line, |kind| {
            kind == b"relocated" || cwd.is_none() && matches!(kind, b"user" | b"assistant")
        }) {
            if let Ok(row) = serde_json::from_slice::<Value>(line) {
                if let Some(path) = string(&row, "relocatedCwd") {
                    cwd = Some(path.into());
                } else if cwd.is_none() && row["isSidechain"] != true {
                    cwd = string(&row, "cwd").map(PathBuf::from);
                }
            }
        }
    })
    .map_err(io_error)?;
    Ok(cwd)
}

pub(crate) fn read(root: &Path, id: &str) -> Result<Transcript> {
    let path = locate(root, id)?.ok_or_else(not_found)?;
    let file = open_optional(&path)
        .map_err(io_error)?
        .ok_or_else(not_found)?;
    let mut transcript = HistoryTranscript {
        timeline: Timeline::for_history(),
        ..HistoryTranscript::default()
    };
    transcript.read(file, None).map_err(io_error)?;
    let sidechains = path.with_extension("").join("subagents");
    let mut agents = Vec::new();
    for sidechain in paths(&sidechains).map_err(io_error)? {
        let Some(name) = sidechain
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| name.starts_with("agent-") && is_jsonl(&sidechain))
        else {
            continue;
        };
        let meta = read_sidechain_meta(&sidechain.with_extension("meta.json")).map_err(io_error)?;
        let parent = string(&meta, "toolUseId")
            .map(str::to_owned)
            .or_else(|| {
                transcript
                    .agent_calls
                    .get(name.trim_start_matches("agent-"))
                    .cloned()
            })
            .unwrap_or_else(|| format!("subagent:{name}"));
        let depth = meta["spawnDepth"].as_u64().unwrap_or(1);
        agents.push((depth, sidechain, parent));
    }
    agents.sort_unstable();
    for (_, sidechain, parent) in agents {
        if let Some(file) = open_optional(&sidechain).map_err(io_error)? {
            transcript.read(file, Some(&parent)).map_err(io_error)?;
        }
    }
    let usage = transcript.usage.finish();
    Ok(Transcript {
        items: transcript.items,
        usage,
    })
}

fn read_sidechain_meta(path: &Path) -> io::Result<Value> {
    let Some(file) = open_optional(path)? else {
        return Ok(Value::Null);
    };
    Ok(serde_json::from_reader(BufReader::new(file)).unwrap_or(Value::Null))
}

#[derive(Default)]
struct HistoryTranscript {
    timeline: Timeline,
    positions: HashMap<String, usize>,
    items: Vec<ItemDraft>,
    agent_calls: HashMap<String, String>,
    sidechain_parents: HashMap<String, String>,
    seen: HashSet<String>,
    usage: HistoryUsage,
}

impl HistoryTranscript {
    fn read(&mut self, file: File, parent: Option<&str>) -> io::Result<()> {
        let mut events = Vec::new();
        let turn_at_open = self.timeline.turn.clone();
        self.timeline.turn = parent
            .and_then(|parent| self.timeline.tool_turn(parent))
            .or_else(|| {
                if parent.is_none() {
                    turn_at_open.clone()
                } else {
                    None
                }
            });
        scan(file, |line| {
            if !has_type(line, |kind| {
                matches!(kind, b"assistant" | b"user" | b"system" | b"result")
            }) {
                if !has_type(line, |kind| {
                    matches!(
                        kind,
                        b"file-history-snapshot"
                            | b"queue-operation"
                            | b"progress"
                            | b"summary"
                            | b"custom-title"
                            | b"ai-title"
                            | b"last-prompt"
                            | b"tools-used"
                            | b"derived-from"
                            | b"relocated"
                            | b"session-instructions"
                            | b"agent-name"
                            | b"agent-color"
                    )
                }) {
                    if let Ok(row) = serde_json::from_slice::<Value>(line) {
                        verre_agent_api::drift::report(
                            "claude",
                            "history row",
                            string(&row, "type"),
                        );
                    }
                }
                return;
            }
            let Ok(row) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            let id = string(&row, "uuid");
            if id.is_some_and(|id| !self.seen.insert(id.into())) {
                return;
            }
            self.index_agent_calls(&row);
            let row_parent = string(&row, "parent_tool_use_id")
                .or_else(|| string(&row, "parentToolUseId"))
                .or_else(|| string(&row, "parentToolUseID"))
                .or(parent)
                .map(str::to_owned)
                .or_else(|| {
                    if row["isSidechain"] != true {
                        return None;
                    }
                    string(&row, "agentId")
                        .and_then(|agent| self.agent_calls.get(agent).cloned())
                        .or_else(|| {
                            string(&row, "parentUuid")
                                .and_then(|uuid| self.sidechain_parents.get(uuid).cloned())
                        })
                        .or_else(|| {
                            string(&row, "agentId")
                                .or(id)
                                .map(|id| format!("subagent:{id}"))
                        })
                });
            if let (Some(id), Some(parent)) = (id, &row_parent) {
                self.sidechain_parents.insert(id.into(), parent.clone());
            }
            let main = row_parent.is_none() && row["isSidechain"] != true;
            match string(&row, "type") {
                Some("assistant" | "user") => {
                    if row["type"] == "assistant" {
                        self.usage.assistant(&row, main);
                    }
                    self.timeline
                        .message(&row, row_parent.as_deref(), &mut events)
                }
                Some("system") => self
                    .timeline
                    .notice(&row, row_parent.as_deref(), &mut events),
                Some("result") if main => self.usage.result(&row),
                _ => {}
            }
            self.apply(&mut events);
        })?;
        if parent.is_none() {
            self.timeline.finish_history_turn(&mut events);
            self.apply(&mut events);
        } else {
            self.timeline.turn = turn_at_open;
        }
        Ok(())
    }

    fn apply(&mut self, events: &mut Vec<SessionEvent>) {
        for event in events.drain(..) {
            if let SessionEvent::Item(item) = event {
                if let Some(position) = self.positions.get(&item.id) {
                    self.items[*position] = item;
                } else {
                    self.positions.insert(item.id.clone(), self.items.len());
                    self.items.push(item);
                }
            }
        }
    }

    fn index_agent_calls(&mut self, row: &Value) {
        let native = row
            .get("toolUseResult")
            .or_else(|| row.get("tool_use_result"));
        let Some(agent) = native.and_then(|native| string(native, "agentId")) else {
            return;
        };
        for result in row["message"]["content"].as_array().into_iter().flatten() {
            if result["type"] == "tool_result" {
                if let Some(call) = string(result, "tool_use_id") {
                    self.agent_calls.insert(agent.into(), call.into());
                }
            }
        }
    }
}

#[derive(Default)]
struct HistoryUsage {
    by_message: HashMap<String, TokenUsage>,
    without_id: TokenUsage,
    last: Option<TokenUsage>,
    model: Option<String>,
}

impl HistoryUsage {
    fn assistant(&mut self, row: &Value, main: bool) {
        let message = &row["message"];
        let model = string(message, "model");
        if row["isSynthetic"] == true || model.is_some_and(|model| model.starts_with('<')) {
            return;
        }
        let raw = &message["usage"];
        let mut usage = token_usage(raw);
        if usage == TokenUsage::default() {
            return;
        }
        if main {
            usage.context_used = add_tokens(usage.input, usage.output);
            usage.context_window = [raw, message, row]
                .into_iter()
                .find_map(context_window)
                .or_else(|| model.and_then(|model| default_context_window(model, usage.input)));
            self.last = Some(usage.clone());
            self.model = model.map(str::to_owned);
        }
        if let Some(id) = string(message, "id") {
            self.by_message.insert(id.into(), usage);
        } else {
            self.without_id = accumulate_usage(&self.without_id, &usage);
        }
    }

    fn result(&mut self, row: &Value) {
        let window = context_window(row).or_else(|| {
            let model = self.model.as_deref()?;
            context_window(&row["modelUsage"][model])
        });
        if let (Some(last), Some(window)) = (&mut self.last, window) {
            last.context_window = Some(window);
        }
    }

    fn finish(self) -> ThreadUsage {
        let mut total = self
            .by_message
            .values()
            .fold(self.without_id, |total, usage| {
                accumulate_usage(&total, usage)
            });
        let total = (total != TokenUsage::default()).then(|| {
            total.context_window = self.last.as_ref().and_then(|last| last.context_window);
            total
        });
        ThreadUsage {
            last_turn: self.last,
            total,
            ..ThreadUsage::default()
        }
    }
}

fn context_window(value: &Value) -> Option<u64> {
    value["contextWindow"]
        .as_u64()
        .or_else(|| value["context_window"].as_u64())
        .filter(|window| *window > 0)
}

fn default_context_window(model: &str, input: Option<u64>) -> Option<u64> {
    match model {
        "claude-opus-4-6" | "claude-sonnet-4-6" => {
            Some(if input.is_some_and(|input| input > 200_000) {
                1_000_000
            } else {
                200_000
            })
        }
        "claude-opus-4-6[1m]"
        | "claude-sonnet-4-6[1m]"
        | "claude-opus-4-7"
        | "claude-opus-4-8"
        | "claude-opus-5"
        | "claude-opus-5-5"
        | "claude-sonnet-5"
        | "claude-sonnet-5-5"
        | "claude-haiku-5-5"
        | "claude-fable-5-2" => Some(1_000_000),
        "claude-opus-4-5-20251101" | "claude-sonnet-4-5-20250929" | "claude-haiku-4-5-20251001" => {
            Some(200_000)
        }
        _ => None,
    }
}

fn record(file: File, id: &str) -> io::Result<SessionRecord> {
    let updated_at = file
        .metadata()?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let mut record = SessionRecord {
        native_id: id.into(),
        title: String::new(),
        preview: None,
        cwd: None,
        created_at: None,
        updated_at,
        source: ThreadSource::Unknown,
        origin: None,
        model: None,
        resumable: true,
        parent_native_id: None,
        internal: false,
    };
    let mut title = None;
    let mut summary = None;
    let mut fallback_prompt = None;
    let mut last_activity = None;
    scan(file, |line| {
        if let Some(latest) = latest_timestamp(line) {
            last_activity = last_activity.max(Some(latest));
        }
        if !has_type(line, |kind| match kind {
            b"custom-title" | b"ai-title" | b"summary" | b"relocated" => true,
            b"last-prompt" => fallback_prompt.is_none(),
            b"user" => record.preview.is_none(),
            b"assistant" => record.model.is_none(),
            _ => false,
        }) {
            return;
        }
        let Ok(row) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        if row["isSidechain"] == true {
            return;
        }
        match string(&row, "type") {
            Some("custom-title") => title = nonempty(&row, "customTitle").or(title.take()),
            Some("ai-title") => summary = nonempty(&row, "aiTitle").or(summary.take()),
            Some("summary") => summary = nonempty(&row, "summary").or(summary.take()),
            Some("last-prompt") if fallback_prompt.is_none() => {
                fallback_prompt = nonempty(&row, "lastPrompt")
            }
            Some("relocated") => {
                if let Some(cwd) = string(&row, "relocatedCwd") {
                    record.cwd = Some(cwd.into());
                }
            }
            Some("user" | "assistant") => {
                record.cwd = record
                    .cwd
                    .take()
                    .or_else(|| string(&row, "cwd").map(PathBuf::from));
                record.created_at = record.created_at.or_else(|| timestamp(&row));
                if record.origin.is_none() {
                    record.origin = nonempty(&row, "entrypoint");
                }
                if row["type"] == "assistant" && record.model.is_none() {
                    record.model = string(&row["message"], "model")
                        .filter(|model| !model.starts_with('<'))
                        .map(str::to_owned);
                }
                if record.preview.is_none() && row["type"] == "user" && !synthetic_user(&row) {
                    record.preview = first_prompt(&row["message"]["content"]);
                }
            }
            _ => {}
        }
    })?;
    if let Some(last_activity) = last_activity {
        record.updated_at = last_activity;
    }
    record.source = source(record.origin.as_deref());
    record.preview = record.preview.or(fallback_prompt);
    record.title = title
        .or(summary)
        .or_else(|| {
            record.preview.as_ref().map(|preview| {
                preview
                    .lines()
                    .next()
                    .unwrap_or(preview)
                    .chars()
                    .take(100)
                    .collect()
            })
        })
        .unwrap_or_else(|| "Claude session".into());
    record.resumable = record.cwd.is_some();
    Ok(record.classify("claude"))
}

/// Metadata rows that Claude rewrites later (titles, tool lists, lineage) carry no
/// timestamp, so file mtime overstates activity. Use the newest recorded event instead.
fn latest_timestamp(line: &[u8]) -> Option<i64> {
    static KEY: std::sync::LazyLock<memmem::Finder<'static>> =
        std::sync::LazyLock::new(|| memmem::Finder::new(b"\"timestamp\":\""));
    KEY.find_iter(line)
        .filter_map(|start| {
            let value = &line[start + KEY.needle().len()..];
            let end = memchr::memchr(b'"', &value[..value.len().min(40)])?;
            crate::events::iso_millis(std::str::from_utf8(&value[..end]).ok()?)
        })
        .max()
}

fn first_prompt(content: &Value) -> Option<String> {
    let blocks = user_content(content);
    if injected_user(content, &blocks).is_some() {
        return None;
    }
    blocks.into_iter().find_map(|block| match block {
        ContentBlock::Text { text } if !text.trim().is_empty() => {
            Some(text.trim().chars().take(300).collect())
        }
        _ => None,
    })
}

fn nonempty(row: &Value, key: &str) -> Option<String> {
    string(row, key)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

fn source(label: Option<&str>) -> ThreadSource {
    match label {
        Some("cli") => ThreadSource::Cli,
        Some("sdk-cli" | "sdk-ts" | "sdk-py" | "print") => ThreadSource::Exec,
        Some("claude-desktop" | "claude-desktop-3p" | "local-agent" | "cowork") => {
            ThreadSource::Desktop
        }
        Some("claude-vscode" | "claude-jetbrains" | "vscode" | "jetbrains") => ThreadSource::Ide,
        Some("remote" | "remote_baku" | "remote_cowork" | "github-action") => ThreadSource::Cloud,
        _ => ThreadSource::Unknown,
    }
}

fn locate(root: &Path, id: &str) -> Result<Option<PathBuf>> {
    validate_id(id)?;
    let name = format!("{id}.jsonl");
    let files = session_files(root, Some(&name)).map_err(io_error)?;
    Ok(files
        .into_iter()
        .max_by_key(|path| path.metadata().and_then(|meta| meta.modified()).ok()))
}

fn session_files(root: &Path, name: Option<&str>) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for project in paths(root)? {
        if !project.is_dir() {
            continue;
        }
        if let Some(name) = name {
            let path = project.join(name);
            if path.is_file() {
                files.push(path);
            }
        } else {
            files.extend(
                paths(&project)?
                    .into_iter()
                    .filter(|path| is_jsonl(path) && path.is_file()),
            );
        }
    }
    Ok(files)
}

fn paths(dir: &Path) -> io::Result<Vec<PathBuf>> {
    if dir.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn open_optional(path: &Path) -> io::Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn not_found() -> AgentError {
    AgentError::new(ErrorCode::NotFound, "Claude session history not found")
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
}

fn scan(file: File, mut row: impl FnMut(&[u8])) -> io::Result<()> {
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        row(&line);
    }
}

fn has_type(mut line: &[u8], matches: impl Fn(&[u8]) -> bool) -> bool {
    let kind = |tail: &[u8]| {
        let Some(value) = trim_start(tail).strip_prefix(b":") else {
            return false;
        };
        let Some(quoted) = trim_start(value).strip_prefix(b"\"") else {
            return false;
        };
        let Some(end) = memchr::memchr(b'"', &quoted[..quoted.len().min(48)]) else {
            return false;
        };
        matches(&quoted[..end])
    };
    if let Some(first) = line.strip_prefix(b"{\"type\"") {
        return kind(first);
    }
    static TYPE: std::sync::LazyLock<memmem::Finder<'static>> =
        std::sync::LazyLock::new(|| memmem::Finder::new(b"\"type\""));
    while let Some(index) = TYPE.find(line) {
        line = &line[index + 6..];
        if kind(line) {
            return true;
        }
    }
    false
}

fn trim_start(bytes: &[u8]) -> &[u8] {
    &bytes[bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len())..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use verre_proto::{ItemKind, NoticeLevel, StopReason, ToolStatus};

    fn read_fixture(rows: &str) -> Transcript {
        let root = tempfile::tempdir().unwrap();
        let id = "01a07c95-7d45-7983-a759-ad414c92947e";
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join(format!("{id}.jsonl")), rows).unwrap();
        read(root.path(), id).unwrap()
    }

    #[test]
    fn injected_records_keep_their_turn_and_history_ends_at_main_assistant_rows() {
        let transcript = read_fixture(
            r#"{"type":"user","uuid":"first","message":{"content":"Start the task"}}
{"type":"assistant","uuid":"opened","message":{"content":[{"type":"tool_use","id":"call","name":"Agent"}]}}
{"type":"user","uuid":"tool-result","toolUseResult":{"status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"Started"}]}}
{"type":"assistant","uuid":"811be3aa-d4ae-4457-b2f1-304fb4114dbd","timestamp":"2026-11-23T00:00:00.100Z","message":{"content":[{"type":"thinking","thinking":"","signature":"signed"},{"type":"text","text":"Started."}]}}
{"type":"assistant","uuid":"child","parent_tool_use_id":"call","message":{"content":[{"type":"thinking","thinking":"  ","signature":"signed"}]}}
{"type":"user","message":{"content":" \n<task-notification><task-id>task</task-id><tool-use-id>call</tool-use-id><status>killed</status><summary>Stopped</summary><output-file> </output-file></task-notification> "}}
{"type":"user","uuid":"stdout","message":{"content":[{"type":"text","text":"<local-command-stdout>Command output</local-command-stdout>"}]}}
{"type":"user","uuid":"silent","message":{"content":"<system-reminder>Remember</system-reminder>"}}
{"type":"user","uuid":"command","message":{"content":"<local-command-caveat>Caveat</local-command-caveat>\n<command-name>/help</command-name><command-message>help</command-message><command-args></command-args>"}}
{"type":"user","uuid":"second","message":{"content":"Keep <system-reminder>this</system-reminder> in my message."}}
{"type":"assistant","uuid":"eabab8d4-205a-4a12-aca1-3dc3bd982055","timestamp":"2026-11-23T00:00:00.200Z","message":{"content":[{"type":"thinking","thinking":"Some thoughts"}]}}
"#,
        );
        let items = transcript.items;
        let ids: Vec<_> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "first",
                "call",
                "811be3aa-d4ae-4457-b2f1-304fb4114dbd:1",
                "claude-notice:1",
                "stdout",
                "turn:811be3aa-d4ae-4457-b2f1-304fb4114dbd",
                "second",
                "eabab8d4-205a-4a12-aca1-3dc3bd982055:0",
                "turn:eabab8d4-205a-4a12-aca1-3dc3bd982055",
            ]
        );
        assert!(matches!(&items[1].kind, ItemKind::ToolCall(call)
            if call.status == ToolStatus::Failed
                && call.output == Some(json!({"status":"async_launched"}))));
        assert_eq!(items[3].turn_id.as_deref(), Some("first"));
        assert_eq!(
            items[3].kind,
            ItemKind::Notice {
                level: NoticeLevel::Info,
                text: "Background task failed: Stopped".into(),
                detail: Some(json!({
                    "task-id":"task", "tool-use-id":"call", "status":"killed", "summary":"Stopped"
                })),
            }
        );
        assert_eq!(items[4].turn_id.as_deref(), Some("first"));
        assert_eq!(
            items[4].kind,
            ItemKind::Notice {
                level: NoticeLevel::Info,
                text: "Command output".into(),
                detail: None,
            }
        );
        assert_eq!(
            items[6].kind,
            ItemKind::UserMessage {
                content: vec![ContentBlock::Text {
                    text: "Keep <system-reminder>this</system-reminder> in my message.".into(),
                }],
            }
        );
        assert!(
            matches!(&items[7].kind, ItemKind::Reasoning { text, streaming: false }
            if text == "Some thoughts")
        );
        for (index, id, ts) in [
            (5, "811be3aa-d4ae-4457-b2f1-304fb4114dbd", 1_795_392_000_100),
            (8, "eabab8d4-205a-4a12-aca1-3dc3bd982055", 1_795_392_000_200),
        ] {
            assert_eq!(items[index].turn_id.as_deref(), Some(id));
            assert_eq!(items[index].ts, Some(ts));
            assert_eq!(
                items[index].kind,
                ItemKind::TurnEnd {
                    stop_reason: StopReason::EndTurn,
                    usage: None,
                    duration_ms: None,
                }
            );
        }
    }

    #[test]
    fn reads_latest_assistant_context_and_totals_unique_messages() {
        let transcript = read_fixture(
            r#"{"type":"assistant","uuid":"first","message":{"id":"msg_1","model":"claude-opus-4-6","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":20,"output_tokens":2}}}
{"type":"assistant","uuid":"continued","message":{"id":"msg_1","model":"claude-opus-4-6","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":20,"output_tokens":4}}}
{"type":"assistant","uuid":"latest","message":{"id":"msg_2","model":"claude-opus-4-6","usage":{"input_tokens":2,"cache_creation_input_tokens":2973,"cache_read_input_tokens":312416,"output_tokens":604}}}
{"type":"assistant","uuid":"child","isSidechain":true,"message":{"id":"msg_3","model":"claude-haiku-4-5-20251001","usage":{"input_tokens":3,"output_tokens":1}}}
{"type":"assistant","uuid":"synthetic","message":{"model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}
{"type":"assistant","uuid":"truncated"
"#,
        );
        assert_eq!(
            transcript.usage.last_turn.unwrap(),
            TokenUsage {
                input: Some(315_391),
                cached_input: Some(312_416),
                output: Some(604),
                context_used: Some(315_995),
                context_window: Some(1_000_000),
                ..TokenUsage::default()
            }
        );
        assert_eq!(
            transcript.usage.total.unwrap(),
            TokenUsage {
                input: Some(315_429),
                cached_input: Some(312_436),
                output: Some(609),
                context_window: Some(1_000_000),
                ..TokenUsage::default()
            }
        );
    }

    #[test]
    fn activity_time_ignores_untimestamped_metadata_appended_later() {
        let root = tempfile::tempdir().unwrap();
        let id = "10000000-0000-4000-8000-000000000001";
        let project = root.path().join("fixture");
        fs::create_dir(&project).unwrap();
        let path = project.join(format!("{id}.jsonl"));
        fs::write(
            &path,
            include_str!("../tests/fixtures/desktop_history.jsonl"),
        )
        .unwrap();
        let record = record(File::open(&path).unwrap(), id).unwrap();
        let start = 1_776_070_800_000;
        assert_eq!(record.updated_at, start + 2_500);
        assert_eq!(record.created_at, Some(start));
        assert_eq!(record.source, ThreadSource::Desktop);
        let transcript = read(root.path(), id).unwrap();
        assert!(!transcript
            .items
            .iter()
            .any(|item| matches!(&item.kind, ItemKind::Reasoning { .. })));
        assert!(transcript.items.iter().any(
            |item| matches!(&item.kind, ItemKind::AgentMessage { text, .. } if text == "Pear.")
        ));
    }

    #[test]
    fn uses_recorded_window_and_leaves_unknown_model_capacity_empty() {
        let rows = r#"{"type":"assistant","message":{"model":"provider/custom","usage":{"input_tokens":10,"output_tokens":2}}}
"#;
        let unknown = read_fixture(rows);
        assert_eq!(unknown.usage.last_turn.unwrap().context_window, None);
        assert_eq!(unknown.usage.total.unwrap().context_window, None);

        let recorded = read_fixture(&format!(
            "{rows}{}\n",
            r#"{"type":"result","modelUsage":{"provider/custom":{"contextWindow":123456}}}"#
        ));
        assert_eq!(
            recorded.usage.last_turn.unwrap().context_window,
            Some(123_456)
        );
        assert_eq!(recorded.usage.total.unwrap().context_window, Some(123_456));
    }
}
