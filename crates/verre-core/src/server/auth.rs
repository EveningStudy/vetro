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
use verre_agent_api::{AgentError, Result};
use verre_proto::rpc::{
    AuthSession, ErrorCode, Pairing, RemoteDevice, RemoteSetParams, RemoteStatus, ServerEvent,
    TailscaleStatus,
};

use super::{
    tailscale::{Serve, Tailscale},
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
    tailscale: TailscaleStatus,
    external: Option<ExternalOrigin>,
    pairing: Option<Pairing>,
    attempts: VecDeque<(Instant, IpAddr)>,
    peer_attempts: HashMap<IpAddr, usize>,
}

struct ExternalOrigin {
    url: String,
    authority: Authority,
}

impl ExternalOrigin {
    fn parse(input: &str) -> Result<Self> {
        let error =
            || bad_request("Enter an HTTPS origin with no credentials, path, query, or fragment");
        let input = input.trim();
        let uri: axum::http::Uri = input.parse().map_err(|_| error())?;
        if uri.scheme_str() != Some("https")
            || uri
                .authority()
                .is_none_or(|host| host.as_str().contains('@'))
            || uri.path() != "/"
            || uri.query().is_some()
            || input.contains(['#', '\\'])
        {
            return Err(error());
        }
        let url = reqwest::Url::parse(input).map_err(|_| error())?;
        let loopback = match url.host_str() {
            Some(host) if host.trim_end_matches('.').eq_ignore_ascii_case("localhost") => true,
            Some(host) => host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|address| {
                    address.to_canonical().is_loopback() || address.is_unspecified()
                }),
            None => true,
        };
        if loopback {
            return Err(bad_request(
                "The remote HTTPS origin must use a non-loopback hostname or address",
            ));
        }
        let url = url.origin().ascii_serialization();
        let authority = url
            .trim_start_matches("https://")
            .parse()
            .map_err(|_| error())?;
        Ok(Self { url, authority })
    }
}

impl Remote {
    fn allows(&self, host: &Authority) -> bool {
        self.external
            .as_ref()
            .is_some_and(|origin| &origin.authority == host)
            || self.tailscale.serve_url.as_deref().is_some_and(|url| {
                url.trim_start_matches("https://")
                    .eq_ignore_ascii_case(host.as_str())
            })
    }
}

