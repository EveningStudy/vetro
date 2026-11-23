use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{atomic::AtomicBool, Arc, RwLock},
    time::Duration,
};

use tokio::sync::{broadcast, Mutex, Notify};
use vetro_agent_api::{AgentError, Result, SessionRecord};
use vetro_proto::{rpc::*, *};

use crate::{
    bad_request, files,
    home::Home,
    internal, not_found, page_limit,
    registry::Registry,
    sessions::Sessions,
    storage::{Filter, Index},
};

pub struct Core {
    pub home: Home,
    pub registry: Registry,
    pub(crate) store: Arc<Index>,
    pub(crate) sessions: Sessions,
    pub(crate) events: broadcast::Sender<Arc<ServerEvent>>,
    detection: RwLock<Option<Vec<AgentInfo>>>,
    detection_refresh: Mutex<()>,
    pub(crate) history_requested: Notify,
    pub(crate) history_authenticated: RwLock<HashSet<String>>,
    pub(crate) project_refresh_requested: AtomicBool,
}

impl Core {
    pub fn new(home: Home, registry: Registry) -> Arc<Self> {
        let store = Index::open(home.root()).unwrap_or_else(|error| {
            home.log(&format!("Index unavailable: {error}"));
            Index::temporary()
        });
        Arc::new(Self {
            home,
            registry,
            store: Arc::new(store),
            sessions: Sessions::default(),
            events: broadcast::channel(1024).0,
            detection: RwLock::new(None),
            detection_refresh: Mutex::new(()),
            history_requested: Notify::new(),
            history_authenticated: RwLock::new(HashSet::new()),
            project_refresh_requested: AtomicBool::new(false),
        })
    }

    pub(crate) fn request_history_refresh(&self) {
        self.history_requested.notify_waiters();
    }

    pub(crate) async fn session_worker(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let minutes = self.home.settings().idle_release_minutes;
            let idle = (minutes != 0).then(|| Duration::from_secs(u64::from(minutes) * 60));
            self.sessions.maintenance(idle).await;
            if let Some(idle) = idle {
                self.registry
                    .release_idle(
                        idle,
                        |id| self.sessions.has_live(id),
                        |id| {
                            self.history_authenticated.write().unwrap().remove(id);
                        },
                    )
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

    pub async fn authenticate(&self, params: AuthenticateParams) -> Result<()> {
        self.check_agent_enabled(&params.agent_id)?;
        let agent = self.registry.get(&params.agent_id)?;
        let result = {
            let _guard = self.registry.enter(&params.agent_id, true).await;
            let result = agent.authenticate(&params.method_id).await;
            if result.is_ok() && params.agent_id == "devin" {
                self.history_authenticated
                    .write()
                    .unwrap()
                    .insert(params.agent_id.clone());
            }
            result
        };
        self.auth_result(&params.agent_id, result).await?;
        self.request_history_refresh();
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
        let overlaid = self.overlay(thread.clone());
        if let Err(error) = self.store.attached(&thread, &overlaid.title) {
            self.home
                .log(&format!("Could not save attached thread: {error}"));
        }
        self.publish(ServerEvent::ThreadsUpserted {
            threads: vec![overlaid],
        });
    }

    pub(crate) fn update_thread(&self, params: ThreadUpdateParams) -> Result<ThreadSummary> {
        let existing = self.store.get(&params.thread_id)?;
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
        if let Some(existing) = &existing {
            let title = self.overlay(existing.clone()).title;
            self.store.rename(&params.thread_id, &title)?;
        }
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
        let filter = Filter {
            cwd: project.as_deref(),
            agents: params.agent_ids.as_deref(),
            sources: params.sources.as_deref(),
            parent: params.parent_id.as_deref(),
            children: params.include_children.unwrap_or(false),
            archived: params.include_archived.unwrap_or(false),
            cursor,
        };
        let mut threads = self.store.list(&filter, limit + 1, &self.overlay_json()?)?;
        let next_cursor = if threads.len() > limit {
            threads.pop();
            let last = threads.last().unwrap();
            Some(serde_json::to_string(&(last.updated_at, &last.id)).map_err(internal)?)
        } else {
            None
        };
        Ok(ThreadsListResult {
            threads: threads
                .into_iter()
                .map(|thread| self.overlay(thread))
                .collect(),
            next_cursor,
        })
    }

    fn overlay_json(&self) -> Result<String> {
        self.home
            .read_state(|state| serde_json::to_string(&state.threads))
            .map_err(internal)
    }

    pub fn search(&self, params: SearchParams) -> Result<SearchResult> {
        self.search_threads(params, None, None)
    }

    pub fn search_threads(
        &self,
        params: SearchParams,
        agent: Option<&str>,
        parent: Option<&str>,
    ) -> Result<SearchResult> {
        let mut hits = self.store.search(
            params.query.trim(),
            page_limit(params.limit, 50, 500),
            &self.overlay_json()?,
            agent,
            parent,
        )?;
        for hit in &mut hits {
            hit.thread = self.overlay(hit.thread.clone());
        }
        Ok(SearchResult { hits })
    }

    pub fn indexed_thread(&self, id: &str) -> Result<Option<ThreadSummary>> {
        Ok(self.store.get(id)?.map(|thread| self.overlay(thread)))
    }

    pub fn links(&self) -> Result<Vec<(String, ThreadParent)>> {
        self.store.links()
    }

    pub(crate) fn projects(&self) -> Result<Vec<Project>> {
        let mut projects = BTreeMap::new();
        for (path, count, updated) in self.store.projects()? {
            let mut project = project_at(path.clone());
            project.thread_count = count;
            project.last_activity_at = Some(updated);
            projects.insert(path, project);
        }
        self.home.read_state(|state| {
            for (path, overlay) in &state.projects {
                if overlay.added || overlay.hidden {
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
                .exists
                .cmp(&left.exists)
                .then_with(|| right.pinned.cmp(&left.pinned))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(projects)
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
        if !self.projects()?.iter().any(|project| project.path == path) {
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
        let projects = self.projects()?;
        let project = projects
            .iter()
            .find(|project| project.path == path)
            .cloned()
            .ok_or_else(|| not_found("Unknown project"))?;
        self.publish(ServerEvent::ProjectsUpdated { projects });
        Ok(project)
    }

    pub(crate) fn publish_projects(&self) {
        match self.projects() {
            Ok(projects) => self.publish(ServerEvent::ProjectsUpdated { projects }),
            Err(error) => self.home.log(&format!("Could not list projects: {error}")),
        }
    }
}

pub(crate) fn summary(agent_id: &str, record: SessionRecord) -> ThreadSummary {
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
