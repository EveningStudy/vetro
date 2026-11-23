use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use tokio::sync::{broadcast, Mutex, Notify};
use vetro_agent_api::{Agent, AgentError, Result, SessionRecord};
use vetro_proto::{rpc::*, *};

use crate::{
    bad_request, files,
    home::Home,
    internal, not_found, page_limit,
    registry::Registry,
    sessions::Sessions,
    storage::{MemoryStore, ThreadStore},
};

pub struct Core {
    pub home: Home,
    pub registry: Registry,
    pub(crate) store: Arc<dyn ThreadStore>,
    pub(crate) sessions: Sessions,
    pub(crate) events: broadcast::Sender<Arc<ServerEvent>>,
    detection: RwLock<Option<Vec<AgentInfo>>>,
    detection_refresh: Mutex<()>,
    history_ready: AtomicBool,
    history_refresh: Mutex<()>,
    history_requested: Notify,
}

impl Core {
    pub fn new(home: Home, registry: Registry) -> Arc<Self> {
        Self::with_store(home, registry, Arc::new(MemoryStore::default()))
    }

    pub fn with_store(home: Home, registry: Registry, store: Arc<dyn ThreadStore>) -> Arc<Self> {
        Arc::new(Self {
            home,
            registry,
            store,
            sessions: Sessions::default(),
            events: broadcast::channel(1024).0,
            detection: RwLock::new(None),
            detection_refresh: Mutex::new(()),
            history_ready: AtomicBool::new(false),
            history_refresh: Mutex::new(()),
            history_requested: Notify::new(),
        })
    }

