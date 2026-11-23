use std::{collections::HashSet, fs, path::Path, sync::Mutex};

use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Row, Transaction, TransactionBehavior,
};
use vetro_agent_api::Result;
use vetro_proto::{rpc::SearchHit, ThreadParent, ThreadStatus, ThreadSummary};

use crate::{
    history::Mark,
    internal,
    links::{self, Call, Projection},
};

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA temp_store = MEMORY;
CREATE TEMP TABLE IF NOT EXISTS attached (id TEXT PRIMARY KEY, summary TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, agent TEXT NOT NULL, cwd TEXT, source TEXT NOT NULL,
    updated INTEGER NOT NULL, archived INTEGER NOT NULL, summary TEXT NOT NULL,
    hint TEXT, mark TEXT, prompt TEXT, indexed TEXT
);
CREATE INDEX IF NOT EXISTS threads_agent ON threads(agent);
CREATE INDEX IF NOT EXISTS threads_updated ON threads(updated DESC, id);
CREATE INDEX IF NOT EXISTS threads_hint ON threads(hint);
CREATE INDEX IF NOT EXISTS threads_source ON threads(agent, source);
CREATE TABLE IF NOT EXISTS calls (
    parent TEXT NOT NULL, call TEXT NOT NULL, target TEXT, evidence TEXT NOT NULL,
    PRIMARY KEY(parent, call)
);
CREATE INDEX IF NOT EXISTS calls_target ON calls(target);
CREATE TABLE IF NOT EXISTS call_sessions (
    child TEXT NOT NULL, parent TEXT NOT NULL, call TEXT NOT NULL,
    PRIMARY KEY(child,parent,call)
);
CREATE INDEX IF NOT EXISTS call_sessions_parent ON call_sessions(parent);
CREATE TABLE IF NOT EXISTS links (child TEXT PRIMARY KEY, parent TEXT NOT NULL, data TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS links_parent ON links(parent);
CREATE VIRTUAL TABLE IF NOT EXISTS messages USING fts5(thread UNINDEXED, title, body);
CREATE TABLE IF NOT EXISTS documents (thread TEXT NOT NULL, row_id INTEGER NOT NULL,
    PRIMARY KEY(thread, row_id));
";

const SUMMARY: &str = "coalesce((SELECT summary FROM attached WHERE id=t.id),t.summary), l.data,
    (SELECT count(*) FROM links WHERE parent=t.id)";
const JOIN: &str = "threads t LEFT JOIN links l ON l.child=t.id";

#[derive(Default)]
pub(crate) struct Changes {
    pub upserted: Vec<ThreadSummary>,
    pub removed: Vec<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct Filter<'a> {
    pub cwd: Option<&'a str>,
    pub agents: Option<&'a [String]>,
    pub sources: Option<&'a [vetro_proto::ThreadSource]>,
    pub parent: Option<&'a str>,
    pub children: bool,
    pub archived: bool,
    pub cursor: Option<(i64, String)>,
}

pub(crate) struct Index(Mutex<Connection>);

impl Index {
    pub fn temporary() -> Self {
        let conn = Connection::open_in_memory().expect("open temporary index");
        conn.execute_batch(SCHEMA)
            .expect("initialize temporary index");
        Self(Mutex::new(conn))
    }

    pub fn open(root: &Path) -> Result<Self> {
        for name in [
            "index.db",
            "index.db-wal",
            "index.db-shm",
            "index.db-journal",
        ] {
            let path = root.join(name);
            if let Ok(metadata) = fs::symlink_metadata(&path) {
                if metadata.file_type().is_symlink()
                    || fs::canonicalize(&path).map_err(internal)?
                        != fs::canonicalize(root).map_err(internal)?.join(name)
                {
                    return Err(internal("Index file redirects outside its location"));
                }
            }
        }
        let path = root.join("index.db");
        let open = || {
            let conn = Connection::open(&path)?;
            conn.execute_batch(SCHEMA)?;
            Ok(conn)
        };
        let conn = match open() {
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
                ) =>
            {
                for name in [
                    "index.db",
                    "index.db-wal",
                    "index.db-shm",
                    "index.db-journal",
                ] {
                    match fs::remove_file(root.join(name)) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(internal(error)),
                    }
                }
                open().map_err(internal)?
            }
            other => other.map_err(internal)?,
        };
        Ok(Self(Mutex::new(conn)))
    }

    pub fn get(&self, id: &str) -> Result<Option<ThreadSummary>> {
        get(&self.0.lock().unwrap(), id)
    }

    pub fn projects(&self) -> Result<Vec<(String, u32, i64)>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT cwd, count(*), max(updated) FROM threads WHERE cwd IS NOT NULL GROUP BY cwd",
        ).map_err(internal)?;
        collect(&mut stmt, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
    }

    pub fn list(
        &self,
        filter: &Filter<'_>,
        limit: usize,
        overlays: &str,
    ) -> Result<Vec<ThreadSummary>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare_cached(&format!(
            "WITH f AS (SELECT ?1 AS v), overlay AS (SELECT key, value FROM json_each(?2))
             SELECT {SUMMARY} FROM {JOIN} CROSS JOIN f LEFT JOIN overlay o ON o.key=t.id
             WHERE (f.v->>'archived' OR NOT coalesce(o.value->>'archived',t.archived))
               AND (f.v->>'cwd' IS NULL OR t.cwd=f.v->>'cwd')
               AND (f.v->>'agents' IS NULL OR t.agent IN (SELECT value FROM json_each(f.v,'$.agents')))
               AND (f.v->>'sources' IS NULL OR t.source IN (SELECT value FROM json_each(f.v,'$.sources')))
               AND (CASE WHEN f.v->>'parent' IS NOT NULL THEN l.parent=f.v->>'parent'
                    ELSE f.v->>'children' OR l.parent IS NULL END)
               AND (f.v->>'cursor' IS NULL OR t.updated < f.v->>'$.cursor[0]'
                    OR (t.updated=f.v->>'$.cursor[0]' AND t.id>f.v->>'$.cursor[1]'))
             ORDER BY t.updated DESC, t.id LIMIT ?3"
        )).map_err(internal)?;
        collect(
            &mut stmt,
            params![json(filter)?, overlays, limit as i64],
            row_summary,
        )
    }

    pub fn marks(&self, agent: &str) -> Result<Vec<Mark>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT id, mark, indexed FROM threads WHERE agent=?1")
            .map_err(internal)?;
        collect(&mut stmt, [agent], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
    }

    pub fn mark(
        &self,
        thread: &ThreadSummary,
        hint: Option<&str>,
        mark: &str,
        title: &str,
    ) -> Result<Changes> {
        self.change(thread, Some((hint, mark)), None, title)
    }

    pub fn index(
        &self,
        thread: &ThreadSummary,
        mark: Option<&str>,
        projection: &Projection,
        title: &str,
    ) -> Result<Changes> {
        self.change(thread, None, Some((mark, projection)), title)
    }

    pub fn attached(&self, thread: &ThreadSummary, title: &str) -> Result<()> {
        self.change(thread, None, None, title).map(drop)
    }

    fn change(
        &self,
        thread: &ThreadSummary,
        native: Option<(Option<&str>, &str)>,
        projection: Option<(Option<&str>, &Projection)>,
        title: &str,
    ) -> Result<Changes> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)?;
        let old = get(&tx, &thread.id)?;
        let mut affected = HashSet::from([thread.id.clone()]);
        let mut published = affected.clone();
        if projection.is_some() || old.is_none() {
            affected.extend(dependents(&tx, &thread.id)?);
        }
        let chosen = match (&old, native, projection) {
            (Some(old), Some(_), _) if old.live => old,
            (Some(old), None, Some(_)) => old,
            _ => thread,
        };
        save(&tx, chosen)?;
        if native.is_none() && projection.is_none() {
            if thread.live {
                tx.execute(
                    "INSERT OR REPLACE INTO attached VALUES (?1,?2)",
                    params![thread.id, json(thread)?],
                )
                .map_err(internal)?;
            } else {
                tx.execute("DELETE FROM attached WHERE id=?1", [&thread.id])
                    .map_err(internal)?;
            }
        }
        if let Some((hint, mark)) = native {
            tx.execute(
                "UPDATE threads SET hint=?2, mark=?3 WHERE id=?1",
                params![thread.id, hint, mark],
            )
            .map_err(internal)?;
        }
        if let Some((mark, projection)) = projection {
            if mark.is_none() || !chosen.live {
                tx.execute(
                    "UPDATE threads SET prompt=?2, indexed=?3 WHERE id=?1",
                    params![thread.id, projection.prompt, mark],
                )
                .map_err(internal)?;
                replace_documents(&tx, &thread.id, &projection.messages)?;
                tx.execute("DELETE FROM calls WHERE parent=?1", [&thread.id])
                    .map_err(internal)?;
                tx.execute("DELETE FROM call_sessions WHERE parent=?1", [&thread.id])
                    .map_err(internal)?;
                for call in &projection.calls {
                    let evidence = json(call)?;
                    for target in &call.targets {
                        let key = format!("{target}:{}", call.id);
                        tx.execute(
                            "INSERT INTO calls VALUES (?1,?2,?3,?4)",
                            params![thread.id, key, target, evidence],
                        )
                        .map_err(internal)?;
                        for child in &call.ids {
                            tx.execute(
                                "INSERT OR IGNORE INTO call_sessions VALUES (?1,?2,?3)",
                                params![format!("{target}:{child}"), thread.id, key],
                            )
                            .map_err(internal)?;
                        }
                    }
                }
                affected.extend(dependents(&tx, &thread.id)?);
            }
        }
        set_title(&tx, &thread.id, title)?;
        for id in affected {
            relink(&tx, &id, &mut published)?;
        }
        let mut upserted = Vec::new();
        for id in published {
            let current = get(&tx, &id)?;
            if id == thread.id && current == old {
                continue;
            }
            upserted.extend(current);
        }
        tx.commit().map_err(internal)?;
        Ok(Changes {
            upserted,
            removed: Vec::new(),
        })
    }

    pub fn remove(&self, ids: Vec<String>) -> Result<Changes> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)?;
        let mut changes = Changes::default();
        let mut affected = HashSet::new();
        let mut published = HashSet::new();
        for id in ids {
            if get(&tx, &id)?.is_some_and(|summary| summary.live) {
                continue;
            }
            affected.extend(dependents(&tx, &id)?);
            published.extend(parent(&tx, &id)?.map(|value| value.thread_id));
            for sql in [
                "DELETE FROM threads WHERE id=?1",
                "DELETE FROM calls WHERE parent=?1",
                "DELETE FROM call_sessions WHERE parent=?1",
                "DELETE FROM links WHERE child=?1 OR parent=?1",
                "DELETE FROM messages WHERE rowid IN (SELECT row_id FROM documents WHERE thread=?1)",
                "DELETE FROM documents WHERE thread=?1",
            ] {
                tx.execute(sql, [&id]).map_err(internal)?;
            }
            changes.removed.push(id);
        }
        for id in affected {
            relink(&tx, &id, &mut published)?;
        }
        for id in published {
            changes.upserted.extend(get(&tx, &id)?);
        }
        tx.commit().map_err(internal)?;
        Ok(changes)
    }

    pub fn links(&self) -> Result<Vec<(String, ThreadParent)>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT child, data FROM links ORDER BY child")
            .map_err(internal)?;
        collect(&mut stmt, [], |row| Ok((row.get(0)?, decode(row, 1)?)))
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        set_title(&self.0.lock().unwrap(), id, title)
    }

    pub fn children(&self, id: &str) -> Result<Vec<(String, ThreadParent)>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT child, data FROM links WHERE parent=?1 ORDER BY child")
            .map_err(internal)?;
        collect(&mut stmt, [id], |row| Ok((row.get(0)?, decode(row, 1)?)))
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        overlays: &str,
        agent: Option<&str>,
        parent: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        if !query.chars().any(char::is_alphanumeric) {
            return Ok(Vec::new());
        }
        let query = format!("\"{}\"", tokenize(query).replace('"', "\"\""));
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(&format!(
                "WITH hits AS MATERIALIZED (
                SELECT thread, rank, snippet(messages,-1,char(2),char(3),' … ',24) AS snippet
                FROM messages WHERE messages MATCH ?1
             ), best AS (SELECT thread, min(rank) AS score, snippet FROM hits GROUP BY thread)
             SELECT {SUMMARY}, best.snippet FROM best JOIN threads t ON t.id=best.thread
             LEFT JOIN links l ON l.child=t.id
             LEFT JOIN json_each(?3) o ON o.key=t.id
             WHERE NOT coalesce(o.value->>'archived',t.archived)
             AND (?4 IS NULL OR t.agent=?4) AND (?5 IS NULL OR l.parent=?5)
             ORDER BY best.score, t.updated DESC, t.id LIMIT ?2"
            ))
            .map_err(internal)?;
        collect(
            &mut stmt,
            params![query, limit as i64, overlays, agent, parent],
            |row| {
                let snippet: String = row.get(3)?;
                Ok(SearchHit {
                    thread: row_summary(row)?,
                    snippet: snippet
                        .replace('\u{1}', "")
                        .replace(END_START, "")
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                        .replace('\u{2}', "<mark>")
                        .replace('\u{3}', "</mark>"),
                })
            },
        )
    }
}

