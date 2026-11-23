mod binary;
mod catalog;
mod interaction;
#[cfg(test)]
mod live;
mod mapping;
mod session;

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use serde_json::{json, Value};
use session::Live;
use tokio::sync::Mutex as AsyncMutex;
use vetro_agent_api::{
    async_trait,
    jsonrpc::{self, Incoming, Peer, RemoteError, Version},
    process::{Process, SpawnSpec},
    Agent, AgentError, AgentOptions, EventSink, Result, Session, SessionPage, StartRequest,
    Transcript, WireLog,
};
use vetro_proto::{
    rpc::{AuthenticateResult, ErrorCode},
    AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthMethod, AuthState,
};

pub use binary::resolve_binary;

const SOURCE_KINDS: &[&str] = &[
    "cli",
    "vscode",
    "exec",
    "appServer",
    "subAgent",
    "subAgentReview",
    "subAgentCompact",
    "subAgentThreadSpawn",
    "subAgentOther",
    "unknown",
];
const TIMEOUT: Duration = Duration::from_secs(60);

pub struct CodexAgent {
    spec: Option<SpawnSpec>,
    missing_binary_message: &'static str,
    app: AsyncMutex<Option<Arc<App>>>,
}

impl CodexAgent {
    pub fn new(binary: Option<PathBuf>) -> Self {
        Self::from_config(binary.as_deref(), SpawnSpec::new)
    }

    pub fn from_config(binary: Option<&Path>, spec: impl FnOnce(PathBuf) -> SpawnSpec) -> Self {
        let mut agent = Self::from_spec(resolve_binary(binary).map(spec));
        if agent.spec.is_none() {
            agent.missing_binary_message = binary::missing_binary_message(binary);
        }
        agent
    }

    pub fn from_spec(spec: Option<SpawnSpec>) -> Self {
        Self {
            spec,
            missing_binary_message: "Codex executable not found",
            app: AsyncMutex::new(None),
        }
    }

    async fn app(&self) -> Result<Arc<App>> {
        let mut slot = self.app.lock().await;
        if let Some(app) = slot
            .as_ref()
            .filter(|app| !app.state.lock().unwrap().closed)
        {
            return Ok(Arc::clone(app));
        }
        let spec = self
            .spec
            .as_ref()
            .ok_or_else(|| unavailable(self.missing_binary_message))?;
        let app = App::start(spec.clone()).await?;
        *slot = Some(Arc::clone(&app));
        Ok(app)
    }
}

impl Default for CodexAgent {
    fn default() -> Self {
        Self::new(None)
    }
}

#[async_trait]
impl Agent for CodexAgent {
    fn id(&self) -> &str {
        "codex"
    }

    async fn info(&self) -> AgentInfo {
        let mut info = AgentInfo {
            id: self.id().into(),
            name: "Codex".into(),
            kind: AgentKind::Codex,
            installed: self
                .spec
                .as_ref()
                .is_some_and(|spec| spec.program.is_file()),
            path: self
                .spec
                .as_ref()
                .map(|spec| spec.program.to_string_lossy().into_owned()),
            version: None,
            auth: AgentAuth {
                state: AuthState::Unknown,
                methods: Vec::new(),
                account: None,
            },
            capabilities: AgentCapabilities {
                image_input: true,
                mid_turn_input: true,
                resume: true,
                list_sessions: true,
            },
            error: None,
        };
        match self.app().await {
            Ok(app) => {
                info.version.clone_from(&app.version);
                match app
                    .call("account/read", json!({ "refreshToken": false }))
                    .await
                {
                    Ok(response) => {
                        let account = &response["account"];
                        info.auth.state =
                            if account.is_object() || response["requiresOpenaiAuth"] == false {
                                AuthState::Ok
                            } else if response["requiresOpenaiAuth"] == true {
                                AuthState::Required
                            } else {
                                AuthState::Unknown
                            };
                        info.auth.account = mapping::string(account, "workspaceName")
                            .or_else(|| mapping::string(account, "planType"))
                            .or_else(|| mapping::string(account, "type"));
                        info.auth.methods = auth_methods(&app.requirements().await);
                    }
                    Err(error) => info.error = Some(error.message),
                }
            }
            Err(error) => info.error = Some(error.message),
        }
        info
    }

