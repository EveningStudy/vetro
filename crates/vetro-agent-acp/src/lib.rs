use std::{
    collections::{BTreeMap, HashMap},
    env,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use vetro_agent_api::{
    async_trait, process::Process, Agent, AgentError, AgentOptions, EventSink, Result, Session,
    SessionPage, StartRequest, Transcript, WireLog,
};
use vetro_proto::{
    rpc::{AuthenticateResult, ErrorCode},
    AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthMethod, AuthState,
};

pub use vetro_agent_api::process::SpawnSpec;

mod devin_history;
mod interaction;
mod kiro_history;
mod mapping;
mod session;
mod wire;

use mapping::{array, string};
use session::AcpSession;
use wire::{agent_error, session_error, Connection};

pub struct AcpAgent {
    id: String,
    name: String,
    spec: SpawnSpec,
    steering: bool,
    version: OnceCell<Option<String>>,
    runtime: AsyncMutex<Option<Arc<Runtime>>>,
    directories: Mutex<HashMap<String, PathBuf>>,
    connection: Mutex<Weak<Connection>>,
    previous_method: Option<Arc<Mutex<Option<String>>>>,
}

struct Runtime {
    connection: Arc<Connection>,
    capabilities: Capabilities,
    methods: Vec<AuthMethod>,
    prepared: Mutex<Option<(Instant, Arc<AcpSession>)>>,
    auth_probe: OnceCell<()>,
}

const PREPARED_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct Capabilities {
    image: bool,
    steering: bool,
    load: bool,
    resume: bool,
    list: bool,
    close: bool,
    delete: bool,
    fork: bool,
}

impl AcpAgent {
    pub fn new(id: impl Into<String>, name: impl Into<String>, spec: SpawnSpec) -> Self {
        let id = id.into();
        Self {
            steering: id == "antigravity",
            previous_method: (id == "devin").then(|| Arc::new(Mutex::new(None))),
            id,
            name: name.into(),
            spec,
            version: OnceCell::new(),
            runtime: AsyncMutex::new(None),
            directories: Mutex::new(HashMap::new()),
            connection: Mutex::new(Weak::new()),
        }
    }

    pub fn antigravity() -> Self {
        Self::new("antigravity", "Antigravity", standard_spec("agy"))
    }

    pub fn devin() -> Self {
        let mut spec = standard_spec("devin");
        spec.args
            .extend(["--process-model".into(), "in-process".into()]);
        Self::new("devin", "Devin", spec)
    }

    pub fn with_mid_turn_input(mut self, enabled: bool) -> Self {
        self.steering = enabled;
        self
    }

    pub async fn wire_log(&self) -> Option<Arc<WireLog>> {
        self.runtime
            .lock()
            .await
            .as_ref()
            .map(|runtime| runtime.connection.log())
    }

    async fn runtime(&self, cwd: Option<&Path>) -> Result<Arc<Runtime>> {
        let mut current = self.runtime.lock().await;
        if let Some(runtime) = current
            .as_ref()
            .filter(|runtime| runtime.connection.alive())
        {
            return Ok(Arc::clone(runtime));
        }
        *current = None;
        let mut spec = self.resolved_spec()?;
        if spec.cwd.is_none() {
            spec.cwd = cwd.map(Path::to_owned);
        }
        let connection = Connection::spawn(spec, self.previous_method.clone())?;
        let initial = connection
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false,
                        "elicitation": { "form": {} }
                    },
                    "clientInfo": { "name": "vetro", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await;
        let initial = match initial {
            Ok(initial) => initial,
            Err(error) => return Err(connection.startup_error(error).await),
        };
        if initial["protocolVersion"].as_u64() != Some(1) {
            return Err(AgentError::new(
                ErrorCode::AgentUnavailable,
                "Agent did not negotiate ACP v1",
            ));
        }
        let agent = &initial["agentCapabilities"];
        let session = &agent["sessionCapabilities"];
        let supports = |key: &str| session[key].is_object() || session[key].as_bool() == Some(true);
        let runtime = Arc::new(Runtime {
            connection,
            prepared: Mutex::new(None),
            auth_probe: OnceCell::new(),
            capabilities: Capabilities {
                image: agent["promptCapabilities"]["image"]
                    .as_bool()
                    .unwrap_or_default(),
                steering: self.steering
                    || agent["promptCapabilities"]["midTurnInput"]
                        .as_bool()
                        .unwrap_or_default()
                    || agent["_meta"]["cognition.ai/midTurnPrompts"].as_bool() == Some(true),
                load: agent["loadSession"].as_bool().unwrap_or_default(),
                resume: supports("resume"),
                list: supports("list"),
                close: supports("close"),
                delete: supports("delete"),
                fork: supports("fork"),
            },
            methods: array(&initial["authMethods"])
                .filter_map(|method| {
                    Some(AuthMethod {
                        id: string(method, "id")?,
                        name: string(method, "name")?,
                        description: string(method, "description"),
                    })
                })
                .collect(),
        });
        *current = Some(Arc::clone(&runtime));
        *self.connection.lock().unwrap() = Arc::downgrade(&runtime.connection);
        Ok(runtime)
    }

    fn resolved_spec(&self) -> Result<SpawnSpec> {
        let mut spec = self.spec.clone();
        spec.program = resolve(&self.spec.program).ok_or_else(|| {
            AgentError::new(
                ErrorCode::AgentUnavailable,
                format!("ACP binary not found: {}", self.spec.program.display()),
            )
        })?;
        Ok(spec)
    }

    fn cwd(&self, cwd: Option<&Path>) -> Result<PathBuf> {
        let cwd = cwd
            .or(self.spec.cwd.as_deref())
            .unwrap_or_else(|| Path::new("."));
        std::path::absolute(cwd)
            .map_err(|error| AgentError::new(ErrorCode::BadRequest, error.to_string()))
    }

    fn remember(&self, id: &str, cwd: &Path) {
        self.directories
            .lock()
            .unwrap()
            .insert(id.to_owned(), cwd.to_owned());
    }

    async fn stored_cwd(&self, runtime: &Runtime, id: &str, cwd: Option<&Path>) -> Result<PathBuf> {
        if cwd.is_some() {
            return self.cwd(cwd);
        }
        let known = self.directories.lock().unwrap().get(id).cloned();
        if let Some(known) = known {
            return self.cwd(Some(&known));
        }
        if runtime.capabilities.list {
            let mut cursor = None;
            let mut visited = std::collections::HashSet::new();
            loop {
                let page = self.list_on(runtime, cursor).await?;
                if let Some(record) = page
                    .sessions
                    .into_iter()
                    .find(|record| record.native_id == id)
                {
                    return self.cwd(record.cwd.as_deref());
                }
                cursor = page.next_cursor;
                if !cursor
                    .as_ref()
                    .is_some_and(|cursor| visited.insert(cursor.clone()))
                {
                    break;
                }
            }
        }
        self.cwd(None)
    }

    async fn list_on(&self, runtime: &Runtime, cursor: Option<String>) -> Result<SessionPage> {
        if !runtime.capabilities.list {
            return Ok(SessionPage {
                incomplete: true,
                ..Default::default()
            });
        }
        let params = cursor.map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
        let result = runtime.call("session/list", params).await?;
        let prepared = runtime
            .prepared
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, session)| session.id.clone());
        let mut sessions = array(&result["sessions"])
            .filter(|session| {
                session["sessionId"].as_str() != prepared.as_deref() || prepared.is_none()
            })
            .filter_map(|session| {
                mapping::record(
                    &self.id,
                    session,
                    runtime.capabilities.load || runtime.capabilities.resume,
                )
            })
            .collect::<Vec<_>>();
        if self.id == "kiro" {
            kiro_history::fill_created_at(&self.spec, &mut sessions).await;
        }
        for record in &sessions {
            if let Some(cwd) = &record.cwd {
                self.remember(&record.native_id, cwd);
            }
        }
        Ok(SessionPage {
            sessions,
            next_cursor: string(&result, "nextCursor").filter(|cursor| !cursor.is_empty()),
            incomplete: result["sessions"].as_array().is_none_or(|sessions| {
                sessions
                    .iter()
                    .any(|session| session["sessionId"].as_str().is_none_or(str::is_empty))
            }),
        })
    }

    async fn read_replay(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript> {
        let runtime = self.runtime(cwd).await?;
        if !runtime.capabilities.load {
            return Err(AgentError::new(
                ErrorCode::NotResumable,
                "ACP agent does not support history replay",
            ));
        }
        let cwd = self.stored_cwd(&runtime, native_id, cwd).await?;
        let _lifecycle = runtime.connection.lifecycle.lock().await;
        let session = AcpSession::new(
            Arc::clone(&runtime.connection),
            native_id.to_owned(),
            cwd.clone(),
            &runtime.capabilities,
            None,
            true,
        );
        runtime.connection.register(native_id, &session)?;
        let transcript = Arc::new(Mutex::new(None));
        let replay = Arc::clone(&transcript);
        let capture = Arc::downgrade(&session);
        let reply = runtime
            .connection
            .call_ordered(
                "session/load",
                session_params(native_id, &cwd),
                move |reply| {
                    if reply.is_ok() {
                        if let Some(session) = capture.upgrade() {
                            *replay.lock().unwrap() = Some(session.take_replay());
                        }
                    }
                },
            )
            .await;
        runtime.connection.observe_auth("session/load", &reply);
        if let Err(error) = reply {
            runtime.connection.unregister(native_id);
            return Err(session_error(error, &self.id, native_id));
        }
        runtime.close_session(native_id).await;
        let result = transcript.lock().unwrap().take().unwrap_or_default();
        Ok(result)
    }

    async fn new_session(
        &self,
        runtime: &Runtime,
        cwd: PathBuf,
        sink: Option<EventSink>,
    ) -> Result<Arc<AcpSession>> {
        let _capture = runtime.connection.capture_new();
        let result = runtime
            .call("session/new", json!({ "cwd": cwd, "mcpServers": [] }))
            .await?;
        let id = string(&result, "sessionId").ok_or_else(|| {
            AgentError::new(ErrorCode::Internal, "ACP session/new omitted sessionId")
        })?;
        let session = AcpSession::new(
            Arc::clone(&runtime.connection),
            id.clone(),
            cwd,
            &runtime.capabilities,
            sink,
            false,
        );
        session.initialize(&result);
        runtime.connection.register(&id, &session)?;
        session.capture_initial_options();
        Ok(session)
    }
}

