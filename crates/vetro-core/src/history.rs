use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, UNIX_EPOCH},
};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::{sync::mpsc, task::JoinSet, time};
use vetro_agent_api::{Agent, AgentError, Result, SessionRecord, Transcript};
use vetro_proto::{
    rpc::{ErrorCode, ServerEvent},
    ThreadSummary,
};

use crate::{internal, links::Projection, now, service::summary, storage::Changes, Core};

mod native;

pub(crate) type Mark = (String, Option<String>, Option<String>);

struct Entry {
    thread: ThreadSummary,
    hint: Option<String>,
    mark: String,
}

impl Core {
    pub(crate) async fn history_worker(self: Arc<Self>) {
        let mut jobs = JoinSet::new();
        for agent in self.registry.all().cloned() {
            let core = self.clone();
            jobs.spawn(async move { core.watch_agent(agent).await });
        }
        jobs.spawn(self.clone().live_index_worker());
        while let Some(job) = jobs.join_next().await {
            if let Err(error) = job {
                self.home.log(&format!("History worker stopped: {error}"));
            }
        }
    }

    async fn watch_agent(&self, agent: Arc<dyn Agent>) {
        let id = agent.id();
        let paths = self
            .registry
            .history_paths
            .get(id)
            .cloned()
            .unwrap_or_default();
        let (wake, mut changed) = mpsc::channel(1);
        let agent_id = id.to_owned();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_err()
                    || event.is_ok_and(|event| {
                        !matches!(event.kind, EventKind::Access(_))
                            && event.paths.iter().any(|path| relevant(&agent_id, path))
                    })
                {
                    let _ = wake.try_send(());
                }
            })
            .ok();
        let mut watched = HashSet::new();
        loop {
            if let Some(watcher) = &mut watcher {
                for path in &paths {
                    if !watched.contains(path)
                        && watcher.watch(path, RecursiveMode::Recursive).is_ok()
                    {
                        watched.insert(path.clone());
                    }
                }
            }
            let pending = self.sync_agent(&agent, Some(Duration::from_secs(3))).await;
            let interval = if pending {
                Duration::from_millis(250)
            } else if paths.is_empty() || id == "devin" {
                Duration::from_secs(3)
            } else {
                match id {
                    "antigravity" | "codex" => Duration::from_secs(5),
                    _ => Duration::from_secs(15),
                }
            };
            tokio::select! {
                _ = time::sleep(interval) => {},
                _ = self.history_requested.notified() => {},
                Some(()) = changed.recv() => {
                    time::sleep(Duration::from_millis(350)).await;
                    while changed.try_recv().is_ok() {}
                },
            }
        }
    }

    pub async fn refresh_history(self: &Arc<Self>) {
        let mut jobs = JoinSet::new();
        for agent in self.registry.all() {
            let core = self.clone();
            let agent = Arc::clone(agent);
            jobs.spawn(async move { core.sync_agent(&agent, None).await });
        }
        while jobs.join_next().await.is_some() {}
    }

    async fn sync_agent(&self, agent: &Arc<dyn Agent>, budget: Option<Duration>) -> bool {
        if self.check_agent_enabled(agent.id()).is_err() {
            return false;
        }
        let started = Instant::now();
        match self.sync(agent, budget).await {
            Ok((total, indexed, failed, pending)) => {
                if (indexed > 0 && !pending) || failed > 0 {
                    self.home.log(&format!(
                        "History {}: {total} sessions, {indexed} transcripts indexed, {failed} failed ({:.2?})",
                        agent.id(), started.elapsed()
                    ));
                }
                pending
            }
            Err(error) => {
                self.home
                    .log(&format!("History refresh for {}: {error}", agent.id()));
                false
            }
        }
    }

    async fn sync(
        &self,
        agent: &Arc<dyn Agent>,
        budget: Option<Duration>,
    ) -> Result<(usize, usize, usize, bool)> {
        let id = agent.id();
        let store = self.store.clone();
        let agent_id = id.to_owned();
        let stored = blocking(move || store.marks(&agent_id)).await?;
        let mut known: HashMap<_, _> = stored
            .into_iter()
            .map(|(id, mark, indexed)| (id, (mark, indexed)))
            .collect();
        let paths = self
            .registry
            .history_paths
            .get(id)
            .cloned()
            .unwrap_or_default();
        let roots = paths.clone();
        let agent_id = id.to_owned();
        let inventory = blocking(move || native::inventory(&agent_id, &roots)).await?;
        let list = if inventory.authoritative {
            Vec::new()
        } else {
            let _guard = self.registry.enter(id, false).await;
            match time::timeout(Duration::from_secs(30), list(agent)).await {
                Ok(Ok(list)) => list,
                Ok(Err(error)) if inventory.files.is_empty() => return Err(error),
                Err(error) if inventory.files.is_empty() => return Err(internal(error)),
                _ => Vec::new(),
            }
        };
        let mut entries = HashMap::new();
        for record in list {
            let mark = record_mark(&record);
            insert(&mut entries, id, record, false, mark);
        }
        let total = entries.len()
            + inventory
                .files
                .keys()
                .filter(|native| !entries.contains_key(&format!("{id}:{native}")))
                .count();
        let mut scanned = 0;
        let mut indexed = 0;
        let mut failed = 0;
        let mut changes = Changes::default();
        let mut pending = Vec::new();
        for (native_id, file) in inventory.files {
            let thread_id = format!("{id}:{native_id}");
            if let Some(entry) = entries.get_mut(&thread_id) {
                entry.mark.push_str(&file.mark);
            } else if let Some(record) = file.record {
                insert(&mut entries, id, record, file.archived, file.mark);
            } else if known
                .get(&thread_id)
                .is_some_and(|(mark, _)| mark.as_ref() == Some(&file.mark))
            {
                let needs_transcript = known
                    .remove(&thread_id)
                    .is_some_and(|(mark, indexed)| mark != indexed);
                if needs_transcript {
                    if let Some(thread) = self.store.get(&thread_id)? {
                        pending.push((thread, file.mark));
                    }
                } else {
                    scanned += 1;
                }
            } else if let Some(path) = file.path {
                let source = id.to_owned();
                let mark = file.mark;
                if let Some(record) =
                    blocking(move || native::file_record(&source, &path, native_id)).await?
                {
                    insert(&mut entries, id, record, file.archived, mark);
                }
            }
        }
        let mut entries: Vec<_> = entries.into_values().collect();
        entries.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.thread.updated_at));
        for entry in entries {
            let previous = known.remove(&entry.thread.id).unwrap_or_default();
            if previous.0.as_ref() != Some(&entry.mark) {
                let title = self.overlay(entry.thread.clone()).title;
                let store = self.store.clone();
                let mark = entry.mark.clone();
                let hint = entry.hint;
                let thread = entry.thread.clone();
                let updated =
                    blocking(move || store.mark(&thread, hint.as_deref(), &mark, &title)).await?;
                changes.upserted.extend(updated.upserted);
                if changes.upserted.len() >= 32 {
                    self.publish_history(std::mem::take(&mut changes));
                }
            }
            if previous.1.as_ref() != Some(&entry.mark) {
                pending.push((entry.thread, entry.mark));
            } else {
                scanned += 1;
            }
        }
        let removed: Vec<_> = known
            .into_iter()
            .filter_map(|(id, (mark, _))| mark.map(|_| id))
            .collect();
        if !removed.is_empty() && inventory.authoritative {
            let store = self.store.clone();
            let removed = blocking(move || store.remove(removed)).await?;
            changes.upserted.extend(removed.upserted);
            changes.removed.extend(removed.removed);
        }
        self.publish_history(changes);
        let may_read = id != "devin" || self.history_authenticated.read().unwrap().contains(id);
        if may_read {
            if !pending.is_empty() {
                self.progress(id, scanned, total, false);
            }
            pending.sort_unstable_by_key(|(thread, _)| {
                (
                    self.sessions.get(&thread.id).is_none(),
                    std::cmp::Reverse(thread.updated_at),
                )
            });
            let started = Instant::now();
            for (offset, (thread, mark)) in pending.into_iter().enumerate() {
                if offset > 0 && budget.is_some_and(|budget| started.elapsed() >= budget) {
                    self.progress(id, scanned, total, false);
                    return Ok((total, indexed, failed, true));
                }
                if self
                    .sessions
                    .get(&thread.id)
                    .is_some_and(|thread| thread.is_live())
                {
                    scanned += 1;
                    continue;
                }
                let read = {
                    let _guard = self.registry.enter(id, false).await;
                    time::timeout(
                        Duration::from_secs(30),
                        self.read_history_items(agent, &thread),
                    )
                    .await
                };
                match read {
                    Ok(Ok(transcript)) => {
                        let title = self.overlay(thread.clone()).title;
                        let store = self.store.clone();
                        let saved = thread.clone();
                        let (changes, transcript) = blocking(move || {
                            let projection = Projection::from_history(&saved, &transcript.items);
                            let changes = store.index(&saved, Some(&mark), &projection, &title)?;
                            Ok((changes, transcript))
                        })
                        .await?;
                        if let Some(actor) = self.sessions.get(&thread.id) {
                            actor.refresh_history(
                                self.store.get(&thread.id)?.unwrap_or(thread),
                                transcript.items,
                            );
                        }
                        self.publish_history(changes);
                        indexed += 1;
                    }
                    Ok(Err(error)) if id == "devin" && error.code == ErrorCode::AuthRequired => {
                        self.history_authenticated.write().unwrap().remove(id);
                        break;
                    }
                    Ok(Err(error)) => {
                        failed += 1;
                        if failed <= 3 {
                            self.home
                                .log(&format!("Indexing {} failed: {:?}", thread.id, error.code));
                        }
                    }
                    Err(_) => failed += 1,
                }
                scanned += 1;
                if scanned % 16 == 0 {
                    self.progress(id, scanned, total, false);
                }
                tokio::task::yield_now().await;
            }
        }
        self.progress(id, total, total, true);
        Ok((total, indexed, failed, false))
    }

    pub(crate) async fn read_history_items(
        &self,
        agent: &Arc<dyn Agent>,
        summary: &ThreadSummary,
    ) -> Result<Transcript> {
        if agent.id() == "devin" && !self.history_authenticated.read().unwrap().contains("devin") {
            return Err(AgentError::new(
                ErrorCode::AuthRequired,
                "Sign in to Devin to read its history",
            ));
        }
        if agent.id() == "codex"
            && summary
                .origin
                .as_deref()
                .is_some_and(|origin| origin.starts_with("Codex desktop catalog ("))
        {
            return Ok(Transcript::default());
        }
        let read = agent.read(&summary.native_id, summary.cwd.as_deref().map(Path::new));
        let result = if agent.id() == "codex" {
            time::timeout(Duration::from_secs(8), read)
                .await
                .map_err(internal)
                .and_then(std::convert::identity)
        } else {
            read.await
        };
        #[cfg(feature = "codex")]
        if agent.id() == "codex"
            && result
                .as_ref()
                .map_or(true, |transcript| transcript.items.is_empty())
        {
            if let Some(root) = self
                .registry
                .history_paths
                .get("codex")
                .and_then(|roots| roots.first())
                .cloned()
            {
                let id = summary.native_id.clone();
                if let Some(transcript) =
                    blocking(move || native::codex_fallback(&root, &id)).await?
                {
                    return Ok(transcript);
                }
            }
        }
        result
    }

    async fn live_index_worker(self: Arc<Self>) {
        let mut interval = time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            for actor in self.sessions.snapshot() {
                let store = self.store.clone();
                let core = self.clone();
                match blocking(move || {
                    let Some((thread, projection)) = actor.take_projection() else {
                        return Ok(Changes::default());
                    };
                    let title = core.overlay(thread.clone()).title;
                    store.index(&thread, None, &projection, &title)
                })
                .await
                {
                    Ok(changes) => self.publish_history(changes),
                    Err(error) => self.home.log(&format!("Indexing live history: {error}")),
                }
            }
            if self
                .project_refresh_requested
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                self.publish_projects();
            }
        }
    }

    fn progress(&self, id: &str, scanned: usize, total: usize, done: bool) {
        self.publish(ServerEvent::IndexProgress {
            agent_id: id.into(),
            scanned: scanned.try_into().unwrap_or(u32::MAX),
            total: total.try_into().unwrap_or(u32::MAX),
            done,
        });
    }

    fn publish_history(&self, changes: Changes) {
        if changes.upserted.is_empty() && changes.removed.is_empty() {
            return;
        }
        let mut upserted = Vec::new();
        let mut projects = !changes.removed.is_empty();
        for summary in changes.upserted {
            if let Some(actor) = self.sessions.get(&summary.id) {
                actor.refresh_links(&summary);
            }
            projects |= summary.cwd.is_some();
            upserted.push(self.overlay(summary));
        }
        if !upserted.is_empty() {
            self.publish(ServerEvent::ThreadsUpserted { threads: upserted });
        }
        if !changes.removed.is_empty() {
            self.publish(ServerEvent::ThreadsRemoved {
                thread_ids: changes.removed,
            });
        }
        if projects {
            self.project_refresh_requested
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

async fn list(agent: &Arc<dyn Agent>) -> Result<Vec<SessionRecord>> {
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = None;
    loop {
        let page = agent.list(cursor).await?;
        records.extend(
            page.sessions
                .into_iter()
                .filter(|record| !record.native_id.is_empty()),
        );
        let Some(next) = page.next_cursor else {
            return Ok(records);
        };
        if !seen.insert(next.clone()) {
            return Err(internal("Agent returned a repeated history cursor"));
        }
        cursor = Some(next);
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(internal)?
}

fn insert(
    entries: &mut HashMap<String, Entry>,
    id: &str,
    record: SessionRecord,
    archived: bool,
    mark: String,
) {
    let hint = record.parent_native_id.as_ref().map(|parent| {
        if parent.contains(':') {
            parent.clone()
        } else {
            format!("{id}:{parent}")
        }
    });
    let mut thread = summary(id, record);
    thread.archived = archived;
    entries.insert(thread.id.clone(), Entry { thread, hint, mark });
}

fn record_mark(record: &SessionRecord) -> String {
    let value = serde_json::json!([
        record.updated_at,
        record.created_at,
        record.title,
        record.source,
        record.origin,
        record.cwd,
        record.resumable,
        record.parent_native_id,
        record.model,
        record.preview
    ]);
    value.to_string()
}

fn relevant(id: &str, path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match id {
        "claude" => {
            name.ends_with(".jsonl") || name.ends_with(".meta.json") || path.extension().is_none()
        }
        "codex" => {
            name.starts_with("state_")
                || name.starts_with("thread_history_")
                || name.starts_with("rollout-")
                || name.starts_with("codex.db")
                || name.starts_with("codex-dev.db")
        }
        "antigravity" => {
            name.ends_with(".db") || name.ends_with(".db-wal") || name.ends_with(".jsonl")
        }
        _ => true,
    }
}

fn file_mark(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!(":{}:{}", modified.as_nanos(), metadata.len()))
}

fn modified_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or_else(now, |time| time.as_millis() as i64)
}
