use std::{
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
use vetro_proto::rpc::RpcRequest;

use crate::{home::secret, internal, Core};

mod auth;
mod rpc;

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
            auth: auth::Auth::new(secret(32)?),
            address,
            web_dir: std::env::var_os("VETRO_WEB_DIR").map(PathBuf::from),
            stopping,
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

    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> io::Result<()> {
        let core = self.state.core.clone();
        let app = Router::new()
            .route("/api/health", get(health))
            .route("/api/auth/local", post(auth::local))
            .route("/api/auth/pair", post(auth::pair))
            .route("/api/auth/session", get(auth::session))
            .route("/api/ws", get(upgrade))
            .fallback(static_ui)
            .layer(middleware::from_fn(check_origin))
            .with_state(self.state);
        let history = tokio::spawn(core.clone().history_worker());
        let sessions = tokio::spawn(core.clone().session_worker());
        let result = axum::serve(self.listener, app)
            .with_graceful_shutdown(async move {
                shutdown.await;
                self.stop.send_replace(true);
            })
            .await;
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

async fn check_origin(request: Request, next: Next) -> Response {
    let headers = request.headers();
    let Some(host) = authority(headers) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !matches!(
        host.host().to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    ) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let same_origin = origin
            .to_str()
            .ok()
            .and_then(|value| value.parse::<Uri>().ok())
            .is_some_and(|origin| {
                matches!(origin.scheme_str(), Some("http" | "https"))
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
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(client) = state.auth.client(&state.core.home, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade
        .max_message_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| websocket(socket, state, client))
}

async fn websocket(mut socket: WebSocket, state: Arc<HttpState>, client: auth::Client) {
    let mut connection = rpc::Connection::default();
    let mut events = state.core.events.subscribe();
    let mut stopping = state.stopping.clone();
    if *stopping.borrow() {
        return;
    }
    loop {
        let message = tokio::select! {
            _ = stopping.changed() => break,
            incoming = socket.recv() => {
                let Some(Ok(incoming)) = incoming else { break };
                match incoming {
                    Message::Text(text) => {
                        let Ok(request) = serde_json::from_str::<RpcRequest>(&text) else {
                            break;
                        };
                        if !client.valid(&state.core.home) {
                            break;
                        }
                        let response = tokio::select! {
                            _ = stopping.changed() => break,
                            response = rpc::dispatch(&state, &client, &mut connection, request) => response,
                        };
                        serde_json::to_string(&response)
                    }
                    Message::Close(_) => break,
                    _ => continue,
                }
            }
            event = events.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_) | broadcast::error::RecvError::Closed) => break,
                };
                if !client.valid(&state.core.home) {
                    break;
                }
                if !connection.accepts(&event) {
                    continue;
                }
                if matches!(&*event, vetro_proto::rpc::ServerEvent::RemoteUpdated { .. }) {
                    serde_json::to_string(&vetro_proto::rpc::ServerEvent::RemoteUpdated {
                        status: auth::status(&state, &client),
                    })
                } else {
                    serde_json::to_string(&*event)
                }
            }
        };
        let Ok(message) = message else { break };
        let sent = tokio::select! {
            _ = stopping.changed() => break,
            sent = tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Text(message.into()))) => sent,
        };
        if !matches!(sent, Ok(Ok(()))) {
            break;
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
}