fn save(conn: &Connection, thread: &ThreadSummary) -> Result<()> {
    let mut summary = thread.clone();
    if summary.live {
        summary.live = false;
        summary.status = ThreadStatus::Idle;
    }
    summary.parent = None;
    summary.child_count = 0;
    conn.execute(
        "INSERT INTO threads(id,agent,cwd,source,updated,archived,summary)
         VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET
         cwd=excluded.cwd,source=excluded.source,updated=excluded.updated,archived=excluded.archived,
         summary=excluded.summary WHERE summary<>excluded.summary",
        params![thread.id, thread.agent_id, thread.cwd, json(&thread.source)?.trim_matches('"'),
            thread.updated_at, thread.archived, json(&summary)?],
    ).map_err(internal)?;
    Ok(())
}

fn parent(conn: &Connection, id: &str) -> Result<Option<ThreadParent>> {
    conn.query_row("SELECT data FROM links WHERE child=?1", [id], |row| {
        decode(row, 0)
    })
    .optional()
    .map_err(internal)
}

fn dependents(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT child FROM links WHERE parent=?1 UNION
         SELECT id FROM threads WHERE hint=?1 UNION
         SELECT child FROM call_sessions WHERE parent=?1 UNION
         SELECT t.id FROM threads t JOIN (SELECT DISTINCT target FROM calls WHERE parent=?1) c
           ON t.agent=c.target WHERE t.source IN ('exec','unknown')",
        )
        .map_err(internal)?;
    collect(&mut stmt, [id], |row| row.get(0))
}

