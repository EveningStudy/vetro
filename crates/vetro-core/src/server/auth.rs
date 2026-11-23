use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::RwLock,
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    http::{header, uri::Authority, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{watch, Mutex as AsyncMutex};
use vetro_agent_api::{AgentError, Result};
use vetro_proto::rpc::{
    ErrorCode, Pairing, RemoteDevice, RemoteSetParams, RemoteStatus, ServerEvent, TailscaleStatus,
};

use super::{
    tailscale::{Serve, Tailscale, HTTPS_PORT},
    HttpState, RequestHost,
};
use crate::{
    bad_request,
    home::{device_token_hash, equal, secret, Device, Home},
    not_found, now,
};

const COOKIE: &str = "vetro_session";
const REMOTE_COOKIE: &str = "__Host-vetro_session";
const PAIR_TTL: i64 = 10 * 60 * 1000;

pub(super) struct Auth {
    local_cookie: String,
    tailscale: Option<Tailscale>,
    serve: AsyncMutex<Option<Serve>>,
    remote: RwLock<Remote>,
    changes: watch::Sender<()>,
}

struct Remote {
    enabled: bool,
    tailscale: TailscaleStatus,
    pairing: Option<Pairing>,
    attempts: VecDeque<Instant>,
}

pub(super) enum Client {
    Local,
    Remote { id: String, token_hash: String },
}

impl Client {
    pub(super) fn remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    pub(super) fn valid(&self, state: &HttpState) -> bool {
        match self {
            Self::Local => true,
            Self::Remote { id, token_hash } => {
                state.auth.remote.read().unwrap().enabled
                    && state.core.home.read_state(|state| {
                        state
                            .devices
                            .get(id)
                            .is_some_and(|device| equal(&device.token_hash, token_hash))
                    })
            }
        }
    }

    pub(super) async fn invalidated(&self, state: &HttpState, mut changes: watch::Receiver<()>) {
        while self.valid(state) && changes.changed().await.is_ok() {}
    }

    pub(super) fn desktop(&self) -> Result<()> {
        if self.remote() {
            return Err(AgentError::new(
                ErrorCode::Unauthorized,
                "Local authentication required",
            ));
        }
        Ok(())
    }
}

impl Auth {
    pub(super) fn new(local_cookie: String) -> Self {
        let tailscale = Tailscale::find();
        Self {
            local_cookie,
            remote: RwLock::new(Remote {
                enabled: false,
                tailscale: TailscaleStatus {
                    installed: tailscale.is_some(),
                    running: false,
                    dns_name: None,
                    serve_url: None,
                },
                pairing: None,
                attempts: VecDeque::new(),
            }),
            tailscale,
            serve: AsyncMutex::new(None),
            changes: watch::channel(()).0,
        }
    }

    pub(super) fn changes(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    pub(super) fn allows(&self, host: &Authority) -> bool {
        let remote = self.remote.read().unwrap();
        remote.enabled
            && remote.tailscale.running
            && host.port_u16() == Some(HTTPS_PORT)
            && remote
                .tailscale
                .dns_name
                .as_ref()
                .is_some_and(|dns| dns.eq_ignore_ascii_case(host.host()))
    }

    pub(super) async fn refresh(&self) {
        let mut serve = self.serve.lock().await;
        if let Some(tailscale) = &self.tailscale {
            let mut status = tailscale.status().await;
            if serve
                .as_ref()
                .is_some_and(|entry| entry.process.has_exited())
            {
                serve.take();
            }
            if let Some(entry) = serve.as_ref() {
                status.serve_url = Some(entry.url.clone());
            }
            self.remote.write().unwrap().tailscale = status;
        }
    }

    pub(super) async fn set(&self, address: SocketAddr, params: RemoteSetParams) -> Result<()> {
        let mut serve = self.serve.lock().await;
        if !params.enabled {
            let mut remote = self.remote.write().unwrap();
            remote.enabled = false;
            remote.pairing = None;
            remote.tailscale.serve_url = None;
            self.changes.send_replace(());
        }
        if !params.enabled || !params.tailscale_serve {
            if let Some(owned) = serve.take() {
                owned.process.kill().await.map_err(crate::internal)?;
            }
        }
        let mut status = match &self.tailscale {
            Some(tailscale) => tailscale.status().await,
            None => self.remote.read().unwrap().tailscale.clone(),
        };
        if params.enabled && params.tailscale_serve {
            let tailscale = self
                .tailscale
                .as_ref()
                .ok_or_else(|| bad_request("Tailscale CLI not found"))?;
            let dns = status
                .dns_name
                .as_deref()
                .filter(|_| status.running)
                .ok_or_else(|| bad_request("Tailscale is not running or has no DNS name"))?;
            if serve
                .as_ref()
                .is_some_and(|entry| entry.process.has_exited())
            {
                serve.take();
            }
            if serve.is_none() {
                *serve = Some(tailscale.start(dns, address).await?);
            }
            status.serve_url = serve.as_ref().map(|entry| entry.url.clone());
        }
        let mut remote = self.remote.write().unwrap();
        remote.tailscale = status;
        remote.enabled = params.enabled;
        Ok(())
    }

    pub(super) async fn stop(&self) {
        if let Some(owned) = self.serve.lock().await.take() {
            let _ = owned.process.kill().await;
        }
    }

    pub(super) fn client(
        &self,
        state: &HttpState,
        headers: &HeaderMap,
        host: RequestHost,
    ) -> Option<Client> {
        let name = if host == RequestHost::Local {
            COOKIE
        } else {
            REMOTE_COOKIE
        };
        let token = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|header| header.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|part| part.trim().split_once('='))
            .find_map(|(key, value)| (key == name).then_some(value))?;
        if host == RequestHost::Local {
            return equal(token, &self.local_cookie).then_some(Client::Local);
        }
        let (id, token) = token.split_once('.')?;
        let client = Client::Remote {
            id: id.into(),
            token_hash: device_token_hash(token),
        };
        client.valid(state).then_some(client)
    }

    pub(super) fn create_pairing(&self) -> Result<Pairing> {
        let mut remote = self.remote.write().unwrap();
        if !remote.enabled {
            return Err(bad_request("Enable remote access before pairing a device"));
        }
        let code = secret(4)?;
        let pairing = Pairing {
            url: remote
                .tailscale
                .serve_url
                .as_ref()
                .map(|url| format!("{url}/#pair={code}")),
            code,
            expires_at: now() + PAIR_TTL,
        };
        remote.pairing = Some(pairing.clone());
        Ok(pairing)
    }

    fn exchange(&self, home: &Home, code: &str, name: String) -> Result<Option<String>> {
        let mut remote = self.remote.write().unwrap();
        if !remote.enabled {
            return Err(AgentError::new(
                ErrorCode::Unauthorized,
                "Remote access is disabled",
            ));
        }
        let current = Instant::now();
        while remote
            .attempts
            .front()
            .is_some_and(|previous| current.duration_since(*previous) >= Duration::from_secs(60))
        {
            remote.attempts.pop_front();
        }
        if remote.attempts.len() >= 10 {
            return Ok(None);
        }
        remote.attempts.push_back(current);
        let timestamp = now();
        if !remote
            .pairing
            .as_ref()
            .is_some_and(|pairing| pairing.expires_at > timestamp && equal(&pairing.code, code))
        {
            return Err(AgentError::new(
                ErrorCode::Unauthorized,
                "Invalid or expired pairing code",
            ));
        }
        let id = secret(16)?;
        let secret = secret(32)?;
        let device = Device {
            token_hash: device_token_hash(&secret),
            details: RemoteDevice {
                id: id.clone(),
                name,
                created_at: timestamp,
                last_seen_at: Some(timestamp),
                current: false,
            },
        };
        home.change_state(|state| state.devices.insert(id.clone(), device))?;
        remote.pairing.take();
        Ok(Some(format!("{id}.{secret}")))
    }
}

#[derive(Deserialize)]
pub(super) struct LocalRequest {
    token: String,
}

pub(super) async fn local(
    State(state): State<std::sync::Arc<HttpState>>,
    axum::Extension(host): axum::Extension<RequestHost>,
    headers: HeaderMap,
    Json(request): Json<LocalRequest>,
) -> Response {
    if host != RequestHost::Local
        || headers.contains_key("forwarded")
        || headers.contains_key("x-forwarded-for")
        || headers.contains_key("x-forwarded-host")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.core.home.check_token(&request.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    cookie(&state.auth.local_cookie, false)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PairRequest {
    code: String,
    device_name: String,
}

pub(super) async fn pair(
    State(state): State<std::sync::Arc<HttpState>>,
    axum::Extension(host): axum::Extension<RequestHost>,
    Json(request): Json<PairRequest>,
) -> Response {
    if host != RequestHost::Remote {
        return StatusCode::FORBIDDEN.into_response();
    }
    let name = request.device_name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match state
        .auth
        .exchange(&state.core.home, request.code.trim(), name.into())
    {
        Ok(Some(token)) => {
            state.core.publish(ServerEvent::RemoteUpdated {
                status: status(&state, &Client::Local),
            });
            cookie(&token, true)
        }
        Ok(None) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(error) if error.code == ErrorCode::Unauthorized => {
            StatusCode::UNAUTHORIZED.into_response()
        }
        Err(error) => {
            state.core.home.log(&format!("Pairing failed: {error}"));
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(super) async fn session(
    State(state): State<std::sync::Arc<HttpState>>,
    axum::Extension(host): axum::Extension<RequestHost>,
    headers: HeaderMap,
) -> Response {
    let Some(client) = state.auth.client(&state, &headers, host) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    with_no_store(
        Json(json!({"authenticated": true, "remoteClient": client.remote()})).into_response(),
    )
}

pub(super) fn status(state: &HttpState, client: &Client) -> RemoteStatus {
    let remote = state.auth.remote.read().unwrap();
    let devices = state.core.home.read_state(|state| {
        state
            .devices
            .values()
            .map(|device| {
                let mut details = device.details.clone();
                details.current = matches!(client, Client::Remote { id, .. } if *id == details.id);
                details
            })
            .collect()
    });
    RemoteStatus {
        enabled: remote.enabled,
        port: state.address.port(),
        tailscale: remote.tailscale.clone(),
        devices,
    }
}

pub(super) fn revoke(state: &HttpState, client: &Client, device_id: &str) -> Result<RemoteStatus> {
    if matches!(client, Client::Remote { id, .. } if id != device_id) {
        return Err(bad_request(
            "A paired device can only revoke its own session",
        ));
    }
    if state
        .core
        .home
        .change_state(|state| state.devices.remove(device_id))?
        .is_none()
    {
        return Err(not_found("Unknown device"));
    }
    state.auth.changes.send_replace(());
    let status = status(state, client);
    state.core.publish(ServerEvent::RemoteUpdated {
        status: status.clone(),
    });
    Ok(status)
}

fn cookie(token: &str, persistent: bool) -> Response {
    let (name, extra) = if persistent {
        (REMOTE_COOKIE, "; Secure; Max-Age=2592000")
    } else {
        (COOKIE, "")
    };
    let mut response = with_no_store(Json(json!({"ok": true})).into_response());
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{name}={token}; HttpOnly; SameSite=Strict; Path=/{extra}"
        ))
        .unwrap(),
    );
    response
}

fn with_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
