use std::{
    collections::HashMap,
    future::Future,
    io,
    net::{Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Request, State,
    },
    http::{header, uri::Authority, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch},
};
use vetro_agent_api::Result;
use vetro_proto::rpc::{ErrorCode, HelloParams, RpcRequest, RpcResponse};

use crate::{internal, Core};

mod auth;
mod rpc;
mod tailscale;

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

const PLACEHOLDER: &str = "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\">\
    <title>Vetro</title><h1>Vetro</h1><p>The web UI has not been built yet.</p></html>";
const DEFAULT_PORT: u16 = 47600;

pub struct Server {
    listener: TcpListener,
    state: Arc<HttpState>,
    stop: watch::Sender<bool>,
}

struct HttpState {
    core: Arc<Core>,
    auth: auth::Auth,
    address: SocketAddr,
    web_dir: Option<PathBuf>,
    stopping: watch::Receiver<bool>,
    starts: rpc::StartReceipts,
}

#[derive(Clone)]
enum RequestHost {
    Local,
    Remote(Authority),
}

impl Server {
    pub async fn bind(core: Arc<Core>, port: Option<u16>) -> Result<Self> {
        let configured = port.or_else(|| {
            let port = core.home.settings().port;
            (port != 0).then_some(port)
        });
        let listener = bind_loopback(configured).await.map_err(internal)?;
        let address = listener.local_addr().map_err(internal)?;
        let (stop, stopping) = watch::channel(false);
        let state = Arc::new(HttpState {
            core,
            auth: auth::Auth::new(),
            address,
            web_dir: std::env::var_os("VETRO_WEB_DIR").map(PathBuf::from),
            stopping,
            starts: rpc::StartReceipts::default(),
        });
        Ok(Self {
            listener,
            state,
            stop,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.state.address
    }

    pub fn local_url(&self) -> String {
        format!(
            "http://{}/#token={}",
            self.address(),
            self.state.core.home.local_token()
        )
    }

    pub fn enable_remote_proxy(&mut self, origin: &str) -> Result<()> {
        self.state.auth.enable_remote_proxy(origin)
    }

    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> io::Result<()> {
        let core = self.state.core.clone();
        let state = self.state.clone();
        let app = Router::new()
            .route("/api/health", get(health))
            .route("/api/auth/local", post(auth::local))
            .route("/api/auth/pair", post(auth::pair))
            .route("/api/auth/session", get(auth::session))
            .route("/api/ws", get(upgrade))
            .fallback(static_ui)
            .layer(middleware::from_fn_with_state(state.clone(), check_origin))
            .with_state(self.state);
        let history = tokio::spawn(core.clone().history_worker());
        let sessions = tokio::spawn(core.clone().session_worker());
        let serve_watch = tokio::spawn({
            let state = state.clone();
            async move { state.auth.watch_serve(&state).await }
        });
        let result = axum::serve(
            self.listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown.await;
            self.stop.send_replace(true);
        })
        .await;
        serve_watch.abort();
        let _ = serve_watch.await;
        state.auth.stop().await;
        history.abort();
        sessions.abort();
        let _ = history.await;
        let _ = sessions.await;
        if tokio::time::timeout(Duration::from_secs(5), core.shutdown())
            .await
            .is_err()
        {
            core.home.log("Timed out shutting down agents");
        }
        result
    }
}

async fn bind_loopback(port: Option<u16>) -> io::Result<TcpListener> {
    if let Some(port) = port {
        return TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await;
    }
    for port in DEFAULT_PORT..=u16::MAX {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue
            }
            result => return result,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        "No free Vetro port",
    ))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