impl Runtime {
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let result = self.connection.call(method, params).await;
        self.connection.observe_auth(method, &result);
        result.map_err(agent_error)
    }

    async fn close_session(&self, id: &str) {
        if self.capabilities.close {
            let _ = self.call("session/close", json!({ "sessionId": id })).await;
        }
        self.connection.unregister(id);
    }

    async fn discard_prepared(&self, session: Arc<AcpSession>) {
        self.close_session(&session.id).await;
        if self.capabilities.delete {
            let _ = self
                .call("session/delete", json!({ "sessionId": session.id }))
                .await;
        }
    }

    fn take_expired_prepared(&self) -> Option<Arc<AcpSession>> {
        let mut prepared = self.prepared.lock().unwrap();
        if prepared
            .as_ref()
            .is_some_and(|(created, _)| created.elapsed() >= PREPARED_TTL)
        {
            return prepared.take().map(|(_, session)| session);
        }
        None
    }
}

#[async_trait]
impl Agent for AcpAgent {
    fn id(&self) -> &str {
        &self.id
    }

    async fn info(&self) -> AgentInfo {
        let spec = self.resolved_spec();
        let installed = spec.is_ok();
        let path = spec
            .as_ref()
            .ok()
            .map(|spec| spec.program.to_string_lossy().into_owned());
        let version = match &spec {
            Ok(spec) => self
                .version
                .get_or_init(|| version(spec.clone()))
                .await
                .clone(),
            Err(_) => None,
        };
        let (auth_state, account) = match &spec {
            Ok(spec) if self.id == "devin" => devin_auth_status(spec).await,
            _ => (AuthState::Unknown, None),
        };
        let runtime = self.runtime(None).await;
        let mut info = AgentInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: AgentKind::Acp,
            installed,
            path,
            version,
            auth: AgentAuth {
                state: auth_state,
                methods: Vec::new(),
                account,
                provider: None,
                endpoint: None,
            },
            capabilities: AgentCapabilities::default(),
            error: None,
            rate_limits: Vec::new(),
            rate_limits_updated_at: None,
        };
        match runtime {
            Ok(runtime) => {
                let caps = &runtime.capabilities;
                info.capabilities = AgentCapabilities {
                    image_input: caps.image,
                    mid_turn_input: caps.steering,
                    resume: caps.load || caps.resume,
                    list_sessions: caps.list,
                    fork: vetro_proto::ForkCapabilities {
                        whole: caps.fork,
                        at_turn: false,
                    },
                    history_edit: self.history_edit_capabilities(),
                };
                info.auth.methods.clone_from(&runtime.methods);
                if self.id != "devin"
                    && caps.list
                    && *runtime.connection.auth.lock().unwrap() == AuthState::Unknown
                {
                    runtime
                        .auth_probe
                        .get_or_init(|| async {
                            let result = tokio::time::timeout(
                                Duration::from_secs(5),
                                runtime.connection.call("session/list", json!({})),
                            )
                            .await;
                            if let Ok(reply) = result {
                                runtime.connection.observe_auth("session/list", &reply);
                            }
                        })
                        .await;
                }
                let process_auth = *runtime.connection.auth.lock().unwrap();
                if process_auth != AuthState::Unknown {
                    info.auth.state = process_auth;
                }
            }
            Err(error) => info.error = Some(error.message),
        }
        info
    }

    async fn authenticate(&self, method_id: &str) -> Result<AuthenticateResult> {
        self.runtime(None)
            .await?
            .connection
            .authenticate(method_id)
            .await?;
        Ok(AuthenticateResult::default())
    }

    fn process_auth(&self) -> Option<AuthState> {
        self.previous_method.as_ref()?;
        self.connection
            .lock()
            .unwrap()
            .upgrade()
            .filter(|connection| connection.alive())
            .map(|connection| *connection.auth.lock().unwrap())
    }

    async fn recover_auth(&self) -> Result<bool> {
        let Some(previous_method) = &self.previous_method else {
            return Ok(false);
        };
        let Some(runtime) = self.runtime.lock().await.clone() else {
            return Ok(false);
        };
        let needs_method = previous_method.lock().unwrap().is_none()
            && *runtime.connection.auth.lock().unwrap() == AuthState::Required;
        let fallback = if needs_method
            && runtime
                .methods
                .iter()
                .any(|method| method.id == "devin-browser")
        {
            match self.resolved_spec() {
                Ok(spec) if devin_auth_status(&spec).await.0 == AuthState::Ok => {
                    Some("devin-browser")
                }
                _ => None,
            }
        } else {
            None
        };
        runtime.connection.recover_auth(fallback).await
    }

    fn options_need_session(&self) -> bool {
        true
    }

    async fn options(
        &self,
        _cwd: Option<&Path>,
        _selected: &BTreeMap<String, String>,
    ) -> Result<AgentOptions> {
        Ok(AgentOptions::default())
    }

    async fn prepare_options(
        &self,
        cwd: Option<&Path>,
        selected: &BTreeMap<String, String>,
    ) -> Result<AgentOptions> {
        let cwd = self.cwd(cwd)?;
        let runtime = self.runtime(Some(&cwd)).await?;
        let _lifecycle = runtime.connection.lifecycle.lock().await;
        let previous = runtime.prepared.lock().unwrap().take();
        let session = match previous {
            Some((created, session)) if session.cwd == cwd && created.elapsed() < PREPARED_TTL => {
                session
            }
            previous => {
                if let Some((_, session)) = previous {
                    runtime.discard_prepared(session).await;
                }
                self.new_session(&runtime, cwd, None).await?
            }
        };
        let applied = apply_options(&session, selected, true, true).await;
        let options = session.options();
        *runtime.prepared.lock().unwrap() = Some((Instant::now(), session));
        applied?;
        Ok(options)
    }

    async fn list(&self, cursor: Option<String>) -> Result<SessionPage> {
        if cursor.is_none() {
            if let Ok(Some(page)) = self.list_without_runtime().await {
                for record in &page.sessions {
                    if let Some(cwd) = &record.cwd {
                        self.remember(&record.native_id, cwd);
                    }
                }
                return Ok(page);
            }
        }
        let runtime = self.runtime(None).await?;
        self.list_on(&runtime, cursor).await
    }

    async fn list_without_runtime(&self) -> Result<Option<SessionPage>> {
        if self.id == "devin" {
            return devin_history::list(&self.spec).await;
        }
        Ok(None)
    }

    async fn read_without_runtime(&self, native_id: &str) -> Result<Option<Transcript>> {
        if self.id == "devin" {
            return devin_history::read(&self.spec, native_id).await;
        }
        Ok(None)
    }

    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript> {
        let snapshot = self.read_without_runtime(native_id).await;
        if let Ok(Some(transcript)) = snapshot {
            return Ok(transcript);
        }
        let mut transcript = self.read_replay(native_id, cwd).await.map_err(|error| {
            if let Err(snapshot_error) = snapshot {
                return AgentError::new(
                    error.code,
                    format!("{}; local history: {snapshot_error}", error.message),
                );
            }
            error
        })?;
        if self.id == "kiro" {
            kiro_history::enrich_replay(&self.spec, native_id, &mut transcript).await;
        }
        Ok(transcript)
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        let cwd = self.cwd(Some(&req.cwd))?;
        let runtime = self.runtime(Some(&cwd)).await?;
        mapping::prompt(&req.content, &cwd, runtime.capabilities.image)?;
        let (session, reused) = {
            let _lifecycle = runtime.connection.lifecycle.lock().await;
            let previous = runtime.prepared.lock().unwrap().take();
            match previous {
                Some((created, session))
                    if session.cwd == cwd && created.elapsed() < PREPARED_TTL =>
                {
                    session.activate(&Value::Null, sink);
                    (session, true)
                }
                previous => {
                    if let Some((_, session)) = previous {
                        runtime.discard_prepared(session).await;
                    }
                    (
                        self.new_session(&runtime, cwd.clone(), Some(sink)).await?,
                        false,
                    )
                }
            }
        };
        session.capture_initial_options();
        self.remember(&session.id, &cwd);
        let skipped = match apply_options(&session, &req.options, reused, true).await {
            Ok(skipped) => skipped,
            Err(error) => {
                // Prompt has not been sent yet; clean up the empty native session when supported.
                let _lifecycle = runtime.connection.lifecycle.lock().await;
                runtime.discard_prepared(session).await;
                return Err(error);
            }
        };
        if !skipped.is_empty() {
            let summary = skipped
                .into_iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(", ");
            session.emit_notice(
                vetro_proto::NoticeLevel::Info,
                format!("Using agent default for unsupported options: {summary}"),
            );
        }
        if let Err(error) = session.send(req.content).await {
            session.detach().await;
            return Err(error);
        }
        Ok(session)
    }

    async fn attach(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        let runtime = self.runtime(cwd).await?;
        let method = if runtime.capabilities.resume {
            "session/resume"
        } else if runtime.capabilities.load {
            "session/load"
        } else {
            return Err(AgentError::new(
                ErrorCode::NotResumable,
                "ACP agent cannot attach sessions",
            ));
        };
        let cwd = self.stored_cwd(&runtime, native_id, cwd).await?;
        let _lifecycle = runtime.connection.lifecycle.lock().await;
        let session = AcpSession::new(
            Arc::clone(&runtime.connection),
            native_id.to_owned(),
            cwd.clone(),
            &runtime.capabilities,
            None,
            false,
        );
        runtime.connection.register(native_id, &session)?;
        let live = Arc::downgrade(&session);
        let reply = runtime
            .connection
            .call_ordered(method, session_params(native_id, &cwd), move |reply| {
                if let (Ok(value), Some(session)) = (reply, live.upgrade()) {
                    session.activate(value, sink);
                }
            })
            .await;
        runtime.connection.observe_auth(method, &reply);
        if let Err(error) = reply {
            runtime.connection.unregister(native_id);
            return Err(session_error(error, &self.id, native_id));
        }
        self.remember(native_id, &cwd);
        Ok(session)
    }

    async fn fork(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        turn_id: Option<&str>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        if turn_id.is_some() {
            return Err(AgentError::new(
                ErrorCode::BadRequest,
                "This ACP agent only supports forking the whole session",
            ));
        }
        let runtime = self.runtime(cwd).await?;
        if !runtime.capabilities.fork {
            return Err(AgentError::new(
                ErrorCode::NotResumable,
                "ACP agent does not support native session forking",
            ));
        }
        let cwd = self.stored_cwd(&runtime, native_id, cwd).await?;
        let _lifecycle = runtime.connection.lifecycle.lock().await;
        let _capture = runtime.connection.capture_new();
        let result = runtime
            .call("session/fork", session_params(native_id, &cwd))
            .await?;
        let id = string(&result, "sessionId").ok_or_else(|| {
            AgentError::new(ErrorCode::Internal, "ACP session/fork omitted sessionId")
        })?;
        let session = AcpSession::new(
            Arc::clone(&runtime.connection),
            id.clone(),
            cwd.clone(),
            &runtime.capabilities,
            Some(sink),
            false,
        );
        session.initialize(&result);
        runtime.connection.register(&id, &session)?;
        session.capture_initial_options();
        self.remember(&id, &cwd);
        Ok(session)
    }

    async fn shutdown(&self) {
        let mut current = self.runtime.lock().await;
        if let Some(runtime) = current.take() {
            let prepared = runtime.prepared.lock().unwrap().take();
            if let Some((_, session)) = prepared {
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), runtime.discard_prepared(session))
                        .await;
            }
            runtime.connection.stop().await;
        }
    }

    async fn maintain(&self) {
        let runtime = self.runtime.lock().await.clone();
        if let Some(runtime) = runtime {
            let _lifecycle = runtime.connection.lifecycle.lock().await;
            if let Some(session) = runtime.take_expired_prepared() {
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), runtime.discard_prepared(session))
                        .await;
            }
        }
    }
}

