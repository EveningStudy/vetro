use std::{collections::HashSet, fs, path::Path, sync::Mutex, time::Duration};

use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Row, Transaction, TransactionBehavior,
};
use verre_agent_api::{session_is_internal, Result};
use verre_proto::{rpc::SearchHit, ThreadParent, ThreadSource, ThreadStatus, ThreadSummary};

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
    hint TEXT, mark TEXT, prompt TEXT, indexed TEXT, internal INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS threads_agent ON threads(agent);
CREATE INDEX IF NOT EXISTS threads_updated ON threads(updated DESC, id);
CREATE INDEX IF NOT EXISTS threads_hint ON threads(hint);
CREATE INDEX IF NOT EXISTS threads_source ON threads(agent, source);
CREATE INDEX IF NOT EXISTS threads_untyped_id ON threads(':' || lower(substr(id,instr(id,':')+1)));
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
    (SELECT count(*) FROM links WHERE parent=t.id), t.internal";
const JOIN: &str = "threads t LEFT JOIN links l ON l.child=t.id";
const VISIBLE_HISTORY: &str = "NOT (t.agent='codex' AND (t.source='cloud'
    OR coalesce(t.summary->>'origin','') IN
        ('chatgpt','Codex desktop catalog (cloud)','Codex desktop catalog (remote)')))";

#[derive(Default)]
pub(crate) struct Changes {
    pub upserted: Vec<ThreadSummary>,
    pub removed: Vec<String>,
}

#[derive(Default, serde::Serialize)]
pub(crate) struct Filter<'a> {
    pub cwd: Option<&'a str>,
    pub agents: Option<&'a [String]>,
    pub sources: Option<&'a [verre_proto::ThreadSource]>,
    pub parent: Option<&'a str>,
    pub children: bool,
    pub archived: bool,
    pub include_internal: bool,
    pub cursor: Option<(i64, String)>,
}

pub(crate) struct Index(Mutex<Connection>, bool);

pub(crate) struct ProjectActivity {
    pub path: String,
    pub thread_count: u32,
    pub last_activity_at: i64,
    pub root_thread_count: u32,
    pub last_root_activity_at: Option<i64>,
}

impl Index {
    pub fn temporary() -> Self {
        let conn = Connection::open_in_memory().expect("open temporary index");
        conn.execute_batch(SCHEMA)
            .expect("initialize temporary index");
        Self(Mutex::new(conn), true)
    }

