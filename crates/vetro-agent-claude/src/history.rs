use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use memchr::memmem;
use serde_json::Value;
use vetro_agent_api::{AgentError, ItemDraft, Result, SessionEvent, SessionRecord, Transcript};
use vetro_proto::{rpc::ErrorCode, ContentBlock, ThreadSource};

use crate::{
    bad_request,
    events::{timestamp, user_content, Timeline},
    io_error, string,
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
    let mut transcript = HistoryTranscript::default();
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
    Ok(Transcript {
        items: transcript.items,
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
                matches!(kind, b"assistant" | b"user" | b"system")
            }) {
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
            match string(&row, "type") {
                Some("assistant" | "user") => {
                    self.timeline
                        .message(&row, row_parent.as_deref(), &mut events)
                }
                Some("system") => self
                    .timeline
                    .notice(&row, row_parent.as_deref(), &mut events),
                _ => {}
            }
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
        })?;
        if parent.is_some() {
            self.timeline.turn = turn_at_open;
        }
        Ok(())
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
    };
    let mut title = None;
    let mut summary = None;
    let mut fallback_prompt = None;
    scan(file, |line| {
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
                if record.preview.is_none()
                    && row["type"] == "user"
                    && ![
                        "isSynthetic",
                        "isMeta",
                        "isVisibleInTranscriptOnly",
                        "isCompactSummary",
                    ]
                    .iter()
                    .any(|key| row[*key] == true)
                {
                    record.preview = first_prompt(&row["message"]["content"]);
                }
            }
            _ => {}
        }
    })?;
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
    Ok(record)
}

fn first_prompt(content: &Value) -> Option<String> {
    user_content(content)
        .into_iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => {
                let text = text.trim();
                (!text.is_empty()
                    && !text.starts_with("<local-command-")
                    && !text.starts_with("<task-notification>"))
                .then(|| text.chars().take(300).collect())
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
