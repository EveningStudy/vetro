use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use vetro_agent_api::{
    jsonrpc::{Error, Id, RemoteError},
    process::{Process, SpawnSpec, Stdout},
    AgentError, Result, WireLog,
};
use vetro_proto::{rpc::ErrorCode, AuthState};

use crate::session::AcpSession;

pub(crate) type Reply = std::result::Result<Value, Error>;
type Completion = Box<dyn FnOnce(Reply) + Send>;

#[derive(Default)]
struct Dispatch {
    pending: HashMap<u64, Completion>,
    sessions: HashMap<String, Weak<AcpSession>>,
    early_updates: Option<VecDeque<(String, Value)>>,
}

pub(crate) struct Connection {
    process: Process,
    next_id: AtomicU64,
    dead: AtomicBool,
    dispatch: Mutex<Dispatch>,
    pub lifecycle: AsyncMutex<()>,
    pub auth: Mutex<AuthState>,
    auth_required_observed: AtomicBool,
    previous_method: Option<Arc<Mutex<Option<String>>>>,
    attempted_auth: AsyncMutex<bool>,
    signed_in: AtomicBool,
}

impl Connection {
    pub fn spawn(
        spec: SpawnSpec,
        previous_method: Option<Arc<Mutex<Option<String>>>>,
    ) -> Result<Arc<Self>> {
        let (process, stdout) = Process::spawn(spec, Arc::new(WireLog::default().multiplexed()))
            .map_err(|error| AgentError::new(ErrorCode::AgentUnavailable, error.to_string()))?;
        let connection = Arc::new(Self {
            process,
            next_id: AtomicU64::new(1),
            dead: AtomicBool::new(false),
            dispatch: Mutex::new(Dispatch::default()),
            lifecycle: AsyncMutex::new(()),
            auth: Mutex::new(AuthState::Unknown),
            auth_required_observed: AtomicBool::new(false),
            previous_method,
            attempted_auth: AsyncMutex::new(false),
            signed_in: AtomicBool::new(false),
        });
        tokio::spawn(drain(Arc::downgrade(&connection), stdout));
        Ok(connection)
    }

    pub fn alive(&self) -> bool {
        !self.dead.load(Ordering::Acquire) && !self.process.has_exited()
    }

    pub fn log(&self) -> Arc<WireLog> {
        self.process.wire_log()
    }

    pub async fn startup_error(&self, error: Error) -> AgentError {
        let mut error = agent_error(error);
        error.message.push_str(&self.process.exit_detail().await);
        error
    }

    pub async fn stop(&self) {
        self.close(None);
        let _ = self.process.stop().await;
    }

    fn close(&self, error: Option<String>) {
        let (sessions, pending) = {
            let mut dispatch = self.dispatch.lock().unwrap();
            if self.dead.swap(true, Ordering::AcqRel) {
                return;
            }
            dispatch.early_updates = None;
            (
                std::mem::take(&mut dispatch.sessions),
                std::mem::take(&mut dispatch.pending),
            )
        };
        for session in sessions.values().filter_map(Weak::upgrade) {
            session.closed(error.clone());
        }
        for complete in pending.into_values() {
            complete(Err(Error::Closed));
        }
    }