    #[cfg(all(
        test,
        feature = "claude",
        feature = "codex",
        feature = "antigravity-store"
    ))]
    pub fn suppress_workspace_reads(&mut self) {
        self.1 = false;
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
        let open = || -> Result<_> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .open(&path)
                    .map_err(internal)?;
            }
            Ok(())
        };
        let connect = || {
            let mut conn = Connection::open(&path)?;
            conn.busy_timeout(Duration::from_secs(5))?;
            conn.execute_batch(SCHEMA)?;
            migrate_internal(&mut conn)?;
            Ok(conn)
        };
        open()?;
        let conn = match connect() {
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
                open()?;
                connect().map_err(internal)?
            }
            other => other.map_err(internal)?,
        };
        Ok(Self(Mutex::new(conn), true))
    }

    pub fn get(&self, id: &str) -> Result<Option<ThreadSummary>> {
        get(&self.0.lock().unwrap(), id)
    }

    pub fn projects(&self, overlays: &str) -> Result<Vec<ProjectActivity>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(&format!(
                "WITH project_threads AS (
                SELECT t.cwd, t.updated, l.child IS NULL AND t.internal=0
                    AND NOT coalesce(o.value->>'archived',t.archived) AS root
                FROM threads t LEFT JOIN links l ON l.child=t.id
                LEFT JOIN json_each(?1) o ON o.key=t.id
                WHERE t.cwd IS NOT NULL AND {VISIBLE_HISTORY}
            )
            SELECT cwd, count(*), max(updated), count(*) FILTER (WHERE root),
                   max(updated) FILTER (WHERE root) FROM project_threads GROUP BY cwd",
            ))
            .map_err(internal)?;
        collect(&mut stmt, [overlays], |row| {
            Ok(ProjectActivity {
                path: row.get(0)?,
                thread_count: row.get(1)?,
                last_activity_at: row.get(2)?,
                root_thread_count: row.get(3)?,
                last_root_activity_at: row.get(4)?,
            })
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
               AND (t.internal=0 OR f.v->>'include_internal' OR f.v->>'parent' IS NOT NULL
                    OR 'exec' IN (SELECT value FROM json_each(f.v,'$.sources')))
               AND {VISIBLE_HISTORY}
               AND (coalesce(t.summary->>'origin','') != 'Codex desktop catalog (local)'
                    OR f.v->>'include_internal')
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

    pub fn active_roots(&self) -> Result<Vec<ThreadSummary>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT a.summary, l.data, (SELECT count(*) FROM links WHERE parent=t.id), t.internal
                 FROM attached a CROSS JOIN threads t ON t.id=a.id LEFT JOIN links l ON l.child=t.id
                 WHERE l.parent IS NULL AND {VISIBLE_HISTORY}
                   AND a.summary->>'status' IN ('running','awaiting_input')
                 ORDER BY t.updated DESC,t.id"
            ))
            .map_err(internal)?;
        collect(&mut stmt, [], row_summary)
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
        let chosen = match (&old, native, projection) {
            (Some(old), Some(_), _) if old.live => old,
            (Some(old), None, Some(_)) => old,
            _ => thread,
        };
        if projection.is_some()
            || old
                .as_ref()
                .is_none_or(|old| old.created_at != chosen.created_at)
        {
            affected.extend(dependents(&tx, &thread.id)?);
        }
        let internal_flag = chosen.source != ThreadSource::Verre
            && (classified(chosen)
                || (chosen.live && (classified(thread) || old.as_ref().is_some_and(classified))));
        save(&tx, chosen, internal_flag)?;
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
                    let targets = call.targets.iter().map(|target| Some(target.as_str()));
                    for target in call
                        .targets
                        .is_empty()
                        .then_some(None)
                        .into_iter()
                        .chain(targets)
                    {
                        let prefix = target.unwrap_or_default();
                        let key = format!("{prefix}:{}", call.id);
                        tx.execute(
                            "INSERT INTO calls VALUES (?1,?2,?3,?4)",
                            params![thread.id, key, target, evidence],
                        )
                        .map_err(internal)?;
                        for child in &call.ids {
                            tx.execute(
                                "INSERT OR IGNORE INTO call_sessions VALUES (?1,?2,?3)",
                                params![format!("{prefix}:{child}"), thread.id, key],
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
            relink(&tx, &id, &mut published, self.1).map_err(internal)?;
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
            published.extend(
                parent(&tx, &id)
                    .map_err(internal)?
                    .map(|value| value.thread_id),
            );
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
            relink(&tx, &id, &mut published, self.1).map_err(internal)?;
        }
        for id in published {
            changes.upserted.extend(get(&tx, &id)?);
        }
        tx.commit().map_err(internal)?;
        Ok(changes)
    }

    pub fn created_in_verre(&self) -> Result<Vec<String>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT id FROM threads WHERE source='verre'")
            .map_err(internal)?;
        collect(&mut stmt, [], |row| row.get(0))
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
        visible: impl Fn(&ThreadSummary) -> bool,
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
             LEFT JOIN json_each(?2) o ON o.key=t.id
             WHERE NOT coalesce(o.value->>'archived',t.archived)
             AND (?3 IS NULL OR t.agent=?3) AND (?4 IS NULL OR l.parent=?4)
             AND {VISIBLE_HISTORY}
             ORDER BY best.score, t.updated DESC, t.id"
            ))
            .map_err(internal)?;
        let mut rows = stmt
            .query(params![query, overlays, agent, parent])
            .map_err(internal)?;
        let mut hits = Vec::new();
        while hits.len() < limit {
            let Some(row) = rows.next().map_err(internal)? else {
                break;
            };
            let thread = row_summary(row).map_err(internal)?;
            if visible(&thread) {
                let snippet: String = row.get(4).map_err(internal)?;
                hits.push(SearchHit {
                    thread,
                    snippet: snippet
                        .replace('\u{1}', "")
                        .replace(END_START, "")
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                        .replace('\u{2}', "<mark>")
                        .replace('\u{3}', "</mark>"),
                });
            }
        }
        Ok(hits)
    }
}

