use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use vetro_agent_api::{
    async_trait, process::Process, Agent, AgentError, AgentOptions, EventSink, Result, Session,
    SessionPage, StartRequest, Transcript, WireLog,
};
use vetro_proto::{
    rpc::ErrorCode, AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthMethod, AuthState,
};

pub use vetro_agent_api::process::SpawnSpec;

mod interaction;
mod mapping;
mod session;
mod wire;

use mapping::{array, string};
use session::AcpSession;
use wire::{agent_error, Connection};

pub struct AcpAgent {
    id: String,
    name: String,
    spec: SpawnSpec,
    steering: bool,
    version: OnceCell<Option<String>>,
    runtime: AsyncMutex<Option<Arc<Runtime>>>,
    directories: Mutex<HashMap<String, PathBuf>>,
}

struct Runtime {
    connection: Arc<Connection>,
    capabilities: Capabilities,
    methods: Vec<AuthMethod>,
}

#[derive(Default)]
pub(crate) struct Capabilities {
    image: bool,
    steering: bool,
    load: bool,
    resume: bool,
    list: bool,
    close: bool,
    delete: bool,
}

impl AcpAgent {
    pub fn new(id: impl Into<String>, name: impl Into<String>, spec: SpawnSpec) -> Self {
        let id = id.into();
        Self {
            steering: id == "antigravity",
            id,
            name: name.into(),
            spec,
            version: OnceCell::new(),
            runtime: AsyncMutex::new(None),
            directories: Mutex::new(HashMap::new()),
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
        let connection = Connection::spawn(spec)?;
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
            .await
            .map_err(agent_error)?;
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
        if self.id == "devin"
            && runtime
                .methods
                .iter()
                .any(|method| method.id == "devin-browser")
        {
            *runtime.connection.auth.lock().unwrap() = AuthState::Required;
        }
        *current = Some(Arc::clone(&runtime));
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
            return Ok(SessionPage::default());
        }
        let params = cursor.map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
        let result = runtime.call("session/list", params).await?;
        let sessions = array(&result["sessions"])
            .filter_map(|session| {
                mapping::record(
                    session,
                    runtime.capabilities.load || runtime.capabilities.resume,
                )
            })
            .collect::<Vec<_>>();
        for record in &sessions {
            if let Some(cwd) = &record.cwd {
                self.remember(&record.native_id, cwd);
            }
        }
        Ok(SessionPage {
            sessions,
            next_cursor: string(&result, "nextCursor").filter(|cursor| !cursor.is_empty()),
        })
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
        let version = match spec {
            Ok(spec) => self.version.get_or_init(|| version(spec)).await.clone(),
            Err(_) => None,
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
                state: AuthState::Unknown,
                methods: Vec::new(),
                account: None,
            },
            capabilities: AgentCapabilities::default(),
            error: None,
        };
        match runtime {
            Ok(runtime) => {
                let caps = &runtime.capabilities;
                info.capabilities = AgentCapabilities {
                    image_input: caps.image,
                    mid_turn_input: caps.steering,
                    resume: caps.load || caps.resume,
                    list_sessions: caps.list,
                };
                info.auth.methods.clone_from(&runtime.methods);
                info.auth.state = *runtime.connection.auth.lock().unwrap();
            }
            Err(error) => info.error = Some(error.message),
        }
        info
    }

    async fn authenticate(&self, method_id: &str) -> Result<()> {
        self.runtime(None)
            .await?
            .call("authenticate", json!({ "methodId": method_id }))
            .await?;
        Ok(())
    }

    async fn options(&self, cwd: Option<&Path>) -> Result<AgentOptions> {
        let cwd = self.cwd(cwd)?;
        let runtime = self.runtime(Some(&cwd)).await?;
        let _lifecycle = runtime.connection.lifecycle.lock().await;
        let mut cache = runtime.connection.options.lock().await;
        if let Some(cached) = &*cache {
            return Ok(cached.clone());
        }
        let session = self.new_session(&runtime, cwd, None).await?;
        let mut options = session.options();
        runtime.close_session(&session.id).await;
        options.commands = session.options().commands;
        if runtime.capabilities.delete {
            let _ = runtime
                .call("session/delete", json!({ "sessionId": session.id }))
                .await;
        }
        *cache = Some(options.clone());
        Ok(options)
    }

    async fn list(&self, cursor: Option<String>) -> Result<SessionPage> {
        let runtime = self.runtime(None).await?;
        self.list_on(&runtime, cursor).await
    }

    async fn read(&self, native_id: &str, cwd: Option<&Path>) -> Result<Transcript> {
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
        if reply.is_ok() {
            runtime.close_session(native_id).await;
        } else {
            runtime.connection.unregister(native_id);
        }
        reply.map_err(agent_error)?;
        let result = transcript.lock().unwrap().take().unwrap_or_default();
        Ok(result)
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        let cwd = self.cwd(Some(&req.cwd))?;
        let runtime = self.runtime(Some(&cwd)).await?;
        mapping::prompt(&req.content, &cwd, runtime.capabilities.image)?;
        let session = {
            let _lifecycle = runtime.connection.lifecycle.lock().await;
            self.new_session(&runtime, cwd.clone(), Some(sink)).await?
        };
        self.remember(&session.id, &cwd);
        let result = async {
            for (id, value) in req.options {
                session.set_option(&id, &value).await?;
            }
            session.send(req.content).await
        }
        .await;
        if let Err(error) = result {
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
            return Err(agent_error(error));
        }
        self.remember(native_id, &cwd);
        Ok(session)
    }

    async fn shutdown(&self) {
        let mut current = self.runtime.lock().await;
        if let Some(runtime) = current.take() {
            runtime.connection.stop().await;
        }
    }
}

fn session_params(id: &str, cwd: &Path) -> Value {
    json!({ "sessionId": id, "cwd": cwd, "mcpServers": [] })
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
