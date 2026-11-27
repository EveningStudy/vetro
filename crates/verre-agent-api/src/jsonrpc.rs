use std::{
    collections::HashMap,
    io,
    process::ExitStatus,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Number, Value};
use tokio::sync::{mpsc, oneshot};

use crate::{
    process::{Process, Stdout},
    WireLog,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V2,
    Bare,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(Number),
    String(String),
    Null,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)?;
        if let Some(detail) = self.data.as_ref().and_then(format_error_data) {
            if !self.message.contains(&detail) {
                write!(f, " ({detail})")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for RemoteError {}

fn format_error_data(value: &Value) -> Option<String> {
    let raw = match value {
        Value::Null => return None,
        Value::String(text) => text.trim().to_owned(),
        other => other.to_string(),
    };
    let clean: String = raw
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return None;
    }
    const MAX: usize = 200;
    if clean.chars().count() > MAX {
        let truncated: String = clean.chars().take(MAX).collect();
        Some(format!("{truncated}…"))
    } else {
        Some(clean)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error("JSON-RPC peer closed")]
    Closed,
    #[error("invalid JSON-RPC response")]
    InvalidResponse,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Notification {
        method: String,
        params: Option<Value>,
    },
    Request {
        id: Id,
        method: String,
        params: Option<Value>,
    },
    Response {
        id: u64,
        envelope: Value,
    },
}

pub type IncomingMessages = mpsc::UnboundedReceiver<Incoming>;
type Reply = Result<Value, Error>;
type Completion = Box<dyn FnOnce(Reply) + Send>;
type PendingMap = Mutex<Option<HashMap<u64, Completion>>>;

pub struct Peer {
    process: Process,
    version: Version,
    next_id: AtomicU64,
    pending: Arc<PendingMap>,
}

impl Peer {
    /// Start draining stdout. Consume the incoming stream in parallel with requests.
    /// Incoming envelopes may use either version. The adapter handles unknown methods.
    pub fn new(process: Process, stdout: Stdout, version: Version) -> (Self, IncomingMessages) {
        Self::spawn(process, stdout, version, false)
    }

    pub fn new_ordered(
        process: Process,
        stdout: Stdout,
        version: Version,
    ) -> (Self, IncomingMessages) {
        Self::spawn(process, stdout, version, true)
    }

    fn spawn(
        process: Process,
        stdout: Stdout,
        version: Version,
        ordered: bool,
    ) -> (Self, IncomingMessages) {
        let (incoming, messages) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(Some(HashMap::new())));
        tokio::spawn(read_messages(
            stdout,
            Arc::clone(&pending),
            incoming,
            ordered,
        ));
        (
            Self {
                process,
                version,
                next_id: AtomicU64::new(1),
                pending,
            },
            messages,
        )
    }

    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, Error> {
        self.request_with(method, params, |_| {}).await
    }

    pub async fn request_with(
        &self,
        method: &str,
        params: Option<Value>,
        received: impl FnOnce(&Reply) + Send + 'static,
    ) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (send, response) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .as_mut()
            .ok_or(Error::Closed)?
            .insert(
                id,
                Box::new(move |reply| {
                    received(&reply);
                    let _ = send.send(reply);
                }),
            );
        let pending = PendingRequest(&self.pending, id);
        self.write(call(Some(id), method, params)).await?;
        let result = response.await.map_err(|_| Error::Closed)?;
        drop(pending);
        result
    }

    pub fn complete(&self, id: u64, envelope: Value) {
        complete(&self.pending, id, envelope);
    }

    pub fn finish_incoming(&self) {
        self.pending.lock().unwrap().take();
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), Error> {
        self.write(call(None, method, params)).await
    }

    pub async fn respond(&self, id: Id, result: Result<Value, RemoteError>) -> Result<(), Error> {
        let envelope = match result {
            Ok(result) => json!({ "id": id, "result": result }),
            Err(error) => json!({ "id": id, "error": error }),
        };
        self.write(envelope).await
    }

    pub fn wire_log(&self) -> Arc<WireLog> {
        self.process.wire_log()
    }

    pub fn begin_user_work(&self) {
        self.process.begin_user_work();
    }

    pub fn background_processes(&self) -> usize {
        self.process.background_processes()
    }

    pub async fn exit_detail(&self) -> String {
        self.process.exit_detail().await
    }

    pub async fn wait(&self) -> io::Result<ExitStatus> {
        self.process.wait().await
    }

    pub async fn stop(&self) -> io::Result<ExitStatus> {
        self.process.stop().await
    }

    pub async fn kill(&self) -> io::Result<ExitStatus> {
        self.process.kill().await
    }

    async fn write(&self, mut envelope: Value) -> Result<(), Error> {
        if self.version == Version::V2 {
            envelope["jsonrpc"] = json!("2.0");
        }
        self.process.send_line(envelope.to_string()).await?;
        Ok(())
    }
}

struct PendingRequest<'a>(&'a PendingMap, u64);

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        if let Some(pending) = self.0.lock().unwrap().as_mut() {
            pending.remove(&self.1);
        }
    }
}

fn call(id: Option<u64>, method: &str, params: Option<Value>) -> Value {
    let mut envelope = json!({ "method": method });
    if let Some(id) = id {
        envelope["id"] = json!(id);
    }
    if let Some(params) = params {
        envelope["params"] = params;
    }
    envelope
}

async fn read_messages(
    mut stdout: Stdout,
    pending: Arc<PendingMap>,
    incoming: mpsc::UnboundedSender<Incoming>,
    ordered: bool,
) {
    while let Some(Ok(line)) = stdout.recv().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let method = method.to_owned();
            let params = message.get("params").cloned();
            let event = match message.get("id").cloned() {
                Some(id) => match serde_json::from_value(id) {
                    Ok(id) => Incoming::Request { id, method, params },
                    Err(_) => continue,
                },
                None => Incoming::Notification { method, params },
            };
            let _ = incoming.send(event);
        } else if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if ordered {
                if incoming
                    .send(Incoming::Response {
                        id,
                        envelope: message,
                    })
                    .is_err()
                {
                    break;
                }
            } else {
                complete(&pending, id, message);
            }
        }
    }
    if !ordered {
        pending.lock().unwrap().take();
    }
}

fn complete(pending: &PendingMap, id: u64, envelope: Value) {
    let sender = pending
        .lock()
        .unwrap()
        .as_mut()
        .and_then(|map| map.remove(&id));
    if let Some(sender) = sender {
        let result = match (envelope.get("result"), envelope.get("error")) {
            (Some(result), None) => Ok(result.clone()),
            (None, Some(error)) => Err(serde_json::from_value::<RemoteError>(error.clone())
                .map_or(Error::InvalidResponse, Error::Remote)),
            _ => Err(Error::InvalidResponse),
        };
        sender(result);
    }
}

#[cfg(test)]
#[test]
fn remote_error_display_includes_sanitized_data() {
    let error = RemoteError {
        code: -32602,
        message: "Invalid params".into(),
        data: Some(json!(
            "Invalid value 'max' for config option 'thought_level'"
        )),
    };
    assert_eq!(
        error.to_string(),
        "JSON-RPC error -32602: Invalid params (Invalid value 'max' for config option 'thought_level')"
    );
    let plain = RemoteError {
        code: -32601,
        message: "Method not found".into(),
        data: None,
    };
    assert_eq!(plain.to_string(), "JSON-RPC error -32601: Method not found");
}