fn relink(tx: &Transaction<'_>, id: &str, changed: &mut HashSet<String>) -> Result<()> {
    let Some((summary, hint, prompt)) = tx
        .query_row(
            "SELECT summary,hint,prompt FROM threads WHERE id=?1",
            [id],
            |row| {
                Ok((
                    decode::<ThreadSummary>(row, 0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?
    else {
        return Ok(());
    };
    let hint = hint.filter(|hint| hint != id && get(tx, hint).ok().flatten().is_some());
    let old = parent(tx, id)?;
    let known_exact: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM call_sessions WHERE child=?1)",
            [id],
            |row| row.get(0),
        )
        .map_err(internal)?;
    let matching = if known_exact || hint.is_some() || links::eligible(&summary) {
        let mut stmt = tx.prepare_cached(
            "SELECT DISTINCT parent,evidence FROM calls WHERE parent<>?1 AND (target=?2 OR parent=?3)",
        ).map_err(internal)?;
        let calls = stmt
            .query_map(params![id, summary.agent_id, hint], |row| {
                Ok((row.get::<_, String>(0)?, decode::<Call>(row, 1)?))
            })
            .map_err(internal)?;
        let mut best = links::Matcher::new(&summary, hint.as_deref(), prompt.as_deref());
        for call in calls {
            let (parent, call) = call.map_err(internal)?;
            best.consider(parent, &call);
        }
        best.finish()
    } else {
        None
    };
    let mut ancestor = matching.as_ref().map(|link| link.thread_id.clone());
    let mut visited = HashSet::from([id.to_owned()]);
    let mut cycle = false;
    while let Some(next) = ancestor {
        if !visited.insert(next.clone()) {
            cycle = true;
            break;
        }
        ancestor = parent(tx, &next)?.map(|link| link.thread_id);
    }
    let matching = matching.filter(|_| !cycle);
    if old != matching {
        changed.insert(id.into());
        changed.extend(old.map(|link| link.thread_id));
        if let Some(link) = matching {
            tx.execute(
                "INSERT OR REPLACE INTO links VALUES (?1,?2,?3)",
                params![id, link.thread_id, json(&link)?],
            )
            .map_err(internal)?;
            changed.insert(link.thread_id);
        } else {
            tx.execute("DELETE FROM links WHERE child=?1", [id])
                .map_err(internal)?;
        }
    }
    Ok(())
}

fn replace_documents(conn: &Connection, id: &str, messages: &[String]) -> Result<()> {
    conn.execute("DELETE FROM messages WHERE rowid IN (SELECT row_id FROM documents WHERE thread=?1 AND row_id>0)", [id]).map_err(internal)?;
    conn.execute("DELETE FROM documents WHERE thread=?1 AND row_id>0", [id])
        .map_err(internal)?;
    let mut row_id: i64 = conn
        .query_row(
            "SELECT max(0,coalesce(max(rowid),0)) FROM messages",
            [],
            |row| row.get(0),
        )
        .map_err(internal)?;
    for text in messages {
        row_id += 1;
        conn.execute(
            "INSERT INTO messages(rowid,thread,body) VALUES (?1,?2,?3)",
            params![row_id, id, tokenize(text)],
        )
        .map_err(internal)?;
        conn.execute("INSERT INTO documents VALUES (?1,?2)", params![id, row_id])
            .map_err(internal)?;
    }
    Ok(())
}

fn set_title(conn: &Connection, id: &str, title: &str) -> Result<()> {
    let row: i64 = conn
        .query_row("SELECT -rowid FROM threads WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .map_err(internal)?;
    let title = tokenize(title);
    let old: Option<String> = conn
        .query_row("SELECT title FROM messages WHERE rowid=?1", [row], |row| {
            row.get(0)
        })
        .optional()
        .map_err(internal)?;
    if old.as_ref() != Some(&title) {
        conn.execute(
            "INSERT OR REPLACE INTO messages(rowid,thread,title) VALUES (?1,?2,?3)",
            params![row, id, title],
        )
        .map_err(internal)?;
        conn.execute(
            "INSERT OR IGNORE INTO documents VALUES (?1,?2)",
            params![id, row],
        )
        .map_err(internal)?;
    }
    Ok(())
}

fn tokenize(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for character in text.chars() {
        let cjk = matches!(character as u32, 0x3040..=0x30ff | 0x3400..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af);
        if cjk {
            result.push('\u{1}');
        }
        result.push(if matches!(character, '\u{1}'..='\u{3}') {
            ' '
        } else {
            character
        });
        if cjk {
            result.push('\u{1}');
        }
    }
    result
}

const END_START: &str = "\u{3}\u{2}";

fn get(conn: &Connection, id: &str) -> Result<Option<ThreadSummary>> {
    conn.query_row(
        &format!("SELECT {SUMMARY} FROM {JOIN} WHERE t.id=?1"),
        [id],
        row_summary,
    )
    .optional()
    .map_err(internal)
}

fn collect<T>(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    stmt.query_map(params, map)
        .map_err(internal)?
        .collect::<rusqlite::Result<_>>()
        .map_err(internal)
}

fn row_summary(row: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    let mut summary: ThreadSummary = decode(row, 0)?;
    summary.parent = None;
    if row.get_ref(1)?.as_str_or_null()?.is_some() {
        summary.parent = Some(decode(row, 1)?);
    }
    summary.child_count = row.get(2)?;
    Ok(summary)
}

fn decode<T: serde::de::DeserializeOwned>(row: &Row<'_>, column: usize) -> rusqlite::Result<T> {
    serde_json::from_str(row.get_ref(column)?.as_str()?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn json(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(internal)
}