fn session_params(id: &str, cwd: &Path) -> Value {
    json!({ "sessionId": id, "cwd": cwd, "mcpServers": [] })
}

async fn apply_options(
    session: &AcpSession,
    desired: &BTreeMap<String, String>,
    reset_to_initial: bool,
    skip_invalid: bool,
) -> Result<Vec<(String, String)>> {
    let mut target = BTreeMap::new();
    if reset_to_initial {
        if let Some(initial) = session.initial_options() {
            for option in initial.options {
                target.insert(option.id, option.current_value);
            }
        }
    }
    for (id, value) in desired {
        target.insert(id.clone(), value.clone());
    }
    if target.is_empty() {
        return Ok(Vec::new());
    }
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    // Apply Model first so dependent options (e.g. thought_level) update before validation,
    // then Mode, then remaining options.
    for pass in 0..3 {
        let current = session.options().options;
        let pending: Vec<(String, String)> = target
            .iter()
            .filter(|(id, _)| !applied.contains(*id))
            .filter_map(|(id, value)| {
                let option = current.iter().find(|option| option.id == *id);
                let bucket = match option.map(|option| option.category) {
                    Some(vetro_proto::ConfigCategory::Model) => 0,
                    Some(vetro_proto::ConfigCategory::Mode) => 1,
                    _ => 2,
                };
                if bucket != pass {
                    return None;
                }
                if option.is_some_and(|option| option.current_value == *value) {
                    return None;
                }
                if let Some(option) = option {
                    if skip_invalid
                        && !option.choices.is_empty()
                        && !option.choices.iter().any(|choice| choice.value == *value)
                    {
                        if desired.contains_key(id) {
                            skipped.push((option.name.clone(), value.clone()));
                        }
                        return None;
                    }
                } else if skip_invalid {
                    if desired.contains_key(id) {
                        skipped.push((id.clone(), value.clone()));
                    }
                    return None;
                }
                Some((id.clone(), value.clone()))
            })
            .collect();
        for (id, value) in pending {
            session.set_option(&id, &value).await?;
            applied.push(id);
        }
    }
    Ok(skipped)
}