    pub fn capture_new(&self) -> NewCapture<'_> {
        self.dispatch.lock().unwrap().early_updates = Some(VecDeque::new());
        NewCapture(self)
    }

    pub fn register(&self, id: &str, session: &Arc<AcpSession>) -> Result<()> {
        let mut dispatch = self.dispatch.lock().unwrap();
        if !self.alive() {
            return Err(agent_error(Error::Closed));
        }
        if dispatch.sessions.get(id).and_then(Weak::upgrade).is_some() {
            return Err(AgentError::new(
                ErrorCode::Busy,
                "ACP session already attached",
            ));
        }
        dispatch
            .sessions
            .insert(id.to_owned(), Arc::downgrade(session));
        if let Some(updates) = dispatch.early_updates.take() {
            for (_, update) in updates.into_iter().filter(|(target, _)| target == id) {
                session.update(&update);
            }
        }
        Ok(())
    }

    pub fn unregister(&self, id: &str) {
        self.dispatch.lock().unwrap().sessions.remove(id);
    }

    pub fn session(&self, id: &str) -> Option<Arc<AcpSession>> {
        self.dispatch
            .lock()
            .unwrap()
            .sessions
            .get(id)
            .and_then(Weak::upgrade)
    }

    fn update(&self, params: &Value) {
        let Some(id) = params["sessionId"].as_str() else {
            return;
        };
        let mut dispatch = self.dispatch.lock().unwrap();
        if let Some(session) = dispatch.sessions.get(id).and_then(Weak::upgrade) {
            session.update(&params["update"]);
        } else if let Some(updates) = &mut dispatch.early_updates {
            updates.push_back((id.to_owned(), params["update"].clone()));
        }
    }

    async fn write(&self, mut envelope: Value) -> std::result::Result<(), Error> {
        if !self.alive() {
            return Err(Error::Closed);
        }
        envelope["jsonrpc"] = json!("2.0");
        if let Err(error) = self.process.send_line(envelope.to_string()).await {
            self.close(Some(format!("ACP stdin: {error}")));
            return Err(Error::Io(error));
        }
        Ok(())
    }

    fn reserve(&self, complete: Completion) -> std::result::Result<u64, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut dispatch = self.dispatch.lock().unwrap();
        if !self.alive() {
            return Err(Error::Closed);
        }
        dispatch.pending.insert(id, complete);
        Ok(id)
    }

    pub async fn submit(
        &self,
        method: &str,
        params: Value,
        complete: impl FnOnce(Reply) + Send + 'static,
    ) -> Result<()> {
        let id = self.reserve(Box::new(complete)).map_err(agent_error)?;
        self.write(json!({ "id": id, "method": method, "params": params }))
            .await
            .map_err(agent_error)
    }

    pub async fn call(&self, method: &str, params: Value) -> Reply {
        self.call_ordered(method, params, |_| {}).await
    }

    pub async fn call_ordered(
        &self,
        method: &str,
        params: Value,
        before_delivery: impl FnOnce(&Reply) + Send + 'static,
    ) -> Reply {
        let (sender, receiver) = oneshot::channel();
        let id = self.reserve(Box::new(move |reply| {
            before_delivery(&reply);
            let _ = sender.send(reply);
        }))?;
        let pending = Pending(self, id);
        let timeout = if method == "authenticate" { 300 } else { 30 };
        let result = tokio::time::timeout(Duration::from_secs(timeout), async {
            self.write(json!({ "id": id, "method": method, "params": params }))
                .await?;
            receiver.await.map_err(|_| Error::Closed)?
        })
        .await;
        drop(pending);
        result.map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("ACP {method} timed out"),
            ))
        })?
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(json!({ "method": method, "params": params }))
            .await
            .map_err(agent_error)
    }

    pub async fn respond(
        &self,
        id: Id,
        reply: std::result::Result<Value, RemoteError>,
    ) -> Result<()> {
        let message = match reply {
            Ok(result) => json!({ "id": id, "result": result }),
            Err(error) => json!({ "id": id, "error": error }),
        };
        self.write(message).await.map_err(agent_error)
    }

    pub fn observe_auth(&self, method: &str, reply: &Reply) {
        let mut state = self.auth.lock().unwrap();
        if matches!(reply, Err(Error::Remote(error)) if error.code == -32000) {
            self.auth_required_observed.store(true, Ordering::Relaxed);
        }
        *state = observed_auth(*state, self.previous_method.is_some(), method, reply);
    }

    pub fn apply_auth_probe(&self, probe: AuthState) {
        let mut state = self.auth.lock().unwrap();
        if probe != AuthState::Unknown
            && !self.auth_required_observed.load(Ordering::Relaxed)
            && !self.signed_in.load(Ordering::Acquire)
        {
            *state = probe;
        }
    }

    pub async fn authenticate(&self, method: &str) -> Result<()> {
        let _attempt = self.attempted_auth.lock().await;
        self.sign_in(method).await?;
        if let Some(previous) = &self.previous_method {
            *previous.lock().unwrap() = Some(method.to_owned());
        }
        Ok(())
    }

    pub async fn recover_auth(&self) -> Result<bool> {
        let mut attempted = self.attempted_auth.lock().await;
        if *self.auth.lock().unwrap() == AuthState::Ok {
            return Ok(self.signed_in.load(Ordering::Acquire));
        }
        if *attempted || self.signed_in.load(Ordering::Acquire) || !self.alive() {
            return Ok(false);
        }
        let method = self
            .previous_method
            .as_ref()
            .and_then(|method| method.lock().unwrap().clone());
        let Some(method) = method else {
            return Ok(false);
        };
        *attempted = true;
        self.sign_in(&method).await?;
        Ok(true)
    }

    async fn sign_in(&self, method: &str) -> Result<()> {
        let reply = self.call("authenticate", json!({"methodId": method})).await;
        if reply.is_ok() {
            self.signed_in.store(true, Ordering::Release);
        }
        self.observe_auth("authenticate", &reply);
        reply.map_err(agent_error)?;
        Ok(())
    }
}

