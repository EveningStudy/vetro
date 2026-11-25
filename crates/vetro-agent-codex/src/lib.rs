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
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use session::Live;
use tokio::sync::Mutex as AsyncMutex;
use vetro_agent_api::{
    async_trait, gateway_origin,
    jsonrpc::{self, Incoming, Peer, RemoteError, Version},
    process::{Process, SpawnSpec},
    Agent, AgentError, AgentOptions, EventSink, Result, Session, SessionPage, StartRequest,
    Transcript, WireLog,
};
use vetro_proto::{
    rpc::{AuthenticateResult, ErrorCode},
    AgentAuth, AgentCapabilities, AgentInfo, AgentKind, AuthMethod, AuthState,
    HistoryEditCapabilities, RateLimit,
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
                .map(|spec| mapping::display_path(&spec.program.to_string_lossy())),
            version: None,
            auth: AgentAuth {
                state: AuthState::Unknown,
                methods: Vec::new(),
                account: None,
                provider: None,
                endpoint: None,
            },
            capabilities: AgentCapabilities {
                image_input: true,
                mid_turn_input: true,
                resume: true,
                list_sessions: true,
                fork: vetro_proto::ForkCapabilities {
                    whole: true,
                    at_turn: true,
                },
                history_edit: self.history_edit_capabilities(),
            },
            error: None,
            rate_limits: Vec::new(),
            rate_limits_updated_at: None,
        };
        match self.app().await {
            Ok(app) => {
                info.version = app.version.get().cloned();
                let (account, config, limits) = tokio::join!(
                    app.call("account/read", json!({ "refreshToken": false })),
                    app.call("config/read", json!({})),
                    app.refresh_limits(),
                );
                if let Some((limits, observed_at)) = limits {
                    info.rate_limits = limits;
                    info.rate_limits_updated_at = Some(observed_at);
                }
                let config = config.ok();
                info.auth = account_info(account.as_ref().ok(), config.as_ref());
                match account {
                    Ok(_) => info.auth.methods = auth_methods(&app.requirements().await),
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
            incomplete: false,
            sessions: mapping::array(&result["data"])
                .iter()
                .filter_map(mapping::record)
                .collect(),
            next_cursor: mapping::string(&result, "nextCursor"),
        })
    }

    async fn read(&self, native_id: &str, _cwd: Option<&Path>) -> Result<Transcript> {
        let app = self.app().await?;
        match app.transcript(native_id).await {
            Err(error) if error.code == ErrorCode::NotResumable => {
                let response = app
                    .call(
                        "thread/read",
                        json!({ "threadId": native_id, "includeTurns": true }),
                    )
                    .await?;
                Ok(mapping::transcript(&response["thread"]))
            }
            result => result,
        }
    }

    async fn start(&self, req: StartRequest, sink: EventSink) -> Result<Arc<dyn Session>> {
        let content = req.content;
        let input = mapping::input(content.clone())?;
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
                content,
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
        match app.resume(native_id).await {
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

    async fn fork(
        &self,
        native_id: &str,
        cwd: Option<&Path>,
        turn_id: Option<&str>,
        sink: EventSink,
    ) -> Result<Arc<dyn Session>> {
        let app = self.app().await?;
        let catalog = catalog::Catalog::load(&app, cwd).await?;
        let mut params = json!({
            "threadId": native_id,
            "excludeTurns": true,
        });
        if let Some(turn_id) = turn_id {
            params["lastTurnId"] = json!(turn_id);
        }
        if let Some(cwd) = cwd {
            params["cwd"] = json!(cwd.to_string_lossy());
        }
        let response = app.call("thread/fork", params).await?;
        let id = mapping::string(&response["thread"], "id")
            .ok_or_else(|| unavailable("Codex returned a forked thread without an id"))?;
        let live = Live::register(app, id, sink, catalog)?;
        live.hydrate(&response, 0, &BTreeMap::new());
        Ok(live)
    }

    fn history_edit_capabilities(&self) -> HistoryEditCapabilities {
        HistoryEditCapabilities {
            rollback_conversation: true,
            ..Default::default()
        }
    }

    async fn rollback_conversation(
        &self,
        native_id: &str,
        _cwd: Option<&Path>,
        before_turn_id: &str,
    ) -> Result<()> {
        if before_turn_id.trim().is_empty() {
            return Err(bad_request("Codex rollback requires a turn ID"));
        }
        let app = self.app().await?;
        if app.live(native_id).is_some() {
            return Err(AgentError::new(
                ErrorCode::Busy,
                "Detach the Codex thread before rolling it back",
            ));
        }
        app.resume(native_id).await?;
        let result = app
            .call(
                "thread/revert",
                json!({"threadId": native_id, "beforeTurnId": before_turn_id}),
            )
            .await;
        let _ = app
            .call("thread/unsubscribe", json!({"threadId": native_id}))
            .await;
        result.map(|_| ()).map_err(|error| {
            AgentError::new(
                error.code,
                format!("Codex thread/revert: {}", error.message),
            )
        })
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
    version: OnceLock<String>,
    state: Mutex<AppState>,
}

fn account_info(response: Option<&Value>, config_reply: Option<&Value>) -> AgentAuth {
    let response = response.unwrap_or(&Value::Null);
    let account = &response["account"];
    let config = &config_reply.unwrap_or(&Value::Null)["config"];
    let provider_id = config["model_provider"]
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.eq_ignore_ascii_case("openai"));
    let provider_config = provider_id
        .and_then(|id| config["model_providers"].get(id))
        .unwrap_or(&Value::Null);
    let official: &[&str] = if provider_id.is_some() {
        &[]
    } else {
        &["api.openai.com", "chatgpt.com"]
    };
    let endpoint = [
        &provider_config["base_url"],
        &config["openai_base_url"],
        &config["base_url"],
    ]
    .into_iter()
    .filter_map(Value::as_str)
    .find_map(|url| gateway_origin(url, official));
    let state = if account.is_object() || response["requiresOpenaiAuth"] == false {
        AuthState::Ok
    } else if response["requiresOpenaiAuth"] == true {
        AuthState::Required
    } else if provider_id.is_some()
        && provider_config["requiresOpenaiAuth"] != true
        && provider_config["requires_openai_auth"] != true
    {
        AuthState::Ok
    } else {
        AuthState::Unknown
    };
    let label = mapping::string(account, "workspaceName").or_else(|| {
        let plan = account["planType"].as_str().or(account["type"].as_str())?;
        Some(
            match plan.to_ascii_lowercase().as_str() {
                "plus" => "ChatGPT Plus",
                "pro" => "ChatGPT Pro",
                "team" => "ChatGPT Team",
                "enterprise" => "ChatGPT Enterprise",
                "free" => "ChatGPT Free",
                "apikey" | "api_key" => "API Key",
                _ => plan,
            }
            .to_owned(),
        )
    });
    AgentAuth {
        state,
        methods: Vec::new(),
        account: label,
        provider: provider_id
            .map(str::to_owned)
            .or_else(|| endpoint.as_ref().map(|_| "custom".into())),
        endpoint,
    }
}

#[test]
fn custom_provider_and_account_label() {
    let config = json!({"config": {
        "model_provider": "magpie",
        "model_providers": {"magpie": {
            "base_url": "http://user:secret@127.0.0.1:3425/v1?key=secret#token"
        }}
    }});
    let plus = json!({"account": {"planType": "plus"}});
    let auth = account_info(Some(&plus), Some(&config));
    assert_eq!(auth.provider.as_deref(), Some("magpie"));
    assert_eq!(auth.endpoint.as_deref(), Some("http://127.0.0.1:3425"));
    assert_eq!(auth.account.as_deref(), Some("ChatGPT Plus"));
    assert_eq!(account_info(None, Some(&config)).state, AuthState::Ok);
    let required = json!({"requiresOpenaiAuth": true});
    assert_eq!(
        account_info(Some(&required), Some(&config)).state,
        AuthState::Required
    );
}

#[test]
fn gateway_override_and_legacy_account() {
    let config = json!({"config": {
        "model_provider": "OpenAI",
        "openai_base_url": "https://gateway.example/v1"
    }});
    let auth = account_info(None, Some(&config));
    assert_eq!(auth.provider.as_deref(), Some("custom"));
    assert_eq!(auth.endpoint.as_deref(), Some("https://gateway.example"));
    let official = json!({"config": {"openai_base_url": "https://api.openai.com/v1"}});
    assert_eq!(account_info(None, Some(&official)).provider, None);
    let key = json!({"account": {"type": "apiKey"}});
    assert_eq!(
        account_info(Some(&key), None).account.as_deref(),
        Some("API Key")
    );
}

#[test]
fn native_subagent_source_survives_an_interactive_originator() {
    let record = mapping::record(&json!({
        "id": "child", "originator": "codex_cli_rs", "source": "subAgentCompact"
    }))
    .unwrap();
    assert_eq!(record.origin.as_deref(), Some("codex_cli_rs"));
    assert!(record.internal);
}
impl App {
    async fn start(mut spawn: SpawnSpec) -> Result<Arc<Self>> {
        spawn.args.insert(0, "app-server".into());
        let log = WireLog::default().multiplexed().with_filter(|text| {
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
        let (peer, incoming) = Peer::new_ordered(process, stdout, Version::Bare);
        let app = Arc::new(Self {
            peer,
            version: OnceLock::new(),
            state: Mutex::default(),
        });
        tokio::spawn(Self::dispatch(Arc::downgrade(&app), incoming));
        let result = app.call("initialize", json!({
            "clientInfo": {"name": "vetro", "title": "Vetro", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true},
        })).await;
        let result = match result {
            Ok(result) => result,
            Err(mut error) => {
                error.message.push_str(&app.peer.exit_detail().await);
                return Err(error);
            }
        };
        if let Err(error) = app.peer.notify("initialized", None).await {
            let mut error = transport_error(error);
            error.message.push_str(&app.peer.exit_detail().await);
            return Err(error);
        }
        let version = result["userAgent"]
            .as_str()
            .and_then(|agent| agent.split_whitespace().next()?.split_once('/'))
            .map(|(_, version)| version.to_owned());
        if let Some(version) = version {
            let _ = app.version.set(version);
        }
        Ok(app)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        call(&self.peer, method, params).await
    }

    async fn resume(&self, thread_id: &str) -> Result<Value> {
        self.call(
            "thread/resume",
            json!({
                "threadId": thread_id,
                "excludeTurns": true,
                "initialTurnsPage": {"limit": 1, "itemsView": "notLoaded"}
            }),
        )
        .await
    }

    async fn call_ordered(
        &self,
        method: &str,
        params: Value,
        receive: impl FnOnce(&std::result::Result<Value, jsonrpc::Error>) + Send + 'static,
    ) -> Result<Value> {
        tokio::time::timeout(
            TIMEOUT,
            self.peer.request_with(method, Some(params), receive),
        )
        .await
        .map_err(|_| unavailable(format!("Codex {method} timed out")))?
        .map_err(transport_error)
    }

    fn live(&self, id: &str) -> Option<Arc<Live>> {
        self.state
            .lock()
            .unwrap()
            .threads
            .get(id)
            .and_then(Weak::upgrade)
    }

    async fn transcript(&self, thread_id: &str) -> Result<Transcript> {
        let turns = catalog::pages(self, "thread/turns/list", json!({
            "threadId": thread_id, "itemsView": "notLoaded", "sortDirection": "asc", "limit": 64,
        })).await?;
        let mut transcript = Transcript::default();
        for turn in turns {
            let Some(turn_id) = turn["id"].as_str() else {
                continue;
            };
            let entries = catalog::pages(
                self,
                "thread/items/list",
                json!({
                    "threadId": thread_id, "turnId": turn_id, "sortDirection": "asc", "limit": 100,
                }),
            )
            .await?;
            for entry in entries {
                if let Some(mut item) = mapping::item(&entry["item"], Some(turn_id), false) {
                    item.ts = entry["startedAtMs"]
                        .as_i64()
                        .or_else(|| entry["completedAtMs"].as_i64())
                        .or(item.ts);
                    transcript.items.push(item);
                }
            }
            if let Some(end) = mapping::turn_end(&turn, None) {
                transcript.items.push(end);
            }
        }
        Ok(transcript)
    }

    async fn requirements(&self) -> Value {
        self.call("configRequirements/read", json!({}))
            .await
            .map(|mut response| response["requirements"].take())
            .unwrap_or_default()
    }

    async fn refresh_limits(&self) -> Option<(Vec<RateLimit>, i64)> {
        let revision = self.state.lock().unwrap().limits_revision;
        let mut response = self.call("account/rateLimits/read", json!({})).await.ok()?;
        let response = response.as_object_mut()?;
        let buckets = match response.remove("rateLimitsByLimitId") {
            Some(Value::Object(buckets)) if !buckets.is_empty() => buckets.into_iter().collect(),
            _ => {
                let bucket = response.remove("rateLimits")?;
                [(mapping::limit_id(&bucket), bucket)].into()
            }
        };
        let limits = mapping::rate_limits(&buckets);
        let mut state = self.state.lock().unwrap();
        if limits.is_empty() || state.closed || state.limits_revision != revision {
            return None;
        }
        let observed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        state.limits = buckets;
        Self::publish_limits(&state, &limits);
        Some((limits, observed_at))
    }

    fn publish_limits(state: &AppState, limits: &[RateLimit]) {
        for live in state.threads.values().filter_map(Weak::upgrade) {
            live.limits(limits);
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
                Incoming::Response { id, envelope } => app.peer.complete(id, envelope),
                Incoming::Notification { method, .. } if method == "thread/reverted" => {}
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
                        Self::publish_limits(&state, &mapping::rate_limits(&state.limits));
                    } else if method == "account/updated" {
                        state.limits_revision += 1;
                        state.limits.clear();
                        Self::publish_limits(&state, &[]);
                        let app = Arc::clone(&app);
                        tokio::spawn(async move {
                            let _ = app.refresh_limits().await;
                        });
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
            app.peer.finish_incoming();
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
                -32601 => ErrorCode::NotResumable,
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
