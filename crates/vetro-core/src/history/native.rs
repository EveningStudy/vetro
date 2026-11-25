use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use serde_json::Value;
use vetro_agent_api::{Result, SessionRecord};
use vetro_proto::ThreadSource;

use super::{file_mark, modified_ms};
use crate::internal;

#[derive(Default)]
pub(super) struct Inventory {
    pub authoritative: bool,
    pub antigravity_disk_scanned: bool,
    pub files: HashMap<String, NativeFile>,
    pub errors: Vec<String>,
}

pub(super) struct NativeFile {
    pub record: Option<SessionRecord>,
    pub path: Option<PathBuf>,
    pub mark: String,
    pub archived: bool,
}

pub(super) fn inventory(agent: &str, roots: &[PathBuf]) -> Result<Inventory> {
    let mut inventory = Inventory::default();
    match agent {
        "claude" if !roots.is_empty() => {
            for root in roots {
                for project in paths(root)?
                    .into_iter()
                    .filter(|path| path.is_dir() && !path.is_symlink())
                {
                    for path in paths(&project)? {
                        let Some(id) = path.file_stem().and_then(|id| id.to_str()).filter(|id| {
                            uuid(id) && path.extension().is_some_and(|ext| ext == "jsonl")
                        }) else {
                            continue;
                        };
                        let mut extra = String::new();
                        for sidechain in paths(&path.with_extension("").join("subagents"))? {
                            if let Some(mark) = file_mark(&sidechain) {
                                extra.push_str(&format!("{}:{mark}", sidechain.display()));
                            }
                        }
                        insert_file(&mut inventory.files, id.into(), &path, extra, false);
                    }
                }
            }
            inventory.authoritative = true;
        }
        "antigravity" => {
            for root in roots {
                for path in paths(&root.join("conversations"))? {
                    if path.extension().is_none_or(|extension| extension != "db") {
                        continue;
                    }
                    let Some(id) = path.file_stem().and_then(|id| id.to_str()) else {
                        continue;
                    };
                    let wal = file_mark(&path.with_extension("db-wal")).unwrap_or_default();
                    if !insert_file(&mut inventory.files, id.into(), &path, wal, false) {
                        inventory
                            .errors
                            .push(format!("Could not stat {}", path.display()));
                    }
                }
            }
            inventory.antigravity_disk_scanned = !roots.is_empty();
        }
        #[cfg(feature = "codex")]
        "codex" if !roots.is_empty() => inventory = codex::inventory(&roots[0])?,
        _ => {}
    }
    Ok(inventory)
}

