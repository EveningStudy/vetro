use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::RwLock,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, State},
    http::{header, uri::Authority, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{watch, Mutex as AsyncMutex};
use vetro_agent_api::{AgentError, Result};
use vetro_proto::rpc::{
    AuthSession, ErrorCode, Pairing, RemoteDevice, RemoteSetParams, RemoteStatus, ServerEvent,
    TailscaleStatus,
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

const PAIR_TTL: i64 = 10 * 60 * 1000;
const PAIR_WINDOW: Duration = Duration::from_secs(60);
const PAIR_GLOBAL_LIMIT: usize = 60;
const PAIR_PEER_LIMIT: usize = 10;

pub(super) struct Auth {
    tailscale: Option<Tailscale>,
    serve: AsyncMutex<Option<Serve>>,
    remote: RwLock<Remote>,
    changes: watch::Sender<()>,
}

struct Remote {
    enabled: bool,
    tailscale: TailscaleStatus,
    pairing: Option<Pairing>,
    attempts: VecDeque<(Instant, IpAddr)>,
    peer_attempts: HashMap<IpAddr, usize>,
}

#[derive(Clone)]
pub(super) enum Client {
    Local { token_hash: String },
    Remote { id: String, token_hash: String },
}

impl Client {
    pub(super) fn device_key(&self) -> &str {
        match self {
            Self::Local { .. } => "local",
            Self::Remote { id, .. } => id,
        }
    }

    pub(super) fn remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    pub(super) fn valid(&self, state: &HttpState) -> bool {
        match self {
            Self::Local { token_hash } => state.core.home.check_local_session_hash(token_hash),
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
    pub(super) fn new() -> Self {
        let tailscale = Tailscale::find();
        Self {
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
                peer_attempts: HashMap::new(),
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
        token: &str,
        host: RequestHost,
    ) -> Option<Client> {
        let client = match host {
            RequestHost::Local => Client::Local {
                token_hash: device_token_hash(token),
            },
            RequestHost::Remote => {
                let (id, token) = token.split_once('.')?;
                Client::Remote {
                    id: id.into(),
                    token_hash: device_token_hash(token),
                }
            }
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
        remote.attempts.clear();
        remote.peer_attempts.clear();
        Ok(pairing)
    }

    fn exchange(
        &self,
        home: &Home,
        code: &str,
        name: String,
        peer: IpAddr,
    ) -> Result<Option<String>> {
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
            .is_some_and(|(previous, _)| current.duration_since(*previous) >= PAIR_WINDOW)
        {
            let (_, expired_peer) = remote.attempts.pop_front().unwrap();
            let count = remote.peer_attempts.get_mut(&expired_peer).unwrap();
            *count -= 1;
            if *count == 0 {
                remote.peer_attempts.remove(&expired_peer);
            }
        }
        if remote.attempts.len() >= PAIR_GLOBAL_LIMIT
            || remote.peer_attempts.get(&peer).copied().unwrap_or_default() >= PAIR_PEER_LIMIT
        {
            return Ok(None);
        }
        remote.attempts.push_back((current, peer));
        *remote.peer_attempts.entry(peer).or_default() += 1;
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
    match state.core.home.new_local_session() {
        Ok(token) => session_response(token, false),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<PairRequest>,
) -> Response {
    if host != RequestHost::Remote {
        return StatusCode::FORBIDDEN.into_response();
    }
    let name = request.device_name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let peer = headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()
        .and_then(|header| header.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(peer.ip());
    match state
        .auth
        .exchange(&state.core.home, request.code.trim(), name.into(), peer)
    {
        Ok(Some(token)) => {
            state.core.publish(ServerEvent::RemoteUpdated {
                status: status(&state, None),
            });
            session_response(token, true)
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
    let Some(client) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .and_then(|(_, token)| state.auth.client(&state, token, host))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    with_no_store(
        Json(json!({"authenticated": true, "remoteClient": client.remote()})).into_response(),
    )
}

pub(super) fn status(state: &HttpState, client: Option<&Client>) -> RemoteStatus {
    let remote = state.auth.remote.read().unwrap();
    let devices = state.core.home.read_state(|state| {
        state
            .devices
            .values()
            .map(|device| {
                let mut details = device.details.clone();
                details.current =
                    matches!(client, Some(Client::Remote { id, .. }) if *id == details.id);
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
    if device_id == "local" {
        state
            .core
            .home
            .change_state(|state| state.local_session_hashes.clear())?;
    } else if state
        .core
        .home
        .change_state(|state| {
            let removed = state.devices.remove(device_id);
            if removed.is_some() {
                state.prefs.remove(device_id);
            }
            removed
        })?
        .is_none()
    {
        return Err(not_found("Unknown device"));
    }
    state.auth.changes.send_replace(());
    let status = status(state, Some(client));
    state.core.publish(ServerEvent::RemoteUpdated {
        status: status.clone(),
    });
    Ok(status)
}

fn session_response(session: String, remote_client: bool) -> Response {
    with_no_store(
        Json(AuthSession {
            session,
            remote_client,
        })
        .into_response(),
    )
}

fn with_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn pairing_limits_each_peer_and_all_peers_until_local_reissue() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path().join(".vetro"), root.path().into()).unwrap();
        let auth = Auth::new();
        auth.remote.write().unwrap().enabled = true;
        let code = auth.create_pairing().unwrap().code;
        let peer = |host| IpAddr::V4(Ipv4Addr::new(100, 64, 0, host));
        for host in 1..=u8::try_from(PAIR_GLOBAL_LIMIT / PAIR_PEER_LIMIT).unwrap() {
            for _ in 0..PAIR_PEER_LIMIT {
                let error = auth
                    .exchange(&home, "incorrect", "Test".into(), peer(host))
                    .unwrap_err();
                assert_eq!(error.code, ErrorCode::Unauthorized);
            }
            assert!(auth
                .exchange(&home, &code, "Test".into(), peer(host))
                .unwrap()
                .is_none());
        }
        assert!(auth
            .exchange(&home, &code, "Test".into(), peer(100))
            .unwrap()
            .is_none());
        let replacement = auth.create_pairing().unwrap().code;
        assert!(auth
            .exchange(&home, &replacement, "Test".into(), peer(1))
            .unwrap()
            .is_some());
    }
}