fn standard_spec(program: &str) -> SpawnSpec {
    let mut spec = SpawnSpec::new(program);
    spec.args.push("acp".into());
    spec
}

fn resolve(program: &Path) -> Option<PathBuf> {
    let search = |path: PathBuf| {
        if path.is_file() {
            return std::path::absolute(path).ok();
        }
        #[cfg(windows)]
        if path.extension().is_none() {
            let path = path.with_extension("exe");
            if path.is_file() {
                return std::path::absolute(path).ok();
            }
        }
        None
    };
    if program.components().count() > 1 || program.is_absolute() {
        search(program.to_owned())
    } else {
        env::var_os("PATH")
            .and_then(|path| env::split_paths(&path).find_map(|dir| search(dir.join(program))))
    }
}

async fn version(mut spec: SpawnSpec) -> Option<String> {
    spec.args = vec!["--version".into()];
    let (process, mut stdout) = Process::spawn(spec, Arc::new(WireLog::new(0))).ok()?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut version = None;
        while let Some(Ok(line)) = stdout.recv().await {
            if version.is_none() && !line.trim().is_empty() {
                version = Some(line.trim().to_owned());
            }
        }
        process
            .wait()
            .await
            .ok()
            .filter(|status| status.success())?;
        version
    })
    .await
    .ok()
    .flatten();
    if result.is_none() {
        let _ = process.kill().await;
    }
    result
}

