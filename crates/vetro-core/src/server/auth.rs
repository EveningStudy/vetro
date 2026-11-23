use std::sync::Mutex;

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use vetro_agent_api::{AgentError, Result};
use vetro_proto::rpc::{
    ErrorCode, Pairing, RemoteDevice, RemoteStatus, ServerEvent, TailscaleStatus,
};

use super::HttpState;
use crate::{
    bad_request,
    home::{equal, secret, Device, Home},
    not_found, now,
};

const COOKIE: &str = "vetro_session";
const PAIR_TTL: i64 = 5 * 60 * 1000;

pub(super) struct Auth {
    local_cookie: String,
    pairing: Mutex<Option<Pairing>>,
}

pub(super) enum Client {
    Local,
    Remote { id: String, secret: String },
}

impl Client {
    pub(super) fn remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    pub(super) fn valid(&self, home: &Home) -> bool {
        match self {
            Self::Local => true,
            Self::Remote { id, secret } => home.read_state(|state| {
                state
                    .devices
                    .get(id)
                    .is_some_and(|device| equal(&device.secret, secret))
            }),
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
    pub(super) fn new(local_cookie: String) -> Self {
        Self {
            local_cookie,
            pairing: Mutex::new(None),
        }
    }

    pub(super) fn client(&self, home: &Home, headers: &HeaderMap) -> Option<Client> {
        let token = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|header| header.to_str().ok())
            .flat_map(|value| value.split(';'))
            .find_map(|part| part.trim().strip_prefix("vetro_session="))?;
        if equal(token, &self.local_cookie) {
            return Some(Client::Local);
        }
        let (id, token) = token.split_once('.')?;
        let client = Client::Remote {
            id: id.into(),
            secret: token.into(),
        };
        client.valid(home).then_some(client)
    }

    pub(super) fn create_pairing(&self) -> Result<Pairing> {
        let pairing = Pairing {
            code: secret(6)?,
            url: None,
            expires_at: now() + PAIR_TTL,
        };
        *self.pairing.lock().unwrap() = Some(pairing.clone());
        Ok(pairing)
    }

    fn exchange(&self, home: &Home, code: &str, name: String) -> Result<String> {
        let mut pairing = self.pairing.lock().unwrap();
        let timestamp = now();
        if !pairing
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
            secret: secret.clone(),
            details: RemoteDevice {
                id: id.clone(),
                name,
                created_at: timestamp,
                last_seen_at: Some(timestamp),
                current: false,
            },
        };
        home.change_state(|state| state.devices.insert(id.clone(), device))?;
        pairing.take();
        Ok(format!("{id}.{secret}"))
    }
}

#[derive(Deserialize)]
pub(super) struct LocalRequest {
    token: String,
}

pub(super) async fn local(
    State(state): State<std::sync::Arc<HttpState>>,
    headers: HeaderMap,
    Json(request): Json<LocalRequest>,
) -> Response {
    if headers.contains_key("forwarded")
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
    Json(request): Json<PairRequest>,
) -> Response {
    let name = request.device_name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match state
        .auth
        .exchange(&state.core.home, request.code.trim(), name.into())
    {
        Ok(token) => {
            state.core.publish(ServerEvent::RemoteUpdated {
                status: status(&state, &Client::Local),
            });
            cookie(&token, true)
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
    headers: HeaderMap,
) -> Response {
    let Some(client) = state.auth.client(&state.core.home, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    with_no_store(
        Json(json!({"authenticated": true, "remoteClient": client.remote()})).into_response(),
    )
}

pub(super) fn status(state: &HttpState, client: &Client) -> RemoteStatus {
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
        enabled: false,
        port: state.address.port(),
        tailscale: TailscaleStatus {
            installed: false,
            running: false,
            dns_name: None,
            serve_url: None,
        },
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
    let status = status(state, client);
    state.core.publish(ServerEvent::RemoteUpdated {
        status: status.clone(),
    });
    Ok(status)
}

fn cookie(token: &str, persistent: bool) -> Response {
    let max_age = if persistent { "; Max-Age=2592000" } else { "" };
    let mut response = with_no_store(Json(json!({"ok": true})).into_response());
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/{max_age}"
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