    async fn authenticate(&self, method_id: &str) -> Result<AuthenticateResult> {
        let app = self.app().await?;
        if !auth_methods(&app.requirements().await)
            .iter()
            .any(|method| method.id == method_id)
        {
            return Err(bad_request("Unsupported Codex sign-in method"));
        }
        let response = app
            .call("account/login/start", json!({ "type": method_id }))
            .await?;
        if method_id == "chatgptDeviceCode" {
            let url = mapping::string(&response, "verificationUrl")
                .ok_or_else(|| unavailable("Codex did not return a verification URL"))?;
            let code = mapping::string(&response, "userCode")
                .ok_or_else(|| unavailable("Codex did not return a sign-in code"))?;
            return Ok(AuthenticateResult {
                message: Some("Enter the code in your browser to sign in with ChatGPT".into()),
                url: Some(url),
                code: Some(code),
            });
        }
        let url = response["authUrl"]
            .as_str()
            .ok_or_else(|| unavailable("Codex did not return a browser sign-in URL"))?;
        if let Err(error) = catalog::open_browser(url) {
            if let Some(login_id) = response["loginId"].as_str() {
                let _ = app
                    .call("account/login/cancel", json!({ "loginId": login_id }))
                    .await;
            }
            return Err(error);
        }
        Ok(AuthenticateResult {
            message: Some("Finish signing in with ChatGPT in your browser".into()),
            ..Default::default()
        })
    }

    async fn options(
        &self,
        cwd: Option<&Path>,
        selected: &BTreeMap<String, String>,
    ) -> Result<AgentOptions> {
        let app = self.app().await?;
        Ok(AgentOptions {
            options: catalog::Catalog::load(&app, cwd).await?.options(selected),
            commands: Vec::new(),
        })
    }

    async fn list(&self, cursor: Option<String>) -> Result<SessionPage> {
        let mut params = json!({
            "limit": 50,
            "sourceKinds": SOURCE_KINDS,
            "modelProviders": [],
            "useStateDbOnly": true,
        });
        if let Some(cursor) = cursor {
            params["cursor"] = json!(cursor);
        }
        let result = self.app().await?.call("thread/list", params).await?;
        Ok(SessionPage {
            sessions: mapping::array(&result["data"])
                .iter()
                .filter_map(mapping::record)
                .collect(),
            next_cursor: mapping::string(&result, "nextCursor"),
        })
    }

    async fn read(&self, native_id: &str, _cwd: Option<&Path>) -> Result<Transcript> {
        let response = self
            .app()
            .await?
            .call(
                "thread/read",
                json!({ "threadId": native_id, "includeTurns": true }),
            )
            .await?;
        Ok(mapping::transcript(&response["thread"]))
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        let input = mapping::input(req.content)?;
        let app = self.app().await?;
        let catalog = catalog::Catalog::load(&app, Some(&req.cwd)).await?;
        let (params, turn_options) = catalog.start_params(&req.cwd, &req.options)?;
        let response = app.call("thread/start", params).await?;
        let id = mapping::string(&response["thread"], "id")
            .ok_or_else(|| unavailable("Codex returned a thread without an id"))?;
        let live = Live::register(app, id, sink, catalog)?;
        live.hydrate(&response, 0, &req.options);
        if let Err(error) = live
            .start_turn(
                input,
                turn_options,
                req.options.get("mode").map(String::as_str),
            )
            .await
        {
            live.detach().await;
            return Err(error);
        }
        Ok(live)
    }