fn migrate_internal(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('threads') WHERE name='internal')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        tx.execute_batch("ALTER TABLE threads ADD COLUMN internal INTEGER NOT NULL DEFAULT 0")?;
        let mut read = tx.prepare("SELECT id, summary FROM threads")?;
        let mut update = tx.prepare("UPDATE threads SET internal=1 WHERE id=?1")?;
        let mut rows = read.query([])?;
        while let Some(row) = rows.next()? {
            let summary = decode::<ThreadSummary>(row, 1)?;
            if classified(&summary) {
                update.execute([row.get_ref(0)?.as_str()?])?;
            }
        }
    }
    let version: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 1 {
        tx.execute_batch("UPDATE threads SET indexed=NULL; PRAGMA user_version=1;")?;
    }
    if version < 2 {
        let mut updates = Vec::new();
        {
            let mut read = tx.prepare("SELECT id, summary FROM threads")?;
            let mut rows = read.query([])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let mut summary = decode::<ThreadSummary>(row, 1)?;
                if summary.live || summary.status != ThreadStatus::Idle {
                    summary.live = false;
                    summary.status = ThreadStatus::Idle;
                    if let Ok(encoded) = serde_json::to_string(&summary) {
                        updates.push((id, encoded));
                    }
                }
            }
        }
        let mut update = tx.prepare("UPDATE threads SET summary=?2 WHERE id=?1")?;
        for (id, encoded) in updates {
            update.execute(params![id, encoded])?;
        }
        tx.execute_batch("PRAGMA user_version=2;")?;
    }
    if version < 3 {
        let children = {
            let mut read = tx.prepare("SELECT child FROM links ORDER BY child")?;
            let rows = read.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute("DELETE FROM links", [])?;
        let mut changed = HashSet::new();
        for child in children {
            relink(&tx, &child, &mut changed, true)?;
        }
        tx.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        let existing_children = {
            let mut read = tx.prepare("SELECT child FROM links ORDER BY child")?;
            let children = read.query_map([], |row| row.get::<_, String>(0))?;
            children.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute_batch(
            "DELETE FROM call_sessions WHERE EXISTS(
                 SELECT 1 FROM calls c WHERE c.parent=call_sessions.parent
                 AND c.call=call_sessions.call AND c.target IS NULL
             );
             DELETE FROM calls WHERE target IS NULL;
             UPDATE threads SET mark=NULL, indexed=NULL;",
        )?;
        let mut changed = HashSet::new();
        for child in existing_children {
            relink(&tx, &child, &mut changed, true)?;
        }
        tx.pragma_update(None, "user_version", 4)?;
    }
    tx.commit()
}

pub(crate) fn classified(thread: &ThreadSummary) -> bool {
    thread.internal
        || session_is_internal(
            &thread.agent_id,
            thread.source,
            thread.origin.as_deref(),
            &thread.title,
            thread.preview.as_deref(),
        )
}

fn save(conn: &Connection, thread: &ThreadSummary, internal_flag: bool) -> Result<()> {
    let mut summary = thread.clone();
    summary.internal = internal_flag;
    // Live/active/error states are ephemeral to the current run and live in the `attached` table.
    summary.live = false;
    summary.status = ThreadStatus::Idle;
    summary.parent = None;
    summary.child_count = 0;
    conn.execute(
        "INSERT INTO threads(id,agent,cwd,source,updated,archived,summary,internal)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET
         cwd=excluded.cwd,source=excluded.source,updated=excluded.updated,archived=excluded.archived,
         summary=excluded.summary,internal=excluded.internal
         WHERE summary<>excluded.summary OR internal<>excluded.internal",
        params![thread.id, thread.agent_id, thread.cwd, json(&thread.source)?.trim_matches('"'),
            thread.updated_at, thread.archived, json(&summary)?, internal_flag],
    ).map_err(internal)?;
    Ok(())
}

fn parent(conn: &Connection, id: &str) -> rusqlite::Result<Option<ThreadParent>> {
    conn.query_row("SELECT data FROM links WHERE child=?1", [id], |row| {
        decode(row, 0)
    })
    .optional()
}

fn dependents(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT child FROM links WHERE parent=?1 UNION
         SELECT id FROM threads WHERE hint=?1 UNION
         SELECT child FROM call_sessions WHERE parent=?1 UNION
         SELECT t.id FROM threads t JOIN call_sessions s
           ON s.child=':' || lower(substr(t.id,instr(t.id,':')+1))
           WHERE s.parent=?1 AND s.child LIKE ':%' UNION
         SELECT t.id FROM threads t JOIN (SELECT DISTINCT target FROM calls WHERE parent=?1) c
         ON t.agent=c.target WHERE t.source IN ('exec','unknown') UNION
         SELECT t.id FROM threads t JOIN calls c ON c.parent=?1 AND c.target IS NULL
           AND c.evidence->>'launch' WHERE t.source IN ('exec','unknown')
           AND t.summary->>'createdAt' BETWEEN c.evidence->>'start'
           AND c.evidence->>'start' + ?2",
        )
        .map_err(internal)?;
    collect(&mut stmt, params![id, links::LAUNCH_WINDOW], |row| {
        row.get(0)
    })
}

