//! Read-only access to the conversation store that Antigravity keeps on disk: the desktop app
//! under `~/.gemini/antigravity` and the CLI under `~/.gemini/antigravity-cli` (same format).
//!
//! Nothing here writes, creates or deletes a file; see [`sqlite`] for how that is enforced.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use rusqlite::{Connection, OptionalExtension};
use vetro_agent_api::{AgentError, Result, SessionRecord, Transcript};
use vetro_proto::{rpc::ErrorCode, ThreadSource};

use crate::wire::{timestamp_ms, Msg};

pub mod steps;
#[cfg(test)]
mod store_tests;
mod util;
pub mod wire;

pub use util::file_uri_to_path;
pub use vetro_store_sqlite as sqlite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// `~/.gemini/antigravity`, written by the desktop app.
    Desktop,
    /// `~/.gemini/antigravity-cli`, written by `agy` (TUI, `-p` and `acp`).
    Cli,
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    kind: StoreKind,
}

#[derive(Default)]
struct Summary {
    title: String,
    preview: String,
    updated_at: Option<i64>,
    created_at: Option<i64>,
    workspace: Option<PathBuf>,
    source: String,
    app_data_dir: String,
    parent: Option<String>,
    role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliMode {
    Print,
    Acp,
    Interactive,
}

fn internal(e: impl std::fmt::Display) -> AgentError {
    AgentError::new(ErrorCode::Internal, format!("antigravity store: {e}"))
}

fn read_error(error: rusqlite::Error) -> AgentError {
    if matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    ) {
        AgentError::new(ErrorCode::Busy, format!("antigravity store: {error}"))
    } else {
        internal(error)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let t = fs::metadata(path).ok()?.modified().ok()?;
    i64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn workspace_from_json(raw: &str) -> Option<PathBuf> {
    let uris: Vec<String> = serde_json::from_str(raw).ok()?;
    uris.iter().find_map(|u| file_uri_to_path(u))
}

fn has_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

fn batches<Key: rusqlite::ToSql + Clone, Value>(
    conn: &Connection,
    sql: &str,
    mut after: Key,
    mut decode: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<(Key, Value)>,
    mut consume: impl FnMut(Key, Value),
) -> rusqlite::Result<()> {
    const BATCH_SIZE: usize = 64;
    let mut stmt = conn.prepare(sql)?;
    loop {
        let batch = stmt
            .query_map(rusqlite::params![after, BATCH_SIZE as i64], &mut decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if let Some((last, _)) = batch.last() {
            after = last.clone();
        }
        let done = batch.len() < BATCH_SIZE;
        for (key, value) in batch {
            consume(key, value);
        }
        if done {
            return Ok(());
        }
    }
}

fn read_summaries(path: &Path) -> rusqlite::Result<HashMap<String, Summary>> {
    let mut out = HashMap::new();
    if !path.is_file() {
        return Ok(out);
    }
    let conn = sqlite::open_readonly(path)?;
    if !has_table(&conn, "conversation_summaries")? {
        return Ok(out);
    }
    batches(
        &conn,
        "SELECT conversation_id, title, preview, last_modified_time, workspace_uris, source, \
         app_data_dir, parent_conversation_id, raw_summary FROM conversation_summaries \
         WHERE conversation_id > ?1 ORDER BY conversation_id LIMIT ?2",
        String::new(),
        |r| {
            let id: String = r.get(0)?;
            let (title, preview, modified, workspace, source, app_data_dir, parent) =
                (1, 2, 3, 4, 5, 6, 7);
            let text = |i: usize| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(i)
                    .unwrap_or_default()
                    .unwrap_or_default())
            };
            Ok((
                id,
                (
                    text(title)?,
                    text(preview)?,
                    text(modified)?,
                    text(workspace)?,
                    text(source)?,
                    text(app_data_dir)?,
                    text(parent)?,
                    r.get::<_, Option<Vec<u8>>>(8)
                        .unwrap_or_default()
                        .unwrap_or_default(),
                ),
            ))
        },
        |id, (title, preview, modified, workspace, source, app_data_dir, parent, raw)| {
            let raw = Msg(&raw);
            let info = raw.msg(17);
            out.insert(
                id,
                Summary {
                    title,
                    preview,
                    updated_at: util::parse_time(&modified)
                        .or_else(|| raw.msg(3).and_then(timestamp_ms)),
                    created_at: raw
                        .msg(7)
                        .and_then(timestamp_ms)
                        .or_else(|| info.and_then(|i| i.msg(2)).and_then(timestamp_ms)),
                    workspace: workspace_from_json(&workspace).or_else(|| {
                        raw.msg(9)
                            .and_then(|w| w.text(1))
                            .and_then(file_uri_to_path)
                    }),
                    source,
                    app_data_dir,
                    parent: Some(parent).filter(|p| !p.is_empty()),
                    role: info
                        .and_then(|i| i.msg(8))
                        .and_then(|s| s.text(2))
                        .map(str::to_owned),
                },
            );
        },
    )?;
    if has_table(&conn, "conversation_parents")? {
        batches(
            &conn,
            "SELECT conversation_id, parent_id FROM conversation_parents
            WHERE conversation_id > ?1 ORDER BY conversation_id LIMIT ?2",
            String::new(),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            |id, parent| {
                if let Some(summary) = out.get_mut(&id) {
                    if let Some(parent) = parent.filter(|parent| !parent.is_empty()) {
                        summary.parent = Some(parent);
                    }
                }
            },
        )?;
    }
    Ok(out)
}

/// Facts that only the conversation file itself has: used when the summary row is missing or
/// incomplete.
#[derive(Default)]
struct FileFacts {
    created_at: Option<i64>,
    updated_at: Option<i64>,
    workspace: Option<PathBuf>,
    first_input: Option<String>,
}

fn read_file_facts(path: &Path, needs_activity: bool) -> rusqlite::Result<FileFacts> {
    let conn = sqlite::open_readonly(path)?;
    let mut facts = FileFacts::default();
    if has_table(&conn, "trajectory_metadata_blob")? {
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM trajectory_metadata_blob WHERE id = 'main'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(blob) = blob {
            let m = Msg(&blob);
            facts.created_at = m.msg(2).and_then(timestamp_ms);
            facts.workspace = m
                .msg(1)
                .and_then(|w| w.text(1))
                .or_else(|| m.text(7))
                .and_then(file_uri_to_path);
        }
    }
    if has_table(&conn, "steps")? {
        if needs_activity {
            let mut stmt =
                conn.prepare("SELECT step_payload FROM steps WHERE step_payload IS NOT NULL")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let payload: Vec<u8> = row.get(0)?;
                let Some(meta) = Msg(&payload).msg(5) else {
                    continue;
                };
                facts.updated_at = facts.updated_at.max(meta.msg(1).and_then(timestamp_ms));
                if let Some(history) = meta.msg(26) {
                    facts.updated_at = facts.updated_at.max(
                        history
                            .msgs(1)
                            .filter_map(|update| update.msg(2).and_then(timestamp_ms))
                            .max(),
                    );
                }
            }
        }
        let payload: Option<Vec<u8>> = conn
            .query_row(
                "SELECT step_payload FROM steps WHERE step_type = ?1 ORDER BY idx LIMIT 1",
                [steps::STEP_USER_INPUT],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        facts.first_input = payload.and_then(|p| {
            let input = Msg(&p).msg(19)?;
            input
                .text(2)
                .or_else(|| input.msgs(3).find_map(|i| i.text(1)))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
    }
    Ok(facts)
}

/// Which CLI conversations were created by which kind of `agy` run, from the CLI's own logs.
fn cli_modes(root: &Path) -> HashMap<String, CliMode> {
    let mut out = HashMap::new();
    let Ok(dir) = fs::read_dir(root.join("log")) else {
        return out;
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".log") {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let mode = if name.starts_with("acp-server") {
            CliMode::Acp
        } else if text.contains("printmode.go") || text.contains("Print mode:") {
            CliMode::Print
        } else {
            CliMode::Interactive
        };
        for (pos, _) in text.match_indices("Created conversation ") {
            let id: String = text[pos + 21..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
                .collect();
            if !id.is_empty() {
                out.entry(id).or_insert(mode);
            }
        }
    }
    out
}

fn read_steps(path: &Path) -> rusqlite::Result<Vec<vetro_agent_api::ItemDraft>> {
    let conn = sqlite::open_readonly(path)?;
    let mut builder = steps::Builder::default();
    batches(
        &conn,
        "SELECT idx, step_type, status, step_payload, error_details FROM steps
         WHERE idx > ?1 ORDER BY idx LIMIT ?2",
        i64::MIN,
        |row| {
            Ok((
                row.get(0)?,
                (
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)
                        .unwrap_or_default()
                        .unwrap_or_default(),
                    row.get::<_, Option<Vec<u8>>>(4).unwrap_or_default(),
                ),
            ))
        },
        |idx, (kind, status, payload, error)| {
            builder.step(idx, kind, status, &payload, error.as_deref());
        },
    )?;
    Ok(builder.finish())
}

fn shorten(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

impl Store {
    /// Never creates anything; a missing root simply lists nothing.
    pub fn open(root: impl Into<PathBuf>) -> Store {
        let root = root.into();
        let kind = match root.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with("-cli") => StoreKind::Cli,
            _ => StoreKind::Desktop,
        };
        Store { root, kind }
    }

    pub fn with_kind(root: impl Into<PathBuf>, kind: StoreKind) -> Store {
        Store {
            root: root.into(),
            kind,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn kind(&self) -> StoreKind {
        self.kind
    }

    fn conversation_path(&self, id: &str) -> PathBuf {
        self.root.join("conversations").join(format!("{id}.db"))
    }

    /// All conversations whose file lives in this store, newest first. The summaries database
    /// also lists conversations of the sibling store; those are left to that store.
    pub fn list(&self) -> Result<Vec<SessionRecord>> {
        self.list_with_native_activity()
            .map(|entries| entries.into_iter().map(|(record, _)| record).collect())
    }

    pub fn list_with_native_activity(&self) -> Result<Vec<(SessionRecord, Option<i64>)>> {
        let summaries =
            read_summaries(&self.root.join("conversation_summaries.db")).map_err(read_error)?;
        let modes = match self.kind {
            StoreKind::Cli => cli_modes(&self.root),
            StoreKind::Desktop => HashMap::new(),
        };
        let dir = match fs::read_dir(self.root.join("conversations")) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(internal(e)),
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".db").filter(|id| valid_id(id)) else {
                continue;
            };
            if !seen.insert(id.to_owned()) {
                continue;
            }
            let path = entry.path();
            let summary = summaries.get(id);
            let needs_facts = summary.is_none_or(|s| {
                s.workspace.is_none()
                    || s.created_at.is_none()
                    || (s.title.is_empty() && s.role.is_none())
            });
            let needs_activity = summary.and_then(|s| s.updated_at).is_none();
            let facts = if needs_facts || needs_activity {
                read_file_facts(&path, needs_activity).unwrap_or_default()
            } else {
                FileFacts::default()
            };
            let empty = Summary::default();
            let s = summary.unwrap_or(&empty);
            let wal = sibling(&path, "-wal");
            let wal_time = fs::metadata(&wal)
                .ok()
                .filter(|m| m.len() > 0)
                .and_then(|_| mtime_ms(&wal));
            let file_time = mtime_ms(&path).max(wal_time);
            let preview = Some(s.preview.trim())
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .or_else(|| facts.first_input.as_deref().map(|t| shorten(t, 200)));
            let title = Some(s.title.trim())
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .or_else(|| s.role.clone())
                .or_else(|| preview.as_deref().map(|p| shorten(p, 80)))
                .unwrap_or_else(|| "Untitled conversation".into());
            let mode = modes.get(id).copied();
            let source = match self.kind {
                StoreKind::Desktop => ThreadSource::Desktop,
                StoreKind::Cli if mode == Some(CliMode::Print) => ThreadSource::Exec,
                StoreKind::Cli => ThreadSource::Cli,
            };
            let base = Some(s.source.as_str())
                .filter(|v| !v.is_empty())
                .or(Some(s.app_data_dir.as_str()).filter(|v| !v.is_empty()))
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    self.root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            let origin = match mode {
                Some(CliMode::Print) => format!("{base} print"),
                Some(CliMode::Acp) => format!("{base} acp"),
                _ => base,
            };
            let created_at = s.created_at.or(facts.created_at);
            let native_activity = s.updated_at.or(facts.updated_at.max(created_at));
            let record = SessionRecord {
                native_id: id.to_owned(),
                title,
                preview,
                cwd: s.workspace.clone().or(facts.workspace),
                created_at,
                updated_at: native_activity.or(file_time).unwrap_or(0),
                source,
                origin: Some(origin),
                model: None,
                resumable: false,
                parent_native_id: s.parent.clone(),
                internal: false,
            };
            out.push((record.classify("antigravity"), native_activity));
        }
        out.sort_by(|(left, _), (right, _)| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.native_id.cmp(&right.native_id))
        });
        Ok(out)
    }

    /// The full transcript of one conversation, oldest first. Steps are decoded one row at a
    /// time; only the extracted items are kept.
    pub fn read(&self, conversation_id: &str) -> Result<Transcript> {
        if !valid_id(conversation_id) {
            return Err(AgentError::new(
                ErrorCode::BadRequest,
                "invalid conversation id",
            ));
        }
        let path = self.conversation_path(conversation_id);
        if !path.is_file() {
            return Err(AgentError::new(
                ErrorCode::NotFound,
                format!("conversation {conversation_id} not found"),
            ));
        }
        let items = read_steps(&path).map_err(read_error)?;
        Ok(Transcript {
            items,
            ..Transcript::default()
        })
    }
}