async fn check_origin(
    State(state): State<Arc<HttpState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let Some(host) = authority(headers) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let kind = if matches!(
        host.host().to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    ) {
        RequestHost::Local
    } else if state.auth.allows(&host) {
        RequestHost::Remote(host.clone())
    } else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if let Some(origin) = headers.get(header::ORIGIN) {
        let same_origin = origin
            .to_str()
            .ok()
            .and_then(|value| value.parse::<Uri>().ok())
            .is_some_and(|origin| {
                matches!(origin.scheme_str(), Some("http" | "https"))
                    && (matches!(kind, RequestHost::Local) || origin.scheme_str() == Some("https"))
                    && origin
                        .path_and_query()
                        .is_none_or(|path| path.as_str() == "/")
                    && origin
                        .authority()
                        .is_some_and(|origin| origin.as_str().eq_ignore_ascii_case(host.as_str()))
            });
        if !same_origin || headers.get_all(header::ORIGIN).iter().count() != 1 {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    request.extensions_mut().insert(kind);
    next.run(request).await
}

fn authority(headers: &HeaderMap) -> Option<Authority> {
    if headers.get_all(header::HOST).iter().count() != 1 {
        return None;
    }
    let host: Authority = headers.get(header::HOST)?.to_str().ok()?.parse().ok()?;
    (!host.as_str().contains('@')).then_some(host)
}

async fn upgrade(
    State(state): State<Arc<HttpState>>,
    axum::Extension(host): axum::Extension<RequestHost>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .max_message_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| websocket(socket, state, host))
}

async fn handshake(
    socket: &mut WebSocket,
    state: &HttpState,
    host: RequestHost,
    connection: &mut rpc::Connection,
) -> Option<auth::Client> {
    loop {
        let request = match socket.recv().await?.ok()? {
            Message::Text(text) => serde_json::from_str::<RpcRequest>(&text).ok()?,
            Message::Close(_) => return None,
            _ => continue,
        };
        let client = (request.method == "app.hello")
            .then(|| serde_json::from_value::<HelloParams>(request.params.clone()).ok())
            .flatten()
            .and_then(|params| state.auth.client(state, params.session.as_deref()?, &host));
        let response = if let Some(client) = &client {
            let response = rpc::dispatch(state, client, connection, request).await;
            if connection.hello {
                client.record_connection(state);
            }
            response
        } else {
            RpcResponse {
                id: request.id,
                error: Some(
                    vetro_agent_api::AgentError::new(
                        ErrorCode::Unauthorized,
                        "Call app.hello with a valid session",
                    )
                    .into(),
                ),
                result: None,
            }
        };
        socket
            .send(Message::Text(serde_json::to_string(&response).ok()?.into()))
            .await
            .ok()?;
        if connection.hello {
            return client;
        }
    }
}

async fn websocket(mut socket: WebSocket, state: Arc<HttpState>, host: RequestHost) {
    const MAX_REQUESTS: usize = 32;
    const RESYNC: &str = r#"{"event":"app.resync","data":{}}"#;
    let mut connection = rpc::Connection::default();
    let mut stopping = state.stopping.clone();
    if *stopping.borrow() {
        return;
    }
    let client = tokio::select! {
        _ = stopping.changed() => return,
        result = tokio::time::timeout(Duration::from_secs(4), handshake(&mut socket, &state, host, &mut connection)) => {
            let Ok(Some(client)) = result else { return };
            client
        }
    };
    let mut events = state.core.events.subscribe();
    let mut requests = tokio::task::JoinSet::new();
    let mut pending_opens = HashMap::new();
    let invalidated = client.invalidated(&state, state.auth.changes());
    tokio::pin!(invalidated);
    if *stopping.borrow() {
        return;
    }
    loop {
        let mut completed = None;
        let mut self_revoked = false;
        let message = tokio::select! {
            _ = &mut invalidated => break,
            _ = stopping.changed() => break,
            incoming = socket.recv() => {
                let Some(Ok(incoming)) = incoming else { break };
                match incoming {
                    Message::Text(text) => {
                        let Ok(request) = serde_json::from_str::<RpcRequest>(&text) else {
                            break;
                        };
                        if !client.valid(&state) {
                            break;
                        }
                        let self_revoke = request.method == "remote.revokeDevice"
                            && request.params["deviceId"] == client.device_key();
                        let thread_id = request.params["threadId"].as_str().map(str::to_owned);
                        if self_revoke || matches!(request.method.as_str(), "threads.close" | "prefs.set") {
                            if request.method == "threads.close" {
                                if let Some(id) = thread_id {
                                    pending_opens.remove(&id);
                                }
                            }
                            let response = rpc::dispatch(&state, &client, &mut connection, request).await;
                            self_revoked = self_revoke && response.error.is_none();
                            serde_json::to_string(&response)
                        } else if requests.len() >= MAX_REQUESTS {
                            serde_json::to_string(&json!({
                                "id": request.id,
                                "error": {"code": "busy", "message": "Too many concurrent requests"}
                            }))
                        } else {
                            let opened = if matches!(request.method.as_str(), "threads.open" | "threads.rollback") {
                                thread_id.inspect(|id| {
                                    pending_opens.insert(id.clone(), request.id);
                                })
                            } else {
                                None
                            };
                            let state = state.clone();
                            let client = client.clone();
                            requests.spawn(async move {
                                let mut request_connection = rpc::Connection::default();
                                request_connection.hello = true;
                                let response = rpc::dispatch(&state, &client, &mut request_connection, request).await;
                                (response, request_connection, opened)
                            });
                            continue;
                        }
                    }
                    Message::Close(_) => break,
                    _ => continue,
                }
            }
            Some(result) = requests.join_next(), if !requests.is_empty() => {
                let Ok((response, request_connection, opened)) = result else { continue };
                if opened.is_none_or(|id| {
                    if pending_opens.get(&id) != Some(&response.id) {
                        return false;
                    }
                    pending_opens.remove(&id);
                    true
                }) {
                    completed = Some(request_connection);
                }
                serde_json::to_string(&response)
            }
            event = events.recv() => {
                if !client.valid(&state) {
                    break;
                }
                match event {
                    Ok(event) if !connection.accepts(&event) => continue,
                    Ok(event) if matches!(&*event, vetro_proto::rpc::ServerEvent::RemoteUpdated { .. }) => {
                        serde_json::to_string(&vetro_proto::rpc::ServerEvent::RemoteUpdated {
                        status: auth::status(&state, Some(&client)),
                        })
                    }
                    Ok(event) => serde_json::to_string(&*event),
                    Err(broadcast::error::RecvError::Lagged(_)) if connection.hello => Ok(RESYNC.into()),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        };
        let Ok(message) = message else { break };
        if self_revoked {
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                socket.send(Message::Text(message.into())).await?;
                socket.send(Message::Close(None)).await
            })
            .await;
            break;
        }
        let mut outgoing = vec![message];
        if let Some(completed) = completed {
            match connection.merge(completed) {
                Some(events) => {
                    for event in events {
                        if connection.accepts(&event) {
                            if let Ok(message) = serde_json::to_string(&*event) {
                                outgoing.push(message);
                            }
                        }
                    }
                }
                None => outgoing.push(RESYNC.into()),
            }
        }
        for message in outgoing {
            let sent = tokio::select! {
                _ = &mut invalidated => return,
                _ = stopping.changed() => return,
                sent = tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Text(message.into()))) => sent,
            };
            if !matches!(sent, Ok(Ok(()))) {
                return;
            }
        }
    }
}