async fn devin_auth_status(spec: &SpawnSpec) -> (AuthState, Option<String>) {
    use std::process::Stdio;

    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(["auth", "status"])
        .envs(&spec.env)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    match tokio::time::timeout(Duration::from_secs(5), command.output()).await {
        Ok(Ok(output)) => parse_devin_auth_status(&String::from_utf8_lossy(&output.stdout)),
        _ => (AuthState::Unknown, None),
    }
}

fn parse_devin_auth_status(output: &str) -> (AuthState, Option<String>) {
    let lower = output.to_ascii_lowercase();
    if [
        "not logged in",
        "not currently logged in",
        "not yet logged in",
        "not signed in",
        "not authenticated",
        "unauthenticated",
        "logged out",
        "login required",
        "sign in required",
    ]
    .iter()
    .any(|message| lower.contains(message))
    {
        return (AuthState::Required, None);
    }
    if lower.contains("logged in") {
        let tier = output.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.trim();
            (key.trim().eq_ignore_ascii_case("tier") && !value.is_empty()).then(|| value.to_owned())
        });
        return (AuthState::Ok, tier);
    }
    (AuthState::Unknown, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devin_status_parses_login_and_optional_tier() {
        assert_eq!(
            parse_devin_auth_status("LOGGED IN\n Tier: Devin Max"),
            (AuthState::Ok, Some("Devin Max".into()))
        );
        assert_eq!(parse_devin_auth_status("Logged in"), (AuthState::Ok, None));
        assert_eq!(
            parse_devin_auth_status("Not logged in\nTier: none"),
            (AuthState::Required, None)
        );
        assert_eq!(
            parse_devin_auth_status("Not authenticated"),
            (AuthState::Required, None)
        );
        assert_eq!(
            parse_devin_auth_status("You are not currently logged in"),
            (AuthState::Required, None)
        );
        assert_eq!(
            parse_devin_auth_status("unexpected output"),
            (AuthState::Unknown, None)
        );
    }
}