    async fn attach(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        let app = self.app().await?;
        let catalog = catalog::Catalog::load(&app, cwd).await?;
        let live = Live::register(Arc::clone(&app), native_id.to_owned(), sink, catalog)?;
        match app
            .call("thread/resume", json!({ "threadId": native_id }))
            .await
        {
            Ok(response) => {
                live.hydrate(&response, 0, &BTreeMap::new());
                Ok(live)
            }
            Err(error) => {
                live.detach().await;
                Err(error)
            }
        }
    }

    async fn shutdown(&self) {
        let mut slot = self.app.lock().await;
        if let Some(app) = slot.take() {
            app.close(None);
            let _ = app.peer.stop().await;
        }
    }
}

fn auth_methods(requirements: &Value) -> Vec<AuthMethod> {
    if requirements["allowedLoginMethods"]
        .as_array()
        .is_some_and(|methods| !methods.iter().any(|method| method == "chatgpt"))
    {
        return Vec::new();
    }
    vec![
        AuthMethod {
            id: "chatgpt".into(),
            name: "Sign in with ChatGPT".into(),
            description: Some("Continue in your browser".into()),
        },
        AuthMethod {
            id: "chatgptDeviceCode".into(),
            name: "Sign in with a device code".into(),
            description: Some("Continue in a browser on any device".into()),
        },
    ]
}

#[derive(Default)]
struct AppState {
    closed: bool,
    threads: HashMap<String, Weak<Live>>,
    limits: BTreeMap<String, Value>,
    limits_revision: u64,
}

struct App {
    peer: Peer,
    version: Option<String>,
    state: Mutex<AppState>,
}

impl App {
    async fn start(mut spawn: SpawnSpec) -> Result<Arc<Self>> {
        spawn.args.insert(0, "app-server".into());
        let log = WireLog::default().with_filter(|text| {
            if !text.contains("\"userCode\"") && !text.contains("\"authUrl\"") {
                return true;
            }
            let login_response = serde_json::from_str::<Value>(text)
                .ok()
                .is_some_and(|message| {
                    let result = &message["result"];
                    result.get("authUrl").is_some() || result.get("userCode").is_some()
                });
            !login_response
        });
        let (process, stdout) = Process::spawn(spawn, Arc::new(log))
            .map_err(|error| unavailable(format!("Could not start Codex: {error}")))?;
        let (peer, incoming) = Peer::new(process, stdout, Version::Bare);
        let result = call(&peer, "initialize", json!({
            "clientInfo": {"name": "vetro", "title": "Vetro", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true},
        })).await?;
        peer.notify("initialized", None)
            .await
            .map_err(transport_error)?;
        let version = result["userAgent"]
            .as_str()
            .and_then(|agent| agent.split_whitespace().next()?.split_once('/'))
            .map(|(_, version)| version.to_owned());
        let app = Arc::new(Self {
            peer,
            version,
            state: Mutex::default(),
        });
        tokio::spawn(Self::dispatch(Arc::downgrade(&app), incoming));
        Ok(app)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        call(&self.peer, method, params).await
    }

    async fn requirements(&self) -> Value {
        self.call("configRequirements/read", json!({}))
            .await
            .map(|mut response| response["requirements"].take())
            .unwrap_or_default()
    }

    async fn refresh_limits(&self) {
        let revision = self.state.lock().unwrap().limits_revision;
        if let Ok(response) = self.call("account/rateLimits/read", json!({})).await {
            let mut state = self.state.lock().unwrap();
            if state.closed || state.limits_revision != revision {
                return;
            }
            if let Some(buckets) = response["rateLimitsByLimitId"]
                .as_object()
                .filter(|buckets| !buckets.is_empty())
            {
                state.limits = buckets
                    .iter()
                    .map(|(id, value)| (id.clone(), value.clone()))
                    .collect();
            } else if response["rateLimits"].is_object() {
                state.limits = [(
                    mapping::limit_id(&response["rateLimits"]),
                    response["rateLimits"].clone(),
                )]
                .into();
            }
            Self::publish_limits(&state);
        }
    }