async fn static_ui(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let uri = request.uri();
    if uri.path() == "/api" || uri.path().starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let target = uri.path().trim_start_matches('/');
    let target = if target.is_empty() {
        "index.html"
    } else {
        target
    };
    if target.contains(['\\', ':'])
        || Path::new(target)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut asset = None;
    if let Some(root) = &state.web_dir {
        if let Ok(root) = tokio::fs::canonicalize(root).await {
            for name in [target, "index.html"] {
                if let Ok(path) = tokio::fs::canonicalize(root.join(name)).await {
                    if path.starts_with(&root) {
                        if let Ok(bytes) = tokio::fs::read(path).await {
                            asset = Some((name, Body::from(bytes)));
                            break;
                        }
                    }
                }
            }
        }
    } else {
        asset = embedded(target).or_else(|| embedded("index.html"));
    }
    let (name, body) = asset.unwrap_or_else(|| ("index.html", Body::from(PLACEHOLDER)));
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    let body = if request.method() == Method::HEAD {
        Body::empty()
    } else {
        body
    };
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref()).unwrap(),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn embedded(name: &str) -> Option<(&str, Body)> {
    let (_, bytes) = EMBEDDED.iter().find(|(path, _)| *path == name)?;
    Some((name, Body::from(*bytes)))
}

#[cfg(test)]
mod port_tests {
    use super::*;

    #[tokio::test]
    async fn chooses_a_stable_default_and_skips_bound_ports() {
        let first = bind_loopback(None).await.unwrap();
        let second = bind_loopback(None).await.unwrap();
        let first_port = first.local_addr().unwrap().port();
        assert!(first_port >= DEFAULT_PORT);
        assert!(second.local_addr().unwrap().port() > first_port);
    }

    #[tokio::test]
    async fn lagging_websocket_requests_resynchronization_without_closing() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite};
        use vetro_proto::rpc::ServerEvent;

        let root = tempfile::tempdir().unwrap();
        let home = crate::home::Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let session = home.new_local_session().unwrap();
        let core = Core::new(home, crate::registry::Registry::default());
        let server = Server::bind(core.clone(), Some(0)).await.unwrap();
        let request = format!("ws://{}/api/ws", server.address());
        let (stop, stopping) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve(async {
            let _ = stopping.await;
        }));
        let (mut socket, _) = connect_async(request).await.unwrap();
        socket.send(tungstenite::Message::Text(json!({
            "id": 1, "method": "app.hello", "params": {"protocolVersion": 1, "clientName": "test", "session": session},
        }).to_string().into())).await.unwrap();
        let frame = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(frame.to_text().unwrap()).unwrap()["id"],
            1
        );
        for _ in 0..2048 {
            core.publish(ServerEvent::ProjectsUpdated {
                projects: Vec::new(),
            });
        }
        let frame = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(frame.to_text().unwrap()).unwrap()["event"],
            "app.resync"
        );
        socket
            .send(tungstenite::Message::Text(
                json!({
                    "id": 2, "method": "projects.list", "params": {},
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = socket.next().await.unwrap().unwrap();
                let value: serde_json::Value =
                    serde_json::from_str(frame.to_text().unwrap()).unwrap();
                if value["id"] == 2 {
                    assert_eq!(value["result"]["projects"], json!([]));
                    break;
                }
            }
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }
}