    pub(crate) async fn history_worker(self: Arc<Self>) {
        let mut timer = tokio::time::interval(Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = timer.tick() => {},
                _ = self.history_requested.notified() => {},
            }
            self.refresh_history(false).await;
        }
    }

    pub(crate) fn request_history_refresh(&self) {
        self.history_requested.notify_one();
    }

    pub(crate) async fn session_worker(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let minutes = self.home.settings().idle_release_minutes;
            let idle = (minutes != 0).then(|| Duration::from_secs(u64::from(minutes) * 60));
            self.sessions.maintenance(idle).await;
            if let Some(idle) = idle {
                self.registry
                    .release_idle(idle, |id| self.sessions.has_live(id))
                    .await;
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut tasks = tokio::task::JoinSet::new();
        for thread in self.sessions.snapshot() {
            tasks.spawn(async move { thread.shutdown().await });
        }
        while tasks.join_next().await.is_some() {}
        self.registry.shutdown().await;
    }

    pub(crate) fn broadcast(&self, event: Arc<ServerEvent>) {
        let _ = self.events.send(event);
    }

    pub(crate) fn publish(&self, event: ServerEvent) {
        self.broadcast(Arc::new(event));
    }

    pub(crate) fn app_info(&self, remote_client: bool) -> AppInfo {
        AppInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            platform: std::env::consts::OS.into(),
            hostname: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "localhost".into()),
            home_dir: self.home.user_home().to_string_lossy().into_owned(),
            data_dir: self.home.root().to_string_lossy().into_owned(),
            remote_client,
        }
    }

    pub(crate) fn check_agent_enabled(&self, id: &str) -> Result<()> {
        if self
            .home
            .settings()
            .agents
            .get(id)
            .is_some_and(|config| !config.enabled)
        {
            return Err(AgentError::new(
                ErrorCode::AgentUnavailable,
                format!("Agent is disabled: {id}"),
            ));
        }
        Ok(())
    }

    pub async fn agents(&self, refresh: bool) -> Vec<AgentInfo> {
        if !refresh {
            if let Some(agents) = self.detection.read().unwrap().as_ref() {
                return agents.clone();
            }
        }
        let _refresh = self.detection_refresh.lock().await;
        if !refresh {
            if let Some(agents) = self.detection.read().unwrap().as_ref() {
                return agents.clone();
            }
        }
        let previous = self.detection.read().unwrap().clone().unwrap_or_default();
        let mut tasks = tokio::task::JoinSet::new();
        let mut agents = Vec::new();
        for agent in self.registry.all() {
            if self.check_agent_enabled(agent.id()).is_err() {
                let info = previous
                    .iter()
                    .find(|info| info.id == agent.id())
                    .cloned()
                    .unwrap_or_else(|| crate::registry::disabled_info(agent.id()));
                agents.push(AgentInfo {
                    error: Some("Disabled in settings".into()),
                    ..info
                });
                continue;
            }
            let agent = agent.clone();
            let guard = self.registry.enter(agent.id(), true).await;
            tasks.spawn(async move {
                let _guard = guard;
                agent.info().await
            });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(info) => agents.push(info),
                Err(error) => self.home.log(&format!("Agent detection failed: {error}")),
            }
        }
        agents.sort_by(|left, right| left.id.cmp(&right.id));
        *self.detection.write().unwrap() = Some(agents.clone());
        self.publish(ServerEvent::AgentsUpdated {
            agents: agents.clone(),
        });
        agents
    }

    pub(crate) async fn agent_info(&self, id: &str) -> Result<AgentInfo> {
        self.agents(false)
            .await
            .into_iter()
            .find(|agent| agent.id == id)
            .ok_or_else(|| {
                AgentError::new(
                    ErrorCode::AgentUnavailable,
                    format!("Agent unavailable: {id}"),
                )
            })
    }

    pub(crate) async fn authenticate(&self, params: AuthenticateParams) -> Result<()> {
        self.check_agent_enabled(&params.agent_id)?;
        let result = {
            let agent = self.registry.get(&params.agent_id)?;
            let _guard = self.registry.enter(&params.agent_id, true).await;
            agent.authenticate(&params.method_id).await
        };
        self.auth_result(&params.agent_id, result).await?;
        self.set_agent_auth(&params.agent_id, AuthState::Ok).await;
        Ok(())
    }

    pub(crate) async fn auth_result<T>(&self, id: &str, result: Result<T>) -> Result<T> {
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::AuthRequired)
        {
            self.set_agent_auth(id, AuthState::Required).await;
        }
        result
    }

    pub(crate) async fn set_agent_auth(&self, id: &str, state: AuthState) {
        self.agents(false).await;
        let _refresh = self.detection_refresh.lock().await;
        let mut cache = self.detection.write().unwrap();
        let Some(agents) = cache.as_mut() else { return };
        if let Some(agent) = agents.iter_mut().find(|agent| agent.id == id) {
            agent.auth.state = state;
            self.publish(ServerEvent::AgentsUpdated {
                agents: agents.clone(),
            });
        }
    }

    pub(crate) async fn agent_options(
        &self,
        params: AgentOptionsParams,
    ) -> Result<AgentOptionsResult> {
        self.check_agent_enabled(&params.agent_id)?;
        let agent = self.registry.get(&params.agent_id)?;
        let cwd = params
            .cwd
            .as_deref()
            .map(Path::new)
            .map(files::directory)
            .transpose()?;
        let key = serde_json::to_string(&(&params.agent_id, cwd.as_ref())).map_err(internal)?;
        if let Some(cached) = self
            .home
            .read_state(|state| state.options.get(&key).cloned())
        {
            return Ok(cached);
        }
        let result = {
            let _guard = self.registry.enter(agent.id(), true).await;
            agent.options(cwd.as_deref()).await
        };
        let options = self.auth_result(agent.id(), result).await?;
        let result = AgentOptionsResult {
            options: options.options,
            commands: options.commands,
        };
        self.home
            .change_state(|state| state.options.insert(key, result.clone()))?;
        Ok(result)
    }

    pub(crate) fn update_settings(&self, params: SettingsUpdateParams) -> Result<Settings> {
        let updated = self.home.update_settings(params.patch)?;
        *self.detection.write().unwrap() = None;
        self.home.change_state(|state| state.options.clear())?;
        Ok(updated)
    }

    pub async fn refresh_history(&self, only_if_cold: bool) {
        if only_if_cold && self.history_ready.load(Ordering::Acquire) {
            return;
        }
        let _refresh = self.history_refresh.lock().await;
        if only_if_cold && self.history_ready.load(Ordering::Acquire) {
            return;
        }
        let mut lists = tokio::task::JoinSet::new();
        for agent in self.registry.all().cloned() {
            if self.check_agent_enabled(agent.id()).is_err() {
                self.apply_history(agent.id(), Vec::new());
                continue;
            }
            let agent_id = agent.id().to_owned();
            let guard = self.registry.enter(&agent_id, false).await;
            let events = self.events.clone();
            lists.spawn(async move {
                let _guard = guard;
                let result =
                    tokio::time::timeout(Duration::from_secs(30), read_history(agent, events))
                        .await;
                (
                    agent_id,
                    result.map_err(internal).and_then(std::convert::identity),
                )
            });
        }
        while let Some(result) = lists.join_next().await {
            match result {
                Ok((agent_id, Ok(threads))) => self.apply_history(&agent_id, threads),
                Ok((agent_id, Err(error))) => self
                    .home
                    .log(&format!("History refresh for {agent_id}: {error}")),
                Err(error) => self.home.log(&format!("History refresh failed: {error}")),
            }
        }
        self.history_ready.store(true, Ordering::Release);
    }

    fn apply_history(&self, agent_id: &str, threads: Vec<ThreadSummary>) {
        let changes = self.store.replace_history(agent_id, threads);
        if changes.upserted.is_empty() && changes.removed.is_empty() {
            return;
        }
        if !changes.upserted.is_empty() {
            self.publish(ServerEvent::ThreadsUpserted {
                threads: changes
                    .upserted
                    .into_iter()
                    .map(|thread| self.overlay(thread))
                    .collect(),
            });
        }
        if !changes.removed.is_empty() {
            self.publish(ServerEvent::ThreadsRemoved {
                thread_ids: changes.removed,
            });
        }
        self.publish_projects();
    }

    pub(crate) fn overlay(&self, mut thread: ThreadSummary) -> ThreadSummary {
        self.home.read_state(|state| {
            if let Some(overlay) = state.threads.get(&thread.id) {
                if let Some(title) = &overlay.title {
                    thread.title.clone_from(title);
                }
                if let Some(archived) = overlay.archived {
                    thread.archived = archived;
                }
            }
        });
        thread
    }

    pub(crate) fn thread_changed(&self, thread: ThreadSummary) {
        self.store.upsert_attached(thread.clone());
        self.publish(ServerEvent::ThreadsUpserted {
            threads: vec![self.overlay(thread)],
        });
    }

    pub(crate) fn update_thread(&self, params: ThreadUpdateParams) -> Result<ThreadSummary> {
        let existing = self.store.get(&params.thread_id);
        let actor = self.sessions.get(&params.thread_id);
        if existing.is_none() && actor.is_none() {
            return Err(not_found("Unknown thread"));
        }
        self.home.change_state(|state| {
            let overlay = state.threads.entry(params.thread_id.clone()).or_default();
            if params.title.is_some() {
                overlay.title = params.title;
            }
            if params.archived.is_some() {
                overlay.archived = params.archived;
            }
        })?;
        if let Some(actor) = actor {
            return Ok(actor.overlay_changed());
        }
        let thread = self.overlay(existing.unwrap());
        self.publish(ServerEvent::ThreadsUpserted {
            threads: vec![thread.clone()],
        });
        Ok(thread)
    }

    pub fn threads(&self, params: ThreadsListParams) -> Result<ThreadsListResult> {
        let cursor: Option<(i64, String)> = params
            .cursor
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| bad_request("Invalid thread cursor"))?;
        let project = params.project_path.map(|path| {
            files::normalize(Path::new(&path))
                .to_string_lossy()
                .into_owned()
        });
        let limit = page_limit(params.limit, 100, 500);
        let mut threads: Vec<_> = self
            .sorted_threads()
            .into_iter()
            .filter(|thread| {
                (params.include_archived.unwrap_or(false) || !thread.archived)
                    && project
                        .as_ref()
                        .is_none_or(|path| thread.cwd.as_ref() == Some(path))
                    && params
                        .agent_ids
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&thread.agent_id))
                    && params
                        .sources
                        .as_ref()
                        .is_none_or(|sources| sources.contains(&thread.source))
                    && params.parent_id.as_ref().is_none_or(|id| {
                        thread
                            .parent
                            .as_ref()
                            .is_some_and(|parent| parent.thread_id == *id)
                    })
                    && (params.include_children.unwrap_or(false)
                        || params.parent_id.is_some()
                        || thread.parent.is_none())
                    && cursor.as_ref().is_none_or(|(at, id)| {
                        thread.updated_at < *at || (thread.updated_at == *at && thread.id > *id)
                    })
            })
            .take(limit + 1)
            .collect();
        let next_cursor = if threads.len() > limit {
            threads.pop();
            let last = threads.last().unwrap();
            Some(serde_json::to_string(&(last.updated_at, &last.id)).map_err(internal)?)
        } else {
            None
        };
        Ok(ThreadsListResult {
            threads,
            next_cursor,
        })
    }

    fn sorted_threads(&self) -> Vec<ThreadSummary> {
        let mut threads: Vec<_> = self
            .store
            .all()
            .into_iter()
            .map(|thread| self.overlay(thread))
            .collect();
        threads.sort_unstable_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        threads
    }

    pub(crate) fn search(&self, params: SearchParams) -> SearchResult {
        let query = params.query.trim().to_lowercase();
        let hits = self
            .sorted_threads()
            .into_iter()
            .filter(|thread| !thread.archived && thread.title.to_lowercase().contains(&query))
            .take(page_limit(params.limit, 50, 500))
            .map(|thread| SearchHit {
                snippet: snippet(&thread.title, &query),
                thread,
            })
            .collect();
        SearchResult { hits }
    }

    pub(crate) fn projects(&self) -> Vec<Project> {
        let mut projects = BTreeMap::new();
        for thread in self.store.all() {
            let Some(path) = thread.cwd else { continue };
            let project = projects
                .entry(path.clone())
                .or_insert_with(|| project_at(path));
            project.thread_count += 1;
            project.last_activity_at = project.last_activity_at.max(Some(thread.updated_at));
        }
        self.home.read_state(|state| {
            for (path, overlay) in &state.projects {
                if overlay.added {
                    projects
                        .entry(path.clone())
                        .or_insert_with(|| project_at(path.clone()));
                }
                if let Some(project) = projects.get_mut(path) {
                    if let Some(name) = &overlay.name {
                        project.name.clone_from(name);
                    }
                    project.pinned = overlay.pinned;
                    project.hidden = overlay.hidden;
                }
            }
        });
        let mut projects: Vec<_> = projects.into_values().collect();
        projects.sort_by(|left, right| {
            right
                .pinned
                .cmp(&left.pinned)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
        });
        projects
    }

    pub(crate) fn add_project(&self, params: ProjectAddParams) -> Result<Project> {
        let path = files::directory(Path::new(&params.path))?
            .to_string_lossy()
            .into_owned();
        self.home.change_state(|state| {
            state.projects.entry(path.clone()).or_default().added = true;
        })?;
        self.project_changed(&path)
    }

    pub(crate) fn update_project(&self, params: ProjectUpdateParams) -> Result<Project> {
        let path = files::normalize(Path::new(&params.path))
            .to_string_lossy()
            .into_owned();
        if !self.projects().iter().any(|project| project.path == path) {
            return Err(not_found("Unknown project"));
        }
        self.home.change_state(|state| {
            let overlay = state.projects.entry(path.clone()).or_default();
            if params.name.is_some() {
                overlay.name = params.name;
            }
            if let Some(pinned) = params.pinned {
                overlay.pinned = pinned;
            }
            if let Some(hidden) = params.hidden {
                overlay.hidden = hidden;
            }
        })?;
        self.project_changed(&path)
    }

    fn project_changed(&self, path: &str) -> Result<Project> {
        let projects = self.projects();
        let project = projects
            .iter()
            .find(|project| project.path == path)
            .cloned()
            .ok_or_else(|| not_found("Unknown project"))?;
        self.publish(ServerEvent::ProjectsUpdated { projects });
        Ok(project)
    }

    pub(crate) fn publish_projects(&self) {
        self.publish(ServerEvent::ProjectsUpdated {
            projects: self.projects(),
        });
    }
}