    fn publish_limits(state: &AppState) {
        let limits = mapping::rate_limits(&state.limits);
        for live in state.threads.values().filter_map(Weak::upgrade) {
            live.limits(&limits);
        }
    }

    fn close(&self, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        for live in state.threads.drain().filter_map(|(_, live)| live.upgrade()) {
            live.close(error.clone());
        }
    }

    async fn dispatch(app: Weak<Self>, mut incoming: jsonrpc::IncomingMessages) {
        while let Some(message) = incoming.recv().await {
            let Some(app) = app.upgrade() else { return };
            match message {
                Incoming::Notification { method, params } => {
                    let params = params.unwrap_or_default();
                    let mut state = app.state.lock().unwrap();
                    if method == "account/rateLimits/updated" {
                        state.limits_revision += 1;
                        let new = &params["rateLimits"];
                        let bucket = state
                            .limits
                            .entry(mapping::limit_id(new))
                            .or_insert_with(|| json!({}));
                        if let Some(fields) = new.as_object() {
                            for (key, value) in fields.iter().filter(|(_, value)| !value.is_null())
                            {
                                bucket[key] = value.clone();
                            }
                        }
                        Self::publish_limits(&state);
                    } else if method == "account/updated" {
                        state.limits_revision += 1;
                        state.limits.clear();
                        Self::publish_limits(&state);
                        let app = Arc::clone(&app);
                        tokio::spawn(async move { app.refresh_limits().await });
                    } else if let Some(id) = params["threadId"].as_str() {
                        if let Some(live) = state.threads.get(id).and_then(Weak::upgrade) {
                            live.notification(&method, &params);
                        }
                    } else if mapping::global_notice(&method) {
                        for live in state.threads.values().filter_map(Weak::upgrade) {
                            live.notification(&method, &params);
                        }
                    }
                }
                Incoming::Request { id, method, params } => {
                    let params = params.unwrap_or_default();
                    let live = params["threadId"].as_str().and_then(|thread_id| {
                        app.state
                            .lock()
                            .unwrap()
                            .threads
                            .get(thread_id)
                            .and_then(Weak::upgrade)
                    });
                    let supported =
                        live.is_some_and(|live| live.request(id.clone(), &method, &params));
                    if !supported {
                        let _ = app
                            .peer
                            .respond(
                                id,
                                Err(RemoteError {
                                    code: -32601,
                                    message:
                                        "Unsupported Codex server request or unattached thread"
                                            .into(),
                                    data: None,
                                }),
                            )
                            .await;
                    }
                }
            }
        }
        if let Some(app) = app.upgrade() {
            let status = tokio::time::timeout(Duration::from_millis(300), app.peer.wait()).await;
            app.close(Some(match status {
                Ok(Ok(status)) => format!("Codex app-server exited ({status})"),
                _ => "Codex app-server connection closed".into(),
            }));
            let _ = app.peer.kill().await;
        }
    }
}

async fn call(peer: &Peer, method: &str, params: Value) -> Result<Value> {
    tokio::time::timeout(TIMEOUT, peer.request(method, Some(params)))
        .await
        .map_err(|_| unavailable(format!("Codex {method} timed out")))?
        .map_err(transport_error)
}

fn transport_error(error: jsonrpc::Error) -> AgentError {
    match &error {
        jsonrpc::Error::Remote(remote) => {
            let code = match remote.code {
                401 => ErrorCode::AuthRequired,
                -32602 => ErrorCode::BadRequest,
                _ => ErrorCode::Internal,
            };
            AgentError::new(code, error.to_string())
        }
        _ => unavailable(error.to_string()),
    }
}

fn unavailable(message: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::AgentUnavailable, message)
}

fn bad_request(message: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::BadRequest, message)
}