#[derive(Clone)]
pub(super) enum Client {
    Local {
        token_hash: String,
    },
    Remote {
        id: String,
        token_hash: String,
        authority: Authority,
    },
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
            Self::Remote {
                id,
                token_hash,
                authority,
            } => {
                state.auth.allows(authority)
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

    pub(super) fn record_connection(&self, state: &HttpState) {
        let Self::Remote {
            id,
            token_hash,
            authority,
        } = self
        else {
            return;
        };
        let remote = state.auth.remote.read().unwrap();
        if !remote.allows(authority) {
            return;
        }
        let timestamp = now();
        let updated = state.core.home.change_state(|home| {
            let Some(device) = home.devices.get_mut(id).filter(|device| {
                equal(&device.token_hash, token_hash)
                    && device
                        .details
                        .last_seen_at
                        .is_none_or(|previous| previous < timestamp)
            }) else {
                return false;
            };
            device.details.last_seen_at = Some(timestamp);
            true
        });
        drop(remote);
        match updated {
            Ok(true) => state.core.publish(ServerEvent::RemoteUpdated {
                status: status(state, None),
            }),
            Ok(false) => {}
            Err(error) => state
                .core
                .home
                .log(&format!("Could not record remote connection: {error}")),
        }
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
                tailscale: TailscaleStatus {
                    installed: tailscale.is_some(),
                    running: false,
                    dns_name: None,
                    serve_url: None,
                },
                external: None,
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
        self.remote.read().unwrap().allows(host)
    }

    pub(super) fn enable_remote_proxy(&self, origin: &str) -> Result<()> {
        let origin = ExternalOrigin::parse(origin)?;
        let mut remote = self.remote.write().unwrap();
        remote.external = Some(origin);
        Ok(())
    }

    pub(super) async fn watch_serve(&self, state: &HttpState) {
        let mut changes = self.changes();
        loop {
            let exited = self
                .serve
                .lock()
                .await
                .as_ref()
                .map(|entry| entry.process.wait());
            if let Some(exited) = exited {
                tokio::select! {
                    _ = exited => {
                        let mut serve = self.serve.lock().await;
                        self.reap_serve(state, &mut serve);
                    }
                    _ = changes.changed() => {}
                }
            } else if changes.changed().await.is_err() {
                return;
            }
        }
    }

    fn reap_serve(&self, state: &HttpState, serve: &mut Option<Serve>) {
        if !serve
            .as_ref()
            .is_some_and(|entry| entry.process.has_exited())
        {
            return;
        }
        serve.take();
        {
            let mut remote = self.remote.write().unwrap();
            remote.tailscale.serve_url = None;
            remote.pairing = None;
            self.changes.send_replace(());
        }
        state.core.publish(ServerEvent::RemoteUpdated {
            status: status(state, None),
        });
    }

    pub(super) async fn refresh(&self, state: &HttpState) {
        let mut serve = self.serve.lock().await;
        if let Some(tailscale) = &self.tailscale {
            let mut status = tailscale.status().await;
            self.reap_serve(state, &mut serve);
            if let Some(entry) = serve.as_ref().filter(|_| status.running) {
                status.serve_url = Some(entry.url.clone());
            }
            let mut remote = self.remote.write().unwrap();
            if remote.tailscale.serve_url != status.serve_url {
                remote.pairing = None;
                self.changes.send_replace(());
            }
            remote.tailscale = status;
        }
    }

    pub(super) async fn set(&self, state: &HttpState, params: RemoteSetParams) -> Result<()> {
        let external = if params.enabled {
            match (params.tailscale_serve, params.external_url.as_deref()) {
                (true, None) => None,
                (false, Some(url)) => Some(ExternalOrigin::parse(url)?),
                _ => return Err(bad_request("Choose one remote connection method")),
            }
        } else {
            None
        };
        let mut serve = self.serve.lock().await;
        self.reap_serve(state, &mut serve);
        if !params.enabled {
            let mut remote = self.remote.write().unwrap();
            remote.pairing = None;
            remote.tailscale.serve_url = None;
            remote.external = None;
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
            self.reap_serve(state, &mut serve);
            if serve.is_none() {
                *serve = Some(tailscale.start(dns, state.address).await?);
            }
            status.serve_url = serve.as_ref().map(|entry| entry.url.clone());
        }
        let mut remote = self.remote.write().unwrap();
        if remote.tailscale.serve_url != status.serve_url
            || remote.external.as_ref().map(|origin| &origin.url)
                != external.as_ref().map(|origin| &origin.url)
        {
            remote.pairing = None;
        }
        remote.tailscale = status;
        remote.external = external;
        self.changes.send_replace(());
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
        host: &RequestHost,
    ) -> Option<Client> {
        let client = match host {
            RequestHost::Local => Client::Local {
                token_hash: device_token_hash(token),
            },
            RequestHost::Remote(authority) => {
                let (id, token) = token.split_once('.')?;
                Client::Remote {
                    id: id.into(),
                    token_hash: device_token_hash(token),
                    authority: authority.clone(),
                }
            }
        };
        client.valid(state).then_some(client)
    }

    pub(super) fn create_pairing(&self) -> Result<Pairing> {
        let mut remote = self.remote.write().unwrap();
        let address = remote
            .external
            .as_ref()
            .map(|origin| &origin.url)
            .or(remote.tailscale.serve_url.as_ref())
            .ok_or_else(|| bad_request("Start a remote connection before pairing a device"))?;
        let code = secret(4)?;
        let pairing = Pairing {
            url: Some(format!("{address}/#pair={code}")),
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
        request: PairRequest,
        peer: IpAddr,
        authority: &Authority,
    ) -> Result<Option<String>> {
        let name = request.device_name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(bad_request("Enter a device name of up to 100 characters"));
        }
        let proposed = request
            .proposed_session
            .as_deref()
            .map(parse_proposed_session)
            .transpose()?;
        let mut remote = self.remote.write().unwrap();
        if !remote.allows(authority) {
            return Err(AgentError::new(
                ErrorCode::Unauthorized,
                "Remote access is disabled",
            ));
        }
        if let Some((id, token)) = proposed {
            let hash = device_token_hash(token);
            if home.read_state(|state| {
                state
                    .devices
                    .get(id)
                    .is_some_and(|device| equal(&device.token_hash, &hash))
            }) {
                return Ok(request.proposed_session);
            }
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
        if !remote.pairing.as_ref().is_some_and(|pairing| {
            pairing.expires_at > timestamp && equal(&pairing.code, request.code.trim())
        }) {
            return Err(AgentError::new(
                ErrorCode::Unauthorized,
                "Invalid or expired pairing code",
            ));
        }
        let (id, token) = match proposed {
            Some((id, token)) => (id.to_owned(), token.to_owned()),
            None => (secret(16)?, secret(32)?),
        };
        let device = Device {
            token_hash: device_token_hash(&token),
            details: RemoteDevice {
                id: id.clone(),
                name: name.into(),
                created_at: timestamp,
                last_seen_at: None,
                current: false,
            },
        };
        let inserted = home.change_state(|state| {
            let std::collections::btree_map::Entry::Vacant(entry) = state.devices.entry(id.clone())
            else {
                return false;
            };
            entry.insert(device);
            true
        })?;
        if !inserted {
            return Err(bad_request("The proposed device ID is already in use"));
        }
        remote.pairing.take();
        Ok(Some(format!("{id}.{token}")))
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
    if !matches!(host, RequestHost::Local)
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
    proposed_session: Option<String>,
}

fn parse_proposed_session(value: &str) -> Result<(&str, &str)> {
    let hex = |part: &str, len| {
        part.len() == len
            && part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    value
        .split_once('.')
        .filter(|(id, token)| hex(id, 32) && hex(token, 64))
        .ok_or_else(|| bad_request("Invalid proposed pairing session"))
}

pub(super) async fn pair(
    State(state): State<std::sync::Arc<HttpState>>,
    axum::Extension(host): axum::Extension<RequestHost>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<PairRequest>,
) -> Response {
    let RequestHost::Remote(authority) = host else {
        return StatusCode::FORBIDDEN.into_response();
    };
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
        .exchange(&state.core.home, request, peer, &authority)
    {
        Ok(Some(token)) => {
            state.core.publish(ServerEvent::RemoteUpdated {
                status: status(&state, None),
            });
            session_response(token, true)
        }
        Ok(None) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(error) if error.code == ErrorCode::BadRequest => {
            StatusCode::BAD_REQUEST.into_response()
        }
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
        .and_then(|(_, token)| state.auth.client(&state, token, &host))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    with_no_store(
        Json(json!({"authenticated": true, "remoteClient": client.remote()})).into_response(),
    )
}

pub(super) fn status(state: &HttpState, client: Option<&Client>) -> RemoteStatus {
    let remote = state.auth.remote.read().unwrap();
    let mut devices: Vec<_> = state.core.home.read_state(|state| {
        state
            .devices
            .values()
            .filter_map(|device| {
                let mut details = device.details.clone();
                details.current =
                    matches!(client, Some(Client::Remote { id, .. }) if *id == details.id);
                (!client.is_some_and(Client::remote) || details.current).then_some(details)
            })
            .collect()
    });
    devices.sort_by(|left, right| (left.created_at, &left.id).cmp(&(right.created_at, &right.id)));
    RemoteStatus {
        enabled: remote.external.is_some() || remote.tailscale.serve_url.is_some(),
        port: state.address.port(),
        tailscale: remote.tailscale.clone(),
        external_url: remote.external.as_ref().map(|origin| origin.url.clone()),
        pairing_expires_at: client
            .filter(|client| !client.remote())
            .and(remote.pairing.as_ref())
            .map(|pairing| pairing.expires_at),
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
        let home = Home::open(root.path().join(".verre"), root.path().into()).unwrap();
        let auth = Auth::new();
        auth.enable_remote_proxy("https://verre.example.test")
            .unwrap();
        let authority = "verre.example.test".parse().unwrap();
        let code = auth.create_pairing().unwrap().code;
        let peer = |host| IpAddr::V4(Ipv4Addr::new(100, 64, 0, host));
        let pair = |code: &str, host| {
            auth.exchange(
                &home,
                PairRequest {
                    code: code.into(),
                    device_name: "Test".into(),
                    proposed_session: None,
                },
                peer(host),
                &authority,
            )
        };
        for host in 1..=u8::try_from(PAIR_GLOBAL_LIMIT / PAIR_PEER_LIMIT).unwrap() {
            for _ in 0..PAIR_PEER_LIMIT {
                let error = pair("incorrect", host).unwrap_err();
                assert_eq!(error.code, ErrorCode::Unauthorized);
            }
            assert!(pair(&code, host).unwrap().is_none());
        }
        assert!(pair(&code, 100).unwrap().is_none());
        let replacement = auth.create_pairing().unwrap().code;
        assert!(pair(&replacement, 1).unwrap().is_some());
    }
}
