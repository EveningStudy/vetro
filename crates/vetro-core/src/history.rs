use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, UNIX_EPOCH},
};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::{sync::mpsc, task::JoinSet, time};
use vetro_agent_api::{Agent, Result, SessionRecord, Transcript};
use vetro_proto::{
    rpc::{ErrorCode, ServerEvent},
    ThreadSummary,
};

use crate::{
    internal, links::Projection, now, registry::Registry, service::summary, storage::Changes, Core,
};

mod native;

pub(crate) type Mark = (String, Option<String>, Option<String>);

struct Entry {
    thread: ThreadSummary,
    hint: Option<String>,
    mark: String,
}

impl Core {
    pub(crate) async fn history_worker(self: Arc<Self>) {
        let mut workers = JoinSet::new();
        workers.spawn(self.clone().live_index_worker());
        let mut changed = self.registry_changed.subscribe();
        loop {
            let registry = self.registry();
            let mut jobs = JoinSet::new();
            for agent in registry.all().cloned() {
                let core = self.clone();
                let registry = registry.clone();
                jobs.spawn(async move { core.watch_agent(&registry, agent).await });
            }
            loop {
                tokio::select! {
                    _ = changed.changed() => break,
                    Some(job) = jobs.join_next(), if !jobs.is_empty() => {
                        if let Err(error) = job {
                            self.home.log(&format!("History worker stopped: {error}"));
                        }
                    }
                }
            }
            jobs.shutdown().await;
        }
    }