fn paths(path: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(path) {
        Ok(entries) => {
            let mut entries = entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(internal)?;
            entries.sort_unstable();
            Ok(entries)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(internal(error)),
    }
}

fn uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn insert_file(
    files: &mut HashMap<String, NativeFile>,
    id: String,
    path: &Path,
    extra: String,
    archived: bool,
) -> bool {
    let Some(mark) = file_mark(path) else {
        return false;
    };
    if files
        .get(&id)
        .and_then(|file| file.path.as_deref())
        .is_some_and(|previous| modified_ms(previous) > modified_ms(path))
    {
        return true;
    }
    files.insert(
        id,
        NativeFile {
            record: None,
            path: Some(path.into()),
            mark: format!("{archived}:{}{mark}{extra}", path.display()),
            archived,
        },
    );
    true
}

pub(super) fn file_record(agent: &str, path: &Path, id: String) -> Result<Option<SessionRecord>> {
    if agent == "claude" {
        return claude_record(path, id);
    }
    #[cfg(feature = "codex")]
    if agent == "codex" {
        return codex::rollout_record(path, id);
    }
    Ok(None)
}

fn blank(id: String, path: &Path, title: &str) -> SessionRecord {
    SessionRecord {
        native_id: id,
        title: title.into(),
        preview: None,
        cwd: None,
        created_at: None,
        updated_at: modified_ms(path),
        source: ThreadSource::Unknown,
        origin: None,
        model: None,
        resumable: false,
        parent_native_id: None,
        internal: false,
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn timestamp(value: &Value) -> Option<i64> {
    let instant = humantime::parse_rfc3339_weak(value.as_str()?).ok()?;
    Some(instant.duration_since(super::UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn first_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| {
            value
                .as_array()?
                .iter()
                .find_map(|block| block["text"].as_str().filter(|text| !text.is_empty()))
        })
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(300).collect())
}

/// Claude later rewrites untimestamped metadata rows (titles, tool lists, lineage),
/// so file mtime overstates activity. The newest recorded event time is authoritative.
fn latest_timestamp(line: &[u8]) -> Option<i64> {
    static KEY: std::sync::LazyLock<memchr::memmem::Finder<'static>> =
        std::sync::LazyLock::new(|| memchr::memmem::Finder::new(br#""timestamp":""#));
    KEY.find_iter(line)
        .filter_map(|start| {
            let value = &line[start + KEY.needle().len()..];
            let end = memchr::memchr(b'"', &value[..value.len().min(40)])?;
            let text = std::str::from_utf8(&value[..end]).ok()?;
            timestamp(&Value::String(text.into()))
        })
        .max()
}

fn claude_record(path: &Path, id: String) -> Result<Option<SessionRecord>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(internal(error)),
    };
    let mut record = blank(id, path, "Claude session");
    let mut title = None;
    let mut generated = None;
    let mut fallback = None;
    let mut last_activity = None;
    for line in BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { break };
        last_activity = last_activity.max(latest_timestamp(&line));
        if ![
            b"\"custom-title\"".as_slice(),
            b"\"ai-title\"",
            b"\"summary\"",
            b"\"relocated\"",
            b"\"last-prompt\"",
            b"\"user\"",
            b"\"assistant\"",
        ]
        .iter()
        .any(|tag| {
            line.get(..160)
                .unwrap_or(&line)
                .windows(tag.len())
                .any(|window| window == *tag)
        }) {
            continue;
        }
        let Ok(row) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if row["isSidechain"] == true {
            continue;
        }
        match row["type"].as_str() {
            Some("custom-title") => title = string(&row, "customTitle").or(title),
            Some("ai-title") => generated = string(&row, "aiTitle").or(generated),
            Some("summary") => generated = string(&row, "summary").or(generated),
            Some("last-prompt") if fallback.is_none() => fallback = string(&row, "lastPrompt"),
            Some("relocated") => {
                record.cwd = string(&row, "relocatedCwd")
                    .map(PathBuf::from)
                    .or(record.cwd)
            }
            Some("user" | "assistant") => {
                if record.cwd.is_none() {
                    record.cwd = string(&row, "cwd").map(PathBuf::from);
                }
                if record.origin.is_none() {
                    record.origin = string(&row, "entrypoint");
                }
                record.created_at = record.created_at.or_else(|| timestamp(&row["timestamp"]));
                record.parent_native_id = record
                    .parent_native_id
                    .or_else(|| string(&row, "parentSessionId"));
                if row["type"] == "assistant" && record.model.is_none() {
                    record.model =
                        string(&row["message"], "model").filter(|model| !model.starts_with('<'));
                }
                if row["type"] == "user"
                    && record.preview.is_none()
                    && ![
                        "isSynthetic",
                        "isMeta",
                        "isVisibleInTranscriptOnly",
                        "isCompactSummary",
                    ]
                    .iter()
                    .any(|key| row[*key] == true)
                {
                    record.preview = first_text(&row["message"]["content"]).filter(|text| {
                        !text.starts_with("<local-command-")
                            && !text.starts_with("<task-notification>")
                    });
                }
            }
            _ => {}
        }
    }
    record.resumable = record.cwd.is_some();
    record.preview = record.preview.or(fallback);
    if let Some(last_activity) = last_activity {
        record.updated_at = last_activity;
    }
    record.source = match record.origin.as_deref() {
        Some("cli") => ThreadSource::Cli,
        Some("sdk-cli" | "sdk-ts" | "sdk-py" | "print") => ThreadSource::Exec,
        Some("claude-desktop" | "claude-desktop-3p" | "local-agent" | "cowork") => {
            ThreadSource::Desktop
        }
        Some("claude-vscode" | "claude-jetbrains" | "vscode" | "jetbrains") => ThreadSource::Ide,
        Some("remote" | "remote_baku" | "remote_cowork" | "github-action") => ThreadSource::Cloud,
        _ => ThreadSource::Unknown,
    };
    if let Some(name) = title.or(generated).or_else(|| {
        record
            .preview
            .as_ref()?
            .lines()
            .next()
            .map(|line| line.chars().take(100).collect())
    }) {
        record.title = name;
    }
    Ok(
        (record.cwd.is_some() || record.created_at.is_some() || record.preview.is_some())
            .then_some(record),
    )
}

#[cfg(feature = "codex")]
pub(super) use codex::fallback as codex_fallback;

#[cfg(feature = "codex")]
mod codex;
