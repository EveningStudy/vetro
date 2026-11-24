use std::{
    collections::BTreeMap,
    future::Future,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use serde_json::Value;
use tokio::sync::{broadcast, watch, Mutex, Notify};
use vetro_agent_api::{AgentError, Result, SessionRecord};
use vetro_proto::{rpc::*, *};

use crate::{
    bad_request, files,
    home::Home,
    internal, not_found, page_limit,
    registry::Registry,
    sessions::Sessions,
    storage::{Filter, Index},
    workspace,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowTheme {
    pub theme: Option<String>,
    pub background: Option<String>,
}

pub struct Core {
    pub home: Home,
    registry: RwLock<Arc<Registry>>,
    pub(crate) registry_changed: watch::Sender<()>,
    theme: watch::Sender<WindowTheme>,
    notify_tx: broadcast::Sender<NotifyParams>,
    pub(crate) store: Arc<Index>,
    pub(crate) sessions: Sessions,
    pub(crate) events: broadcast::Sender<Arc<ServerEvent>>,
    detection: RwLock<Option<Vec<AgentInfo>>>,
    detection_refresh: Mutex<()>,
    pub(crate) history_requested: Notify,
    pub(crate) project_refresh_requested: AtomicBool,
}

impl Core {
    pub fn new(home: Home, registry: Registry) -> Arc<Self> {
        let store = Index::open(home.root()).unwrap_or_else(|error| {
            home.log(&format!("Index unavailable: {error}"));
            Index::temporary()
        });
        let theme = watch::channel(window_theme(&home.prefs("local"))).0;
        Arc::new(Self {
            home,
            registry: RwLock::new(Arc::new(registry)),
            registry_changed: watch::channel(()).0,
            theme,
            notify_tx: broadcast::channel(64).0,
            store: Arc::new(store),
            sessions: Sessions::default(),
            events: broadcast::channel(1024).0,
            detection: RwLock::new(None),
            detection_refresh: Mutex::new(()),
            history_requested: Notify::new(),
            project_refresh_requested: AtomicBool::new(false),
        })
    }

    pub fn theme_watch(&self) -> watch::Receiver<WindowTheme> {
        self.theme.subscribe()
    }

    pub fn notify_subscribe(&self) -> broadcast::Receiver<NotifyParams> {
        self.notify_tx.subscribe()
    }

    pub fn emit_navigate(&self, thread_id: String) {
        self.publish(ServerEvent::AppNavigate { thread_id });
    }

    pub(crate) fn notify(&self, params: NotifyParams) {
        if self.home.settings().notifications_enabled {
            let _ = self.notify_tx.send(params);
        }
    }

    pub(crate) async fn git_status(&self, cwd: &Path, remote: bool) -> Result<GitStatusResult> {
        if !remote {
            return workspace::git_status(cwd).await;
        }
        match self.remote_repository(cwd).await? {
            Some(repo) => repo.status().await,
            None => Ok(GitStatusResult::default()),
        }
    }

    pub(crate) async fn git_diff(
        &self,
        cwd: &Path,
        path: &str,
        remote: bool,
    ) -> Result<GitDiffResult> {
        if !remote {
            return workspace::git_diff(cwd, path).await;
        }
        self.remote_repository(cwd)
            .await?
            .ok_or_else(|| bad_request("Workspace is not inside a Git work tree"))?
            .diff(path)
            .await
    }

    async fn remote_repository(&self, cwd: &Path) -> Result<Option<workspace::Repository>> {
        let repo = workspace::Repository::discover(cwd).await?;
        if let Some(repo) = &repo {
            check_project_path(repo.root(), &self.projects()?)?;
        }
        Ok(repo)
    }

    pub(crate) async fn open_external(&self, params: OpenExternalParams) -> Result<()> {
        let editor = self.home.settings().external_editor;
        tokio::task::spawn_blocking(move || workspace::open_external(params, editor.as_deref()))
            .await
            .map_err(internal)?
    }

    pub(crate) fn prefs_set(&self, device_id: &str, prefs: Value) -> Result<()> {
        self.home.change_state(|state| {
            state.prefs.insert(device_id.into(), prefs);
        })?;
        if device_id == "local" {
            self.home.read_state(|state| {
                let theme = window_theme(&state.prefs["local"]);
                self.theme.send_if_modified(|current| {
                    if *current == theme {
                        return false;
                    }
                    *current = theme;
                    true
                });
            });
        }
        Ok(())
    }

    pub(crate) fn request_history_refresh(&self) {
        self.history_requested.notify_waiters();
    }

    pub fn registry(&self) -> Arc<Registry> {
        self.registry.read().unwrap().clone()
    }

    pub(crate) async fn session_worker(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let minutes = self.home.settings().idle_release_minutes;
            let idle = (minutes != 0).then(|| Duration::from_secs(u64::from(minutes) * 60));
            self.sessions.maintenance(idle).await;
            self.registry()
                .release_idle(
                    |id| self.sessions.has_live(id),
                    idle,
                    |id| self.update_cached_auth(id, AuthState::Required),
                )
                .await;
        }
    }

    pub async fn shutdown(&self) {
        let mut tasks = tokio::task::JoinSet::new();
        for thread in self.sessions.snapshot() {
            tasks.spawn(async move { thread.shutdown().await });
        }
        while tasks.join_next().await.is_some() {}
        self.registry().shutdown().await;
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
                return self.current_auth(agents.clone());
            }
        }
        let _refresh = self.detection_refresh.lock().await;
        if !refresh {
            if let Some(agents) = self.detection.read().unwrap().as_ref() {
                return self.current_auth(agents.clone());
            }
        }
        let previous = self.detection.read().unwrap().clone().unwrap_or_default();
        let mut tasks = tokio::task::JoinSet::new();
        let mut agents = Vec::new();
        let registry = self.registry();
        for agent in registry.all() {
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
            let Ok(guard) = registry.enter(agent.id(), true).await else {
                continue;
            };
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
        let agents = self.current_auth(agents);
        *self.detection.write().unwrap() = Some(agents.clone());
        self.publish(ServerEvent::AgentsUpdated {
            agents: agents.clone(),
        });
        agents
    }

    fn current_auth(&self, mut agents: Vec<AgentInfo>) -> Vec<AgentInfo> {
        for info in &mut agents {
            if let Ok(agent) = self.registry().get(&info.id) {
                if let Some(auth) = agent.process_auth() {
                    info.auth.state = auth;
                }
            }
        }
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

    pub async fn authenticate(&self, params: AuthenticateParams) -> Result<AuthenticateResult> {
        self.check_agent_enabled(&params.agent_id)?;
        let registry = self.registry();
        let agent = registry.get(&params.agent_id)?;
        let result = {
            let _guard = registry.enter(&params.agent_id, true).await?;
            agent.authenticate(&params.method_id).await
        };
        let result = self.auth_result(&params.agent_id, result).await?;
        self.request_history_refresh();
        if params.agent_id != "codex" {
            self.set_agent_auth(&params.agent_id, AuthState::Ok).await;
        }
        Ok(result)
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
        let _refresh = self.detection_refresh.lock().await;
        self.update_cached_auth(id, state);
    }

    fn update_cached_auth(&self, id: &str, state: AuthState) {
        let mut cache = self.detection.write().unwrap();
        let Some(agents) = cache.as_mut() else { return };
        if let Some(agent) = agents.iter_mut().find(|agent| agent.id == id) {
            if agent.auth.state == state {
                return;
            }
            agent.auth.state = state;
            self.publish(ServerEvent::AgentsUpdated {
                agents: agents.clone(),
            });
        }
    }

    pub(crate) async fn user_agent_call<T, F: Future<Output = Result<T>>>(
        &self,
        id: &str,
        call: impl FnMut() -> F,
    ) -> Result<T> {
        let registry = self.registry();
        self.user_agent_call_on(&registry, id, call).await
    }

    pub(crate) async fn user_agent_call_on<T, F: Future<Output = Result<T>>>(
        &self,
        registry: &Registry,
        id: &str,
        mut call: impl FnMut() -> F,
    ) -> Result<T> {
        let agent = registry.get(id)?;
        let _guard = registry.enter(id, true).await?;
        let result = call().await;
        if !result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::AuthRequired)
        {
            return result;
        }
        self.update_cached_auth(id, AuthState::Required);
        if agent.recover_auth().await? {
            self.update_cached_auth(id, AuthState::Ok);
            self.request_history_refresh();
            let result = call().await;
            if result
                .as_ref()
                .is_err_and(|error| error.code == ErrorCode::AuthRequired)
            {
                self.update_cached_auth(id, AuthState::Required);
            }
            return result;
        }
        result
    }

    pub(crate) async fn agent_options(
        &self,
        params: AgentOptionsParams,
    ) -> Result<AgentOptionsResult> {
        self.check_agent_enabled(&params.agent_id)?;
        let registry = self.registry();
        let agent = registry.get(&params.agent_id)?;
        let cwd = match params
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(path) => Some(files::directory(Path::new(path))?),
            None => Some(self.home.scratch_root()?),
        };
        let info = self.agent_info(agent.id()).await?;
        let selected = if info.kind == AgentKind::Acp {
            BTreeMap::new()
        } else {
            params.selected.unwrap_or_default()
        };
        let version = info.version;
        let key = option_key(agent.id(), version.as_deref(), cwd.as_deref(), &selected)?;
        if let Some(cached) = self
            .home
            .read_state(|state| state.options.get(&key).cloned())
        {
            return Ok(cached);
        }
        if agent.options_need_session() && params.prepare != Some(true) {
            return Ok(AgentOptionsResult {
                options: Vec::new(),
                commands: Vec::new(),
                needs_session: true,
            });
        }
        let options = self
            .user_agent_call_on(&registry, agent.id(), || async {
                if agent.options_need_session() {
                    agent.prepare_options(cwd.as_deref(), &selected).await
                } else {
                    agent.options(cwd.as_deref(), &selected).await
                }
            })
            .await?;
        let result = AgentOptionsResult {
            options: options.options,
            commands: options.commands,
            needs_session: false,
        };
        self.home
            .change_state(|state| state.options.insert(key, result.clone()))?;
        Ok(result)
    }

    pub(crate) fn cached_thread_options(&self, summary: &ThreadSummary) -> AgentOptionsResult {
        let detection = self.detection.read().unwrap();
        let info = detection
            .as_ref()
            .and_then(|agents| agents.iter().find(|info| info.id == summary.agent_id));
        self.home.read_state(|state| {
            let selected = state
                .threads
                .get(&summary.id)
                .map(|overlay| &overlay.options);
            let candidates = state.options.iter().filter_map(|(key, value)| {
                let (agent, version, cwd, choices): (
                    String,
                    Option<String>,
                    Option<String>,
                    BTreeMap<String, String>,
                ) = serde_json::from_str(key).ok()?;
                if agent != summary.agent_id || info.is_some_and(|info| info.version != version) {
                    return None;
                }
                let matches_cwd = cwd == summary.cwd;
                let matches_model = !choices.is_empty()
                    && selected.is_some_and(|selected| {
                        choices
                            .iter()
                            .all(|(id, value)| selected.get(id) == Some(value))
                    });
                (matches_model || choices.is_empty())
                    .then_some(((matches_cwd, matches_model), value))
            });
            let mut result = candidates
                .max_by_key(|(score, _)| *score)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| AgentOptionsResult {
                    options: Vec::new(),
                    commands: Vec::new(),
                    needs_session: false,
                });
            if let Some(selected) = selected {
                for option in &mut result.options {
                    if let Some(value) = selected.get(&option.id) {
                        option.current_value.clone_from(value);
                    }
                }
            }
            result
        })
    }

    pub(crate) async fn remember_initial_options(
        &self,
        agent_id: &str,
        cwd: &Path,
        options: vetro_agent_api::AgentOptions,
    ) -> Result<()> {
        let version = self.agent_info(agent_id).await?.version;
        let key = option_key(agent_id, version.as_deref(), Some(cwd), &BTreeMap::new())?;
        self.home.change_state(|state| {
            state.options.insert(
                key,
                AgentOptionsResult {
                    options: options.options,
                    commands: options.commands,
                    needs_session: false,
                },
            )
        })?;
        Ok(())
    }

    pub(crate) async fn update_settings(&self, params: SettingsUpdateParams) -> Result<Settings> {
        let agents_changed =
            params.patch.get("agents").is_some() || params.patch.get("customAcpAgents").is_some();
        let refresh = self.detection_refresh.lock().await;
        let (updated, replacement) =
            self.home
                .update_settings(params.patch, |previous, current| {
                    agents_changed
                        .then(|| self.registry().reconfigure(previous, current))
                        .transpose()
                })?;
        if let Some((next, retired)) = replacement {
            retired.retire();
            *self.registry.write().unwrap() = Arc::new(next);
            *self.detection.write().unwrap() = None;
            self.registry_changed.send_replace(());
            self.home.change_state(|state| state.options.clear())?;
            tokio::spawn(async move { retired.shutdown().await });
            drop(refresh);
            self.agents(true).await;
        }
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
                if let Some(pinned) = overlay.pinned {
                    thread.pinned = pinned;
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
        if thread.cwd.is_some() {
            self.project_refresh_requested
                .store(true, Ordering::Release);
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
        let archive_changed = params.archived.is_some();
        let title_changed = params.title.is_some();
        self.home.change_state(|state| {
            let overlay = state.threads.entry(params.thread_id.clone()).or_default();
            if params.title.is_some() {
                overlay.title = params.title;
            }
            if params.archived.is_some() {
                overlay.archived = params.archived;
            }
            if params.pinned.is_some() {
                overlay.pinned = params.pinned;
            }
        })?;
        if archive_changed {
            self.project_refresh_requested
                .store(true, Ordering::Release);
        }
        if let Some(existing) = existing.as_ref().filter(|_| title_changed) {
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
            include_internal: params.include_internal.unwrap_or(false),
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
        let hidden: Vec<_> = if params.include_hidden.unwrap_or(false) {
            Vec::new()
        } else {
            self.home.read_state(|state| {
                state
                    .projects
                    .iter()
                    .filter(|(_, project)| project.hidden)
                    .map(|(path, _)| files::normalize(Path::new(path)))
                    .collect()
            })
        };
        let mut hits = self.store.search(
            params.query.trim(),
            page_limit(params.limit, 50, 500),
            &self.overlay_json()?,
            agent,
            parent,
            |thread| {
                hidden.is_empty()
                    || thread.cwd.as_ref().is_none_or(|cwd| {
                        let cwd = files::normalize(Path::new(cwd));
                        !hidden.iter().any(|root| cwd.starts_with(root))
                    })
            },
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
        for activity in self.store.projects(&self.overlay_json()?)? {
            let mut project = project_at(activity.path.clone());
            project.thread_count = activity.thread_count;
            project.last_activity_at = Some(activity.last_activity_at);
            project.root_thread_count = activity.root_thread_count;
            project.last_root_activity_at = activity.last_root_activity_at;
            projects.insert(activity.path, project);
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

    pub(crate) fn remove_project(&self, params: ProjectAddParams) -> Result<()> {
        let path = files::normalize(Path::new(&params.path))
            .to_string_lossy()
            .into_owned();
        let project = self
            .projects()?
            .into_iter()
            .find(|project| project.path == path)
            .ok_or_else(|| not_found("Unknown project"))?;
        if project.thread_count > 0 {
            return Err(bad_request(
                "Sessions still point at this folder, so it stays listed. Hide it instead.",
            ));
        }
        let removed = self.home.change_state(|state| {
            if state
                .projects
                .get(&path)
                .is_some_and(|overlay| overlay.added)
            {
                state.projects.remove(&path);
                true
            } else {
                false
            }
        })?;
        if !removed {
            return Err(bad_request("Only manually added projects can be removed"));
        }
        self.publish_projects();
        Ok(())
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

fn option_key(
    agent: &str,
    version: Option<&str>,
    cwd: Option<&Path>,
    selected: &BTreeMap<String, String>,
) -> Result<String> {
    serde_json::to_string(&(agent, version, cwd, selected)).map_err(internal)
}

pub(crate) fn summary(agent_id: &str, record: SessionRecord) -> ThreadSummary {
    let record = record.classify(agent_id);
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
        pinned: false,
        internal: record.internal,
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
        root_thread_count: 0,
        last_root_activity_at: None,
    }
}

pub(crate) fn in_project(path: &Path, projects: &[Project]) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    projects.iter().any(|project| {
        std::fs::canonicalize(&project.path).is_ok_and(|root| path.starts_with(root))
    })
}

pub(crate) fn check_project_path(path: &Path, projects: &[Project]) -> Result<()> {
    if in_project(path, projects) {
        return Ok(());
    }
    Err(AgentError::new(
        ErrorCode::Unauthorized,
        "Path is outside known project roots",
    ))
}

fn window_theme(prefs: &Value) -> WindowTheme {
    WindowTheme {
        theme: prefs
            .get("theme")
            .and_then(Value::as_str)
            .map(str::to_owned),
        background: prefs
            .get("themeBg")
            .and_then(Value::as_str)
            .filter(|color| {
                color.strip_prefix('#').is_some_and(|hex| {
                    hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
            .map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn theme_watch_tracks_only_local_preferences() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        home.change_state(|state| {
            state
                .prefs
                .insert("local".into(), json!({"theme":"dark", "themeBg":"#f5edd8"}))
        })
        .unwrap();
        let core = Core::new(home, Registry::default());
        let mut theme = core.theme_watch();
        assert_eq!(theme.borrow().theme.as_deref(), Some("dark"));
        assert_eq!(theme.borrow().background.as_deref(), Some("#f5edd8"));
        core.prefs_set("remote", json!({"theme":"light", "themeBg":"#ffffff"}))
            .unwrap();
        assert!(!theme.has_changed().unwrap());
        for expected in ["light", "system"] {
            core.prefs_set("local", json!({"theme":expected, "themeBg":"#f5edd8"}))
                .unwrap();
            assert!(theme.has_changed().unwrap());
            assert_eq!(theme.borrow_and_update().theme.as_deref(), Some(expected));
        }
        core.prefs_set("local", json!({"theme":"system", "themeBg":"#FEF9EF"}))
            .unwrap();
        assert!(theme.has_changed().unwrap());
        assert_eq!(
            theme.borrow_and_update().background.as_deref(),
            Some("#FEF9EF")
        );
        for (invalid, changed) in [("red", true), ("#12345", false)] {
            core.prefs_set("local", json!({"theme":"system", "themeBg":invalid}))
                .unwrap();
            assert_eq!(theme.has_changed().unwrap(), changed);
            assert_eq!(theme.borrow_and_update().background, None);
        }
        core.prefs_set("local", json!({})).unwrap();
        assert_eq!(*theme.borrow_and_update(), WindowTheme::default());
    }

    #[test]
    fn thread_pin_persists_in_local_state_and_publishes() {
        let root = tempfile::tempdir().unwrap();
        let app_dir = root.path().join(".vetro");
        let home = Home::open(app_dir.clone(), root.path().into()).unwrap();
        let core = Core::new(home, Registry::default());
        let thread = summary(
            "devin",
            SessionRecord {
                native_id: "stored".into(),
                title: "Original".into(),
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
            },
        );
        core.store.mark(&thread, None, "1", &thread.title).unwrap();
        let mut events = core.events.subscribe();
        let update = |core: &Core, pinned| {
            core.update_thread(ThreadUpdateParams {
                thread_id: thread.id.clone(),
                pinned,
                title: None,
                archived: None,
            })
            .unwrap()
        };
        assert!(update(&core, Some(true)).pinned);
        assert!(
            matches!(&*events.try_recv().unwrap(), ServerEvent::ThreadsUpserted { threads } if threads[0].pinned)
        );
        assert!(!core.store.get(&thread.id).unwrap().unwrap().pinned);
        let state: Value =
            serde_json::from_slice(&std::fs::read(app_dir.join("state.json")).unwrap()).unwrap();
        assert_eq!(state["threads"][&thread.id]["pinned"], true);
        drop(core);
        let home = Home::open(app_dir, root.path().into()).unwrap();
        let core = Core::new(home, Registry::default());
        assert!(core.threads(ThreadsListParams::default()).unwrap().threads[0].pinned);
        assert!(update(&core, None).pinned);
        assert!(!update(&core, Some(false)).pinned);
    }

    #[tokio::test]
    async fn desktop_notification_respects_settings_and_emits_navigation() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let core = Core::new(home, Registry::default());
        let mut notifications = core.notify_subscribe();
        let params = NotifyParams {
            title: "Finished".into(),
            body: "Done".into(),
            thread_id: Some("devin:stored".into()),
        };
        core.notify(params.clone());
        assert_eq!(
            notifications.try_recv().unwrap().thread_id,
            params.thread_id
        );
        core.update_settings(SettingsUpdateParams {
            patch: json!({"notificationsEnabled": false}),
        })
        .await
        .unwrap();
        core.notify(params);
        assert!(matches!(
            notifications.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let mut events = core.events.subscribe();
        core.emit_navigate("devin:stored".into());
        assert!(
            matches!(&*events.try_recv().unwrap(), ServerEvent::AppNavigate { thread_id } if thread_id == "devin:stored")
        );
    }

    #[test]
    fn thread_visibility_and_project_root_activity() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let core = Core::new(home, Registry::default());
        let records = [
            ("root", ThreadSource::Cli, None, "Root", None, None, false),
            ("exec", ThreadSource::Exec, None, "Exec", None, None, false),
            (
                "compact",
                ThreadSource::Unknown,
                Some("subAgentCompact"),
                "Compact",
                None,
                Some("codex:root"),
                false,
            ),
            (
                "wrapper",
                ThreadSource::Cli,
                None,
                "Wrapper",
                Some("<caller_constraints>\nrun"),
                None,
                false,
            ),
            (
                "projection",
                ThreadSource::Unknown,
                Some("history projection"),
                "Codex thread",
                None,
                None,
                false,
            ),
            (
                "child",
                ThreadSource::Cli,
                None,
                "Interactive child",
                None,
                Some("codex:root"),
                false,
            ),
            (
                "archived",
                ThreadSource::Cli,
                None,
                "Archived",
                None,
                None,
                true,
            ),
        ];
        for (position, (id, source, origin, title, preview, parent, archived)) in
            records.into_iter().enumerate()
        {
            let mut thread = summary(
                "codex",
                SessionRecord {
                    native_id: id.into(),
                    title: title.into(),
                    preview: preview.map(str::to_owned),
                    cwd: Some(root.path().into()),
                    created_at: None,
                    updated_at: position as i64 + 10,
                    source,
                    origin: origin.map(str::to_owned),
                    model: None,
                    resumable: true,
                    parent_native_id: None,
                    internal: false,
                },
            );
            thread.archived = archived;
            core.store.mark(&thread, parent, id, title).unwrap();
        }
        let list = |params| core.threads(params).unwrap().threads;
        let roots = list(ThreadsListParams::default());
        assert_eq!(
            roots
                .iter()
                .map(|thread| thread.native_id.as_str())
                .collect::<Vec<_>>(),
            ["root"]
        );
        let all = list(ThreadsListParams {
            include_internal: Some(true),
            include_children: Some(true),
            ..Default::default()
        });
        let internal = all
            .iter()
            .filter(|thread| thread.internal)
            .map(|thread| thread.native_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(internal, ["projection", "wrapper", "compact", "exec"]);
        let exec = list(ThreadsListParams {
            sources: Some(vec![ThreadSource::Exec]),
            ..Default::default()
        });
        assert_eq!(exec[0].native_id, "exec");
        let children = list(ThreadsListParams {
            parent_id: Some("codex:root".into()),
            ..Default::default()
        });
        assert_eq!(children.len(), 2);
        assert!(children[1].internal);
        assert_eq!(children[1].parent.as_ref().unwrap().thread_id, "codex:root");
        assert_eq!(core.links().unwrap().len(), 2);

        let project = core.projects().unwrap().pop().unwrap();
        assert_eq!(
            (project.thread_count, project.last_activity_at),
            (7, Some(16))
        );
        assert_eq!(
            (project.root_thread_count, project.last_root_activity_at),
            (1, Some(10))
        );
        core.update_thread(ThreadUpdateParams {
            thread_id: "codex:root".into(),
            title: None,
            archived: Some(true),
            pinned: None,
        })
        .unwrap();
        let project = core.projects().unwrap().pop().unwrap();
        assert_eq!(
            (project.root_thread_count, project.last_root_activity_at),
            (0, None)
        );
    }

    #[cfg(feature = "acp")]
    #[tokio::test]
    async fn settings_add_and_remove_custom_agents_without_restart() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let core = Core::new(home, Registry::default());
        let mut events = core.events.subscribe();
        let command = root.path().join("missing-custom-agent");
        let patch = json!({
            "agents": {
                "codex": {"enabled": false}, "claude": {"enabled": false},
                "devin": {"enabled": false}, "antigravity": {"enabled": false}
            },
            "customAcpAgents": [{"id": "custom", "name": "Custom", "command": command}]
        });
        core.update_settings(SettingsUpdateParams { patch })
            .await
            .unwrap();
        assert!(core.registry().get("custom").is_ok());
        let event = events.recv().await.unwrap();
        assert!(matches!(&*event, ServerEvent::AgentsUpdated { agents }
            if agents.iter().any(|agent| agent.id == "custom")));
        core.update_settings(SettingsUpdateParams {
            patch: json!({"customAcpAgents": []}),
        })
        .await
        .unwrap();
        assert!(core.registry().get("custom").is_err());
        let event = events.recv().await.unwrap();
        assert!(matches!(&*event, ServerEvent::AgentsUpdated { agents }
            if agents.iter().all(|agent| agent.id != "custom")));
    }
}