    async fn watch_agent(&self, registry: &Registry, agent: Arc<dyn Agent>) {
        let id = agent.id();
        let paths = registry.history_paths.get(id).cloned().unwrap_or_default();
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
        let mut requested = true;
        let mut pending = false;
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
            if requested
                || pending
                || !paths.is_empty()
                || registry.background_poll_due(id)
                || self.sessions.has_open(id)
            {
                pending = self
                    .sync_agent(registry, &agent, Some(Duration::from_secs(3)))
                    .await;
            }
            requested = false;
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
                _ = self.history_requested.notified() => requested = true,
                Some(()) = changed.recv() => {
                    time::sleep(Duration::from_millis(350)).await;
                    while changed.try_recv().is_ok() {}
                    requested = true;
                },
            }
        }
    }

    pub async fn refresh_history(self: &Arc<Self>) {
        let mut jobs = JoinSet::new();
        let registry = self.registry();
        for agent in registry.all() {
            let core = self.clone();
            let agent = Arc::clone(agent);
            let registry = registry.clone();
            jobs.spawn(async move { core.sync_agent(&registry, &agent, None).await });
        }
        while jobs.join_next().await.is_some() {}
    }

    async fn sync_agent(
        &self,
        registry: &Registry,
        agent: &Arc<dyn Agent>,
        budget: Option<Duration>,
    ) -> bool {
        if self.check_agent_enabled(agent.id()).is_err() {
            return false;
        }
        let started = Instant::now();
        match self.sync(registry, agent, budget).await {
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
        registry: &Registry,
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
        let paths = registry.history_paths.get(id).cloned().unwrap_or_default();
        let allow_protocol = budget.is_none()
            || registry.background_poll_due(id)
            || (paths.is_empty() && self.sessions.has_open(id));
        let roots = paths.clone();
        let agent_id = id.to_owned();
        let inventory = blocking(move || native::inventory(&agent_id, &roots)).await?;
        for error in &inventory.errors {
            self.home.log(&format!("History {id}: {error}"));
        }
        let disk_complete = inventory.errors.is_empty();
        let (list, authoritative) = if inventory.authoritative {
            (Vec::new(), disk_complete)
        } else if !allow_protocol {
            agent.list_without_runtime().await?.map_or_else(
                || (Vec::new(), false),
                |page| {
                    let complete = id == "antigravity"
                        && inventory.antigravity_disk_scanned
                        && disk_complete
                        && !page.incomplete
                        && page.next_cursor.is_none();
                    (page.sessions, complete)
                },
            )
        } else {
            let result = {
                let _guard = registry.enter(id, false).await?;
                time::timeout(Duration::from_secs(30), list(agent)).await
            };
            match result {
                Ok(Ok((list, complete))) => (
                    list,
                    complete
                        && disk_complete
                        && if id == "antigravity" {
                            inventory.antigravity_disk_scanned
                        } else {
                            inventory.files.is_empty() && !matches!(id, "claude" | "codex")
                        },
                ),
                Ok(Err(error)) => {
                    if error.code == ErrorCode::AuthRequired {
                        self.set_agent_auth(id, vetro_proto::AuthState::Required)
                            .await;
                    }
                    if inventory.files.is_empty() {
                        return Err(error);
                    }
                    (Vec::new(), false)
                }
                Err(error) if inventory.files.is_empty() => return Err(internal(error)),
                _ => (Vec::new(), false),
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
            if id == "antigravity" && !entries.contains_key(&thread_id) {
                known.remove(&thread_id);
            }
            if let Some(entry) = entries.get_mut(&thread_id) {
                entry.thread.internal |= file.record.as_ref().is_some_and(|record| {
                    record.internal && record.origin.as_deref() != Some("history projection")
                });
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
        if authoritative && !known.is_empty() {
            let store = self.store.clone();
            let removed: Vec<_> = known
                .into_keys()
                .filter(|id| self.sessions.get(id).is_none())
                .collect();
            if !removed.is_empty() {
                let removed = blocking(move || store.remove(removed)).await?;
                changes.upserted.extend(removed.upserted);
                changes.removed.extend(removed.removed);
            }
        }
        self.publish_history(changes);
        let may_read = id != "devin" || agent.process_auth() == Some(vetro_proto::AuthState::Ok);
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
                let _history_guard = if agent.history_edit_capabilities().rollback_conversation {
                    Some(self.history_edits.read().await)
                } else {
                    None
                };
                if self
                    .sessions
                    .get(&thread.id)
                    .is_some_and(|thread| thread.is_live() || thread.history_reload_needed())
                {
                    scanned += 1;
                    continue;
                }
                let read = {
                    let _guard = if allow_protocol {
                        Some(registry.enter(id, false).await?)
                    } else {
                        None
                    };
                    time::timeout(
                        Duration::from_secs(30),
                        self.read_for_index(agent, &thread, allow_protocol),
                    )
                    .await
                };
                match read {
                    Ok(Ok(Some(transcript))) => {
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
                                transcript,
                            );
                        }
                        self.publish_history(changes);
                        indexed += 1;
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) if id == "devin" && error.code == ErrorCode::AuthRequired => {
                        self.set_agent_auth(id, vetro_proto::AuthState::Required)
                            .await;
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

    async fn read_for_index(
        &self,
        agent: &Arc<dyn Agent>,
        summary: &ThreadSummary,
        allow_protocol: bool,
    ) -> Result<Option<Transcript>> {
        if agent.id() == "devin" && agent.process_auth() != Some(vetro_proto::AuthState::Ok) {
            return Ok(None);
        }
        if allow_protocol || agent.id() == "claude" {
            return self.read_history_items(agent, summary).await.map(Some);
        }
        if self.history_rolled_back(summary) {
            return Ok(None);
        }
        #[cfg(feature = "codex")]
        if agent.id() == "codex" {
            if summary
                .origin
                .as_deref()
                .is_some_and(|origin| origin.starts_with("Codex desktop catalog ("))
            {
                return Ok(Some(Transcript::default()));
            }
            return self.read_codex_store(&summary.native_id).await;
        }
        agent.read_without_runtime(&summary.native_id).await
    }

    #[cfg(feature = "codex")]
    async fn read_codex_store(&self, native_id: &str) -> Result<Option<Transcript>> {
        let Some(root) = self
            .registry()
            .history_paths
            .get("codex")
            .and_then(|roots| roots.first())
            .cloned()
        else {
            return Ok(None);
        };
        let id = native_id.to_owned();
        blocking(move || native::codex_fallback(&root, &id)).await
    }

    pub(crate) async fn read_history_items(
        &self,
        agent: &Arc<dyn Agent>,
        summary: &ThreadSummary,
    ) -> Result<Transcript> {
        if self.history_rolled_back(summary) {
            return self.read_authoritative_history_items(agent, summary).await;
        }
        if agent.id() == "codex"
            && summary
                .origin
                .as_deref()
                .is_some_and(|origin| origin.starts_with("Codex desktop catalog ("))
        {
            return Ok(Transcript::default());
        }
        #[cfg(feature = "codex")]
        let local = if agent.id() == "codex" && !summary.live {
            match self.read_codex_store(&summary.native_id).await {
                Ok(Some(transcript)) => return Ok(transcript),
                result => result,
            }
        } else {
            Ok(None)
        };
        let result = self.read_authoritative_history_items(agent, summary).await;
        #[cfg(feature = "codex")]
        if agent.id() == "codex"
            && result
                .as_ref()
                .map_or(true, |transcript| transcript.items.is_empty())
        {
            let local = if summary.live {
                self.read_codex_store(&summary.native_id).await
            } else {
                local
            };
            if let Some(transcript) = local? {
                return Ok(transcript);
            }
        }
        result
    }

    fn history_rolled_back(&self, summary: &ThreadSummary) -> bool {
        self.home.read_state(|state| {
            state
                .threads
                .get(&summary.id)
                .is_some_and(|overlay| overlay.history_rolled_back)
        })
    }

    pub(crate) async fn read_authoritative_history_items(
        &self,
        agent: &Arc<dyn Agent>,
        summary: &ThreadSummary,
    ) -> Result<Transcript> {
        let read = agent.read(&summary.native_id, summary.cwd.as_deref().map(Path::new));
        if agent.id() == "codex" {
            time::timeout(Duration::from_secs(8), read)
                .await
                .map_err(internal)
                .and_then(std::convert::identity)
        } else {
            read.await
        }
    }

    async fn live_index_worker(self: Arc<Self>) {
        let mut interval = time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            for actor in self.sessions.snapshot() {
                let _history_guard = self.history_edits.read().await;
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
            upserted.push(self.overlay_with_live(summary));
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

async fn list(agent: &Arc<dyn Agent>) -> Result<(Vec<SessionRecord>, bool)> {
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = None;
    let mut complete = true;
    loop {
        let page = agent.list(cursor).await?;
        complete &= !page.incomplete;
        records.extend(
            page.sessions
                .into_iter()
                .filter(|record| !record.native_id.is_empty()),
        );
        let Some(next) = page.next_cursor else {
            return Ok((records, complete));
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
        record.preview,
        record.internal
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use serde_json::json;
    use vetro_agent_api::{
        async_trait, AgentError, AgentOptions, EventSink, Session, SessionEvent, SessionPage,
        StartRequest, WireLog,
    };
    use vetro_proto::{
        rpc::{
            AgentOptionsParams, AuthenticateResult, SearchParams, ThreadStartParams,
            ThreadsListParams,
        },
        AgentInfo, AuthState, ConfigOption, ContentBlock, RateLimit, RequestResolution,
        ThreadSource, ThreadStatus, ThreadUsage,
    };

    use super::*;
    use crate::home::Home;

    struct InventoryAgent {
        id: &'static str,
        auth: Mutex<Option<AuthState>>,
        listing: Mutex<Result<SessionPage>>,
        initial_usage: Option<ThreadUsage>,
    }

    #[async_trait]
    impl Agent for InventoryAgent {
        fn id(&self) -> &str {
            self.id
        }

        async fn info(&self) -> AgentInfo {
            let mut info = crate::registry::disabled_info(self.id);
            info.installed = true;
            info.auth.state = self.process_auth().unwrap_or(AuthState::Ok);
            info
        }

        fn process_auth(&self) -> Option<AuthState> {
            *self.auth.lock().unwrap()
        }

        async fn authenticate(&self, _: &str) -> Result<AuthenticateResult> {
            unreachable!()
        }

        async fn options(
            &self,
            _: Option<&Path>,
            _: &BTreeMap<String, String>,
        ) -> Result<AgentOptions> {
            unreachable!()
        }

        async fn list(&self, _: Option<String>) -> Result<SessionPage> {
            self.listing.lock().unwrap().clone()
        }

        async fn list_without_runtime(&self) -> Result<Option<SessionPage>> {
            Ok((self.id == "antigravity").then(SessionPage::default))
        }

        async fn read(&self, _: &str, _: Option<&Path>) -> Result<Transcript> {
            Ok(Transcript::default())
        }

        async fn start(&self, _: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
            if let Some(usage) = &self.initial_usage {
                sink.send(SessionEvent::Usage(usage.clone())).unwrap();
            }
            Ok(Arc::new(LiveSession))
        }

        async fn attach(
            &self,
            _: &str,
            _: Option<&Path>,
            _: EventSink,
        ) -> Result<Arc<dyn Session>> {
            unreachable!()
        }

        async fn shutdown(&self) {
            *self.auth.lock().unwrap() = None;
        }
    }

    struct LiveSession;

    #[async_trait]
    impl Session for LiveSession {
        fn native_id(&self) -> &str {
            "in-memory"
        }

        async fn send(&self, _: Vec<ContentBlock>) -> Result<()> {
            unreachable!()
        }

        async fn interrupt(&self) -> Result<()> {
            unreachable!()
        }

        async fn set_option(&self, _: &str, _: &str) -> Result<Vec<ConfigOption>> {
            unreachable!()
        }

        async fn respond(&self, _: &str, _: RequestResolution) -> Result<()> {
            unreachable!()
        }

        async fn detach(&self) {}

        fn wire_log(&self) -> Arc<WireLog> {
            Arc::new(WireLog::new(1024))
        }
    }

    fn record(id: &str) -> SessionRecord {
        SessionRecord {
            native_id: id.into(),
            title: id.into(),
            preview: None,
            cwd: None,
            created_at: None,
            updated_at: 1,
            source: ThreadSource::Cli,
            origin: None,
            model: None,
            resumable: true,
            parent_native_id: None,
            internal: false,
        }
    }

    #[tokio::test]
    async fn live_status_survives_stale_index_rows_and_history_publication() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let agent = Arc::new(InventoryAgent {
            id: "kiro",
            auth: Mutex::new(None),
            listing: Mutex::new(Ok(SessionPage::default())),
            initial_usage: None,
        });
        let core = Core::new(home, Registry::new([agent as Arc<dyn Agent>]).unwrap());
        let (live, _actor) = core
            .start(ThreadStartParams {
                agent_id: "kiro".into(),
                cwd: root.path().to_string_lossy().into_owned(),
                options: BTreeMap::new(),
                content: Vec::new(),
            })
            .await
            .unwrap();
        let mut stale = summary("kiro", record("in-memory"));
        core.store.attached(&stale, &stale.title).unwrap();
        stale.title = "Indexed session".into();
        let changes = core
            .store
            .mark(&stale, None, "new history", &stale.title)
            .unwrap();
        let saved = core.store.get(&live.id).unwrap().unwrap();
        assert!(!saved.live);
        assert_eq!(saved.status, ThreadStatus::Idle);
        assert!(saved.updated_at < live.updated_at);

        let listed = core.threads(ThreadsListParams::default()).unwrap().threads;
        let indexed = core.indexed_thread(&live.id).unwrap().unwrap();
        let hits = core
            .search(SearchParams {
                query: stale.title.clone(),
                limit: None,
                include_hidden: None,
            })
            .unwrap()
            .hits;
        let mut events = core.events.subscribe();
        core.publish_history(changes);
        let event = events.try_recv().unwrap();
        let ServerEvent::ThreadsUpserted { threads } = &*event else {
            panic!("Expected an upsert, got {event:?}");
        };
        for thread in [&listed[0], &indexed, &hits[0].thread, &threads[0]] {
            assert_eq!(thread.id, live.id);
            assert_eq!(thread.title, stale.title);
            assert_eq!(thread.updated_at, live.updated_at);
            assert_eq!(thread.status, ThreadStatus::Running);
            assert!(thread.live);
        }
    }

    #[cfg(feature = "codex")]
    #[tokio::test]
    async fn authoritative_codex_sync_prunes_legacy_catalog_stubs() {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("codex");
        std::fs::create_dir(&native).unwrap();
        let state = rusqlite::Connection::open(native.join("state_1.sqlite")).unwrap();
        state
            .execute_batch(
                "CREATE TABLE threads(id, source); INSERT INTO threads VALUES ('local','cli');",
            )
            .unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let agent = Arc::new(InventoryAgent {
            id: "codex",
            auth: Mutex::new(None),
            initial_usage: None,
            listing: Mutex::new(Err(AgentError::new(
                ErrorCode::AgentUnavailable,
                "list failed",
            ))),
        });
        let registry = Registry::new([agent as Arc<dyn Agent>])
            .unwrap()
            .watch_history("codex", vec![native]);
        let core = Core::new(home, registry);

        for (id, source, origin) in [
            (
                "cloud",
                ThreadSource::Cloud,
                "Codex desktop catalog (cloud)",
            ),
            (
                "remote",
                ThreadSource::Desktop,
                "Codex desktop catalog (remote)",
            ),
        ] {
            let mut stale = record(id);
            stale.source = source;
            stale.origin = Some(origin.into());
            stale.resumable = false;
            let thread = crate::service::summary("codex", stale);
            core.store.mark(&thread, None, "legacy", id).unwrap();
        }
        assert_eq!(core.store.marks("codex").unwrap().len(), 2);
        core.refresh_history().await;
        let marks = core.store.marks("codex").unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].0, "codex:local");
    }

    #[tokio::test]
    async fn prunes_only_after_complete_inventory_and_keeps_sessions_in_memory() {
        for id in ["kiro", "antigravity"] {
            let root = tempfile::tempdir().unwrap();
            let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
            let agent = Arc::new(InventoryAgent {
                id,
                auth: Mutex::new(None),
                initial_usage: None,
                listing: Mutex::new(Ok(SessionPage {
                    sessions: ["deleted", "in-memory", "remaining"].map(record).to_vec(),
                    ..Default::default()
                })),
            });
            let mut registry = Registry::new([agent.clone() as Arc<dyn Agent>]).unwrap();
            if id == "antigravity" {
                registry = registry.watch_history(id, vec![root.path().join("antigravity")]);
            }
            let core = Core::new(home, registry);
            core.refresh_history().await;
            let deleted = format!("{id}:deleted");
            let in_memory = format!("{id}:in-memory");
            let (live, _actor) = core
                .start(ThreadStartParams {
                    agent_id: id.into(),
                    cwd: root.path().to_string_lossy().into_owned(),
                    options: BTreeMap::new(),
                    content: Vec::new(),
                })
                .await
                .unwrap();
            assert_eq!(live.id, in_memory);
            assert!(live.live);

            *agent.listing.lock().unwrap() =
                Err(AgentError::new(ErrorCode::AgentUnavailable, "list failed"));
            core.refresh_history().await;
            assert!(core.store.get(&deleted).unwrap().is_some());

            *agent.listing.lock().unwrap() = Ok(SessionPage {
                incomplete: true,
                ..Default::default()
            });
            core.refresh_history().await;
            assert!(core.store.get(&deleted).unwrap().is_some());

            *agent.listing.lock().unwrap() = Ok(SessionPage {
                sessions: vec![record("remaining")],
                ..Default::default()
            });
            let mut events = core.events.subscribe();
            core.refresh_history().await;
            assert!(core.store.get(&deleted).unwrap().is_none());
            assert!(core.store.get(&in_memory).unwrap().is_some());
            assert!(core
                .store
                .get(&format!("{id}:remaining"))
                .unwrap()
                .is_some());
            assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
                matches!(&*event, ServerEvent::ThreadsRemoved { thread_ids } if thread_ids == std::slice::from_ref(&deleted))
            }));
        }
    }

    #[tokio::test]
    async fn idle_release_preserves_vendor_sign_in_until_auth_error() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let agent = Arc::new(InventoryAgent {
            id: "devin",
            auth: Mutex::new(Some(AuthState::Ok)),
            initial_usage: None,
            listing: Mutex::new(Ok(SessionPage::default())),
        });
        let registry = Registry::new([agent.clone() as Arc<dyn Agent>]).unwrap();
        let core = Core::new(home, registry);
        assert_eq!(core.agents(false).await[0].auth.state, AuthState::Ok);
        core.registry()
            .release_idle(|_| false, Some(Duration::ZERO))
            .await;
        assert_eq!(agent.process_auth(), None);
        assert_eq!(core.agents(false).await[0].auth.state, AuthState::Ok);
        let error = AgentError::new(ErrorCode::AuthRequired, "sign-in needed");
        assert!(core.auth_result::<()>("devin", Err(error)).await.is_err());
        assert_eq!(core.agents(false).await[0].auth.state, AuthState::Required);
    }

    #[tokio::test]
    async fn session_usage_persists_and_broadcasts_agent_rate_limits() {
        use vetro_proto::rpc::ServerEvent;

        let root = tempfile::tempdir().unwrap();
        let app_dir = root.path().join(".vetro");
        let home = Home::open(app_dir.clone(), root.path().into()).unwrap();
        let limits = vec![RateLimit {
            label: "five_hour".into(),
            used_percent: 42.0,
            resets_at: Some(1_795_392_000_000),
        }];
        let agent = Arc::new(InventoryAgent {
            id: "claude",
            auth: Mutex::new(None),
            listing: Mutex::new(Ok(SessionPage::default())),
            initial_usage: Some(ThreadUsage {
                rate_limits: limits.clone(),
                ..Default::default()
            }),
        });
        let core = Core::new(
            home,
            Registry::new([agent.clone() as Arc<dyn Agent>]).unwrap(),
        );
        assert!(core
            .agent_info("claude")
            .await
            .unwrap()
            .rate_limits
            .is_empty());

        let mut events = core.events.subscribe();
        let (_, _thread) = core
            .start(ThreadStartParams {
                agent_id: "claude".into(),
                cwd: root.path().to_string_lossy().into_owned(),
                options: BTreeMap::new(),
                content: Vec::new(),
            })
            .await
            .unwrap();
        let updated = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let ServerEvent::AgentChanged { agent } = &*events.recv().await.unwrap() {
                    break agent.clone();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(updated.id, "claude");
        assert_eq!(updated.rate_limits, limits);
        assert!(updated.rate_limits_updated_at.is_some_and(|ts| ts > 0));
        assert_eq!(core.agent_info("claude").await.unwrap(), updated);
        assert!(matches!(
            &*events.try_recv().unwrap(),
            ServerEvent::AgentsUpdated { agents } if agents == std::slice::from_ref(&updated)
        ));
        core.shutdown().await;
        drop(core);

        let home = Home::open(app_dir, root.path().into()).unwrap();
        let reloaded = Core::new(home, Registry::new([agent as Arc<dyn Agent>]).unwrap());
        assert_eq!(reloaded.agent_info("claude").await.unwrap(), updated);
    }

    #[tokio::test]
    async fn empty_rate_limit_probe_keeps_the_persisted_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let app_dir = root.path().join(".vetro");
        let home = Home::open(app_dir.clone(), root.path().into()).unwrap();
        let agent = Arc::new(InventoryAgent {
            id: "codex",
            auth: Mutex::new(None),
            listing: Mutex::new(Ok(SessionPage::default())),
            initial_usage: None,
        });
        let core = Core::new(
            home,
            Registry::new([agent.clone() as Arc<dyn Agent>]).unwrap(),
        );
        core.record_agent_rate_limits(
            "codex",
            vec![RateLimit {
                label: "codex (5h)".into(),
                used_percent: 25.0,
                resets_at: None,
            }],
        );
        let mut events = core.events.subscribe();
        let before_refresh = core.agent_info("codex").await.unwrap();
        assert_eq!(before_refresh.rate_limits[0].used_percent, 25.0);
        assert!(matches!(
            &*events.try_recv().unwrap(),
            vetro_proto::rpc::ServerEvent::AgentChanged { agent } if agent == &before_refresh
        ));
        let state_path = app_dir.join("state.json");
        let before = std::fs::read(&state_path).unwrap();
        assert!(agent.info().await.rate_limits.is_empty());
        assert_eq!(core.agents(true).await, [before_refresh]);
        assert_eq!(std::fs::read(state_path).unwrap(), before);
    }

    #[tokio::test]
    async fn old_state_options_are_normalized_in_agent_options_and_thread_open() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let agent = Arc::new(InventoryAgent {
            id: "kiro",
            auth: Mutex::new(None),
            initial_usage: None,
            listing: Mutex::new(Ok(SessionPage {
                sessions: vec![record("cached")],
                ..Default::default()
            })),
        });
        let registry = Registry::new([agent as Arc<dyn Agent>]).unwrap();
        let core = Core::new(home, registry);
        let key = json!(["kiro", null, null, {}]).to_string();
        let cached = serde_json::from_value(json!({
            "options": [{
                "id": "mode", "name": "Mode", "category": "mode", "currentValue": "spec",
                "choices": [{"name": "Autonomous", "value": "autonomous"}]
            }],
            "commands": [],
            "needsSession": false
        }))
        .unwrap();
        core.home
            .change_state(|state| state.options.insert(key, cached))
            .unwrap();
        let options = core
            .agent_options(AgentOptionsParams {
                agent_id: "kiro".into(),
                cwd: None,
                selected: None,
                prepare: None,
            })
            .await
            .unwrap();
        assert!(options.options[0].choices[0].danger);
        core.refresh_history().await;
        let open = core.open_thread("kiro:cached", None).await.unwrap();
        assert!(open.options[0].choices[0].danger);
    }
}