async fn read_history(
    agent: Arc<dyn Agent>,
    events: broadcast::Sender<Arc<ServerEvent>>,
) -> Result<Vec<ThreadSummary>> {
    let mut threads = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = None;
    loop {
        let page = agent.list(cursor).await?;
        threads.extend(
            page.sessions
                .into_iter()
                .filter(|record| !record.native_id.is_empty())
                .map(|record| summary(agent.id(), record)),
        );
        let scanned = threads.len().try_into().unwrap_or(u32::MAX);
        let done = page.next_cursor.is_none();
        let _ = events.send(Arc::new(ServerEvent::IndexProgress {
            agent_id: agent.id().into(),
            scanned,
            total: scanned,
            done,
        }));
        let Some(next) = page.next_cursor else {
            return Ok(threads);
        };
        if !seen.insert(next.clone()) {
            return Err(internal("Agent returned a repeated history cursor"));
        }
        cursor = Some(next);
    }
}

fn summary(agent_id: &str, record: SessionRecord) -> ThreadSummary {
    ThreadSummary {
        id: format!("{agent_id}:{}", record.native_id),
        agent_id: agent_id.into(),
        native_id: record.native_id,
        title: record.title,
        preview: record.preview,
        cwd: record
            .cwd
            .map(|path| files::normalize(&path).to_string_lossy().into_owned()),
        created_at: record.created_at,
        updated_at: record.updated_at,
        source: record.source,
        origin: record.origin,
        status: ThreadStatus::Idle,
        live: false,
        resumable: record.resumable,
        parent: None,
        child_count: 0,
        model: record.model,
        archived: false,
    }
}

fn project_at(path: String) -> Project {
    let folder = Path::new(&path);
    Project {
        name: folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone()),
        exists: folder.is_dir(),
        path,
        pinned: false,
        hidden: false,
        thread_count: 0,
        last_activity_at: None,
    }
}

fn snippet(title: &str, query: &str) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    if query.is_empty() {
        return escape(title);
    }
    let lower = title.to_lowercase();
    if let Some(start) = lower.find(query) {
        let end = start + query.len();
        if lower.len() == title.len()
            && title.is_char_boundary(start)
            && title.is_char_boundary(end)
        {
            return format!(
                "{}<mark>{}</mark>{}",
                escape(&title[..start]),
                escape(&title[start..end]),
                escape(&title[end..])
            );
        }
    }
    escape(title)
}