fn relink(
    tx: &Transaction<'_>,
    id: &str,
    changed: &mut HashSet<String>,
    workspace_reads: bool,
) -> rusqlite::Result<()> {
    let Some((summary, hint, prompt)) = tx
        .query_row(
            "SELECT t.summary,h.id,t.prompt,t.internal FROM threads t
             LEFT JOIN threads h ON h.id=t.hint AND h.id<>t.id WHERE t.id=?1",
            [id],
            |row| {
                let mut summary = decode::<ThreadSummary>(row, 0)?;
                summary.internal = row.get(3)?;
                Ok((
                    summary,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(());
    };
    let old = parent(tx, id)?;
    let untyped = format!(":{}", summary.native_id.to_ascii_lowercase());
    let known_exact: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM call_sessions WHERE child IN (?1,?2))",
        params![id, untyped],
        |row| row.get(0),
    )?;
    let matching = if known_exact || hint.is_some() || links::eligible(&summary) {
        let mut stmt = tx.prepare_cached(
            "SELECT DISTINCT c.parent,c.evidence
             FROM calls c JOIN threads p ON p.id=c.parent WHERE c.parent<>?1 AND
             (c.target=?2 OR (c.target IS NOT NULL AND c.parent=?3) OR (c.target IS NULL AND (EXISTS(
               SELECT 1 FROM call_sessions s WHERE s.child=?4 AND s.parent=c.parent AND s.call=c.call
             ) OR (c.evidence->>'launch' AND ?5 BETWEEN c.evidence->>'start'
               AND c.evidence->>'start' + ?6))))",
        )?;
        let calls = stmt.query_map(
            params![
                id,
                summary.agent_id,
                hint,
                untyped,
                summary.created_at,
                links::LAUNCH_WINDOW
            ],
            |row| Ok((row.get::<_, String>(0)?, decode::<Call>(row, 1)?)),
        )?;
        let mut best = links::Matcher::new(&summary, hint.as_deref(), prompt.as_deref());
        for call in calls {
            let (parent, call) = call?;
            best.consider_with_workspace_reads(parent, &call, workspace_reads);
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
            let data = serde_json::to_string(&link)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            tx.execute(
                "INSERT OR REPLACE INTO links VALUES (?1,?2,?3)",
                params![id, link.thread_id, data],
            )?;
            changed.insert(link.thread_id);
        } else {
            tx.execute("DELETE FROM links WHERE child=?1", [id])?;
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
        &format!("SELECT {SUMMARY} FROM {JOIN} WHERE t.id=?1 AND {VISIBLE_HISTORY}"),
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
    summary.internal = row.get(3)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use verre_proto::ThreadSource;

    #[test]
    fn migrates_legacy_index_in_place() {
        let root = tempfile::tempdir().unwrap();
        let index = Index::open(root.path()).unwrap();
        let mut summary = crate::service::summary(
            "codex",
            verre_agent_api::SessionRecord {
                native_id: "child".into(),
                title: "Legacy child".into(),
                preview: None,
                cwd: Some(root.path().into()),
                created_at: None,
                updated_at: 15,
                source: ThreadSource::Exec,
                origin: None,
                model: None,
                resumable: true,
                parent_native_id: None,
                internal: false,
            },
        );
        summary.source = ThreadSource::Cli;
        summary.internal = false;
        summary.id = "codex:parent".into();
        summary.native_id = "parent".into();
        index.mark(&summary, None, "parent-mark", "Parent").unwrap();
        summary.source = ThreadSource::Exec;
        summary.id = "codex:child".into();
        summary.native_id = "child".into();
        index
            .mark(
                &summary,
                Some("codex:parent"),
                "legacy-mark",
                &summary.title,
            )
            .unwrap();
        index
            .0
            .lock()
            .unwrap()
            .execute_batch(
                "ALTER TABLE threads DROP COLUMN internal;
             UPDATE threads SET summary=json_remove(summary,'$.internal');",
            )
            .unwrap();
        drop(index);

        let migrated = Index::open(root.path()).unwrap();
        let child = migrated.get("codex:child").unwrap().unwrap();
        assert!(child.internal);
        assert_eq!(child.parent.unwrap().thread_id, "codex:parent");
        assert!(migrated
            .marks("codex")
            .unwrap()
            .iter()
            .any(|(id, mark, _)| {
                id == "codex:child" && mark.as_deref() == Some("legacy-mark")
            }));
        let hits = migrated
            .search("Legacy child", 10, "{}", None, None, |_| true)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].thread.internal);
    }

    #[test]
    fn excludes_legacy_codex_web_and_remote_stubs_from_lists_and_search() {
        let root = tempfile::tempdir().unwrap();
        let index = Index::open(root.path()).unwrap();
        let put = |agent: &str, id: &str, source: ThreadSource, origin: Option<&str>| {
            let summary = crate::service::summary(
                agent,
                verre_agent_api::SessionRecord {
                    native_id: id.into(),
                    title: format!("History {id}"),
                    preview: None,
                    cwd: None,
                    created_at: None,
                    updated_at: 100,
                    source,
                    origin: origin.map(str::to_owned),
                    model: None,
                    resumable: origin.is_none(),
                    parent_native_id: None,
                    internal: false,
                },
            );
            index.mark(&summary, None, id, &summary.title).unwrap();
        };
        put("codex", "local-quick", ThreadSource::Desktop, None);
        put("antigravity", "coding-cloud", ThreadSource::Cloud, None);
        put(
            "codex",
            "local-catalog",
            ThreadSource::Desktop,
            Some("Codex desktop catalog (local)"),
        );
        put(
            "codex",
            "cloud-stub",
            ThreadSource::Cloud,
            Some("Codex desktop catalog (cloud)"),
        );
        put(
            "codex",
            "remote-stub",
            ThreadSource::Desktop,
            Some("Codex desktop catalog (remote)"),
        );
        put(
            "codex",
            "web-thread",
            ThreadSource::Unknown,
            Some("chatgpt"),
        );

        let default_ids: Vec<_> = index
            .list(&Filter::default(), 50, "{}")
            .unwrap()
            .into_iter()
            .map(|t| t.native_id)
            .collect();
        assert_eq!(default_ids, vec!["coding-cloud", "local-quick"]);

        let cloud_sources = [ThreadSource::Cloud];
        let cloud_ids: Vec<_> = index
            .list(
                &Filter {
                    sources: Some(&cloud_sources),
                    include_internal: true,
                    ..Default::default()
                },
                50,
                "{}",
            )
            .unwrap()
            .into_iter()
            .map(|t| t.native_id)
            .collect();
        assert_eq!(cloud_ids, vec!["coding-cloud"]);

        let all_ids: Vec<_> = index
            .list(
                &Filter {
                    include_internal: true,
                    ..Default::default()
                },
                50,
                "{}",
            )
            .unwrap()
            .into_iter()
            .map(|t| t.native_id)
            .collect();
        assert_eq!(
            all_ids,
            vec!["coding-cloud", "local-catalog", "local-quick"]
        );

        let mut search_ids: Vec<_> = index
            .search("History", 50, "{}", None, None, |_| true)
            .unwrap()
            .into_iter()
            .map(|hit| hit.thread.native_id)
            .collect();
        search_ids.sort();
        assert_eq!(search_ids, all_ids);
    }

    #[test]
    fn normalizes_stale_error_status_in_persisted_index_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("index.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch("PRAGMA user_version = 1;").unwrap();
            let mut summary = ThreadSummary {
                id: "devin:s1".into(),
                agent_id: "devin".into(),
                native_id: "s1".into(),
                title: "Finished chat".into(),
                preview: None,
                cwd: None,
                created_at: Some(1),
                updated_at: 2,
                source: ThreadSource::Verre,
                origin: None,
                status: ThreadStatus::Error,
                live: false,
                resumable: true,
                parent: None,
                child_count: 0,
                model: None,
                archived: false,
                pinned: false,
                internal: false,
            };
            let encoded = serde_json::to_string(&summary).unwrap();
            conn.execute(
                "INSERT INTO threads(id,agent,cwd,source,updated,archived,summary,internal)
                 VALUES (?1,'devin',NULL,'verre',2,0,?2,0)",
                params!["devin:s1", encoded],
            )
            .unwrap();
            summary.id = "devin:s2".into();
            summary.native_id = "s2".into();
            summary.status = ThreadStatus::Running;
            summary.live = true;
            let encoded = serde_json::to_string(&summary).unwrap();
            conn.execute(
                "INSERT INTO threads(id,agent,cwd,source,updated,archived,summary,internal)
                 VALUES (?1,'devin',NULL,'verre',3,0,?2,0)",
                params!["devin:s2", encoded],
            )
            .unwrap();
        }
        let index = Index::open(dir.path()).unwrap();
        for id in ["devin:s1", "devin:s2"] {
            let loaded = index.get(id).unwrap().unwrap();
            assert_eq!(loaded.status, ThreadStatus::Idle);
            assert!(!loaded.live);
        }
    }

    #[test]
    fn migrates_v2_links_with_current_source_and_timestamp_rules() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("index.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        let mut summary = crate::service::summary(
            "devin",
            verre_agent_api::SessionRecord {
                native_id: "parent".into(),
                title: "Parent".into(),
                preview: None,
                cwd: None,
                created_at: Some(20),
                updated_at: 35,
                source: ThreadSource::Cli,
                origin: None,
                model: None,
                resumable: true,
                parent_native_id: None,
                internal: false,
            },
        );
        save(&conn, &summary, false).unwrap();
        let link = ThreadParent {
            thread_id: summary.id,
            call_id: Some("call".into()),
            confidence: verre_proto::LinkConfidence::Exact,
        };
        let evidence = serde_json::json!({
            "id": "call",
            "targets": [],
            "ids": ["desktop", "older", "valid"],
            "command": "Read an existing log",
            "cwd": null,
            "start": null,
            "end": null
        });
        conn.execute(
            "INSERT INTO calls VALUES (?1, ':call', NULL, ?2)",
            params![link.thread_id, evidence.to_string()],
        )
        .unwrap();
        for (native_id, source, created_at) in [
            ("desktop", ThreadSource::Desktop, 30),
            ("older", ThreadSource::Exec, 10),
            ("valid", ThreadSource::Exec, 25),
        ] {
            summary.id = format!("claude:{native_id}");
            summary.agent_id = "claude".into();
            summary.native_id = native_id.into();
            summary.source = source;
            summary.created_at = Some(created_at);
            save(&conn, &summary, false).unwrap();
            conn.execute(
                "INSERT INTO call_sessions VALUES (?1, ?2, ':call')",
                params![format!(":{native_id}"), link.thread_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO links VALUES (?1, ?2, ?3)",
                params![summary.id, link.thread_id, json(&link).unwrap()],
            )
            .unwrap();
        }
        drop(conn);

        let mut reader =
            Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let snapshot = reader.transaction().unwrap();
        assert!(parent(&snapshot, "claude:desktop").unwrap().is_some());
        for _ in 0..2 {
            let index = Index::open(dir.path()).unwrap();
            assert!(index.links().unwrap().is_empty());
            let version: u32 = index
                .0
                .lock()
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 4);
        }
        assert!(parent(&snapshot, "claude:desktop").unwrap().is_some());
        snapshot.commit().unwrap();
        assert!(parent(&reader, "claude:desktop").unwrap().is_none());
    }

    #[test]
    fn migrates_v3_by_scheduling_transcript_reindexing_without_prompt_heuristics() {
        let dir = tempfile::tempdir().unwrap();
        let index = Index::open(dir.path()).unwrap();
        let thread = crate::service::summary(
            "kiro",
            verre_agent_api::SessionRecord {
                native_id: "sess_automated".into(),
                title: "Descriptive catalog title".into(),
                preview: None,
                cwd: None,
                created_at: None,
                updated_at: 42,
                source: verre_proto::ThreadSource::Unknown,
                origin: None,
                model: None,
                resumable: true,
                parent_native_id: None,
                internal: false,
            },
        );
        index.mark(&thread, None, "catalog", &thread.title).unwrap();
        {
            let conn = index.0.lock().unwrap();
            conn.execute(
                "UPDATE threads SET prompt=?2,indexed=mark WHERE id=?1",
                params![
                    thread.id,
                    "  <caller_constraints>delegated</caller_constraints>"
                ],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
        }
        drop(index);

        let index = Index::open(dir.path()).unwrap();
        let migrated = index.get(&thread.id).unwrap().unwrap();
        assert!(!migrated.internal);
        assert_eq!(migrated.source, verre_proto::ThreadSource::Unknown);
        assert_eq!(index.marks("kiro").unwrap(), [(thread.id, None, None)]);
    }
}