pub(crate) struct NewCapture<'a>(&'a Connection);

impl Drop for NewCapture<'_> {
    fn drop(&mut self) {
        self.0.dispatch.lock().unwrap().early_updates = None;
    }
}

struct Pending<'a>(&'a Connection, u64);

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.dispatch.lock().unwrap().pending.remove(&self.1);
    }
}

async fn drain(connection: Weak<Connection>, mut stdout: Stdout) {
    let mut failure = None;
    while let Some(line) = stdout.recv().await {
        let Some(connection) = connection.upgrade() else {
            return;
        };
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                failure = Some(format!("ACP stdout: {error}"));
                break;
            }
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(method) = message["method"].as_str() {
            let params = &message["params"];
            if let Some(id) = message
                .get("id")
                .and_then(|id| serde_json::from_value::<Id>(id.clone()).ok())
            {
                let session = params["sessionId"]
                    .as_str()
                    .and_then(|id| connection.session(id));
                let reply = if matches!(method, "session/request_permission" | "elicitation/create")
                {
                    session.map_or_else(
                        || Ok(Some(crate::interaction::cancelled(method))),
                        |session| session.interaction(id.clone(), method, params),
                    )
                } else {
                    Err(RemoteError {
                        code: -32601,
                        message: "Method not supported".into(),
                        data: None,
                    })
                };
                if let Ok(None) = &reply {
                    continue;
                }
                let _ = connection
                    .respond(id, reply.map(|result| result.unwrap_or_default()))
                    .await;
            } else if method == "session/update" {
                connection.update(params);
            }
        } else if let Some(id) = message["id"].as_u64() {
            let complete = connection.dispatch.lock().unwrap().pending.remove(&id);
            if let Some(complete) = complete {
                complete(match (message.get("result"), message.get("error")) {
                    (Some(result), None) => Ok(result.clone()),
                    (None, Some(error)) => Err(serde_json::from_value(error.clone())
                        .map_or(Error::InvalidResponse, Error::Remote)),
                    _ => Err(Error::InvalidResponse),
                });
            }
        }
    }
    if let Some(connection) = connection.upgrade() {
        connection.close(Some(
            failure.unwrap_or_else(|| "ACP agent closed its output".into()),
        ));
    }
}

fn observed_auth(state: AuthState, devin: bool, method: &str, reply: &Reply) -> AuthState {
    match reply {
        Err(Error::Remote(error)) if error.code == -32000 => AuthState::Required,
        Ok(_)
            if method == "authenticate"
                || (!devin && matches!(method, "session/new" | "session/prompt"))
                || (!devin && method == "session/list" && state == AuthState::Unknown) =>
        {
            AuthState::Ok
        }
        _ => state,
    }
}

pub(crate) fn agent_error(error: Error) -> AgentError {
    let code = match &error {
        Error::Closed => ErrorCode::AgentUnavailable,
        Error::Remote(error) => match error.code {
            -32000 => ErrorCode::AuthRequired,
            -32002 => ErrorCode::NotFound,
            -32602 => ErrorCode::BadRequest,
            -32601 => ErrorCode::NotResumable,
            _ => ErrorCode::Internal,
        },
        _ => ErrorCode::Internal,
    };
    AgentError::new(code, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_only_confirms_unknown_generic_auth() {
        let success = Ok(json!({}));
        let required = Err(Error::Remote(RemoteError {
            code: -32000,
            message: "auth_required".into(),
            data: None,
        }));
        let state = observed_auth(AuthState::Unknown, false, "session/list", &success);
        assert_eq!(state, AuthState::Ok);
        for method in [
            "session/list",
            "session/new",
            "session/prompt",
            "session/load",
        ] {
            assert_eq!(
                observed_auth(state, false, method, &required),
                AuthState::Required
            );
        }
        assert_eq!(
            observed_auth(AuthState::Required, false, "session/list", &success),
            AuthState::Required
        );
        assert_eq!(
            observed_auth(AuthState::Unknown, true, "session/list", &success),
            AuthState::Unknown
        );
    }
}
