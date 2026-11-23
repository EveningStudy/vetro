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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("JSON-RPC error {code}: {message}")]
pub struct RemoteError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
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
}

pub type IncomingMessages = mpsc::UnboundedReceiver<Incoming>;
type PendingMap = Mutex<Option<HashMap<u64, oneshot::Sender<Result<Value, Error>>>>>;

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
        let (incoming, messages) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(Some(HashMap::new())));
        tokio::spawn(read_messages(stdout, Arc::clone(&pending), incoming));
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
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (send, response) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .as_mut()
            .ok_or(Error::Closed)?
            .insert(id, send);
        let pending = PendingRequest(&self.pending, id);
        self.write(call(Some(id), method, params)).await?;
        let result = response.await.map_err(|_| Error::Closed)?;
        drop(pending);
        result
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
            let sender = pending
                .lock()
                .unwrap()
                .as_mut()
                .and_then(|map| map.remove(&id));
            if let Some(sender) = sender {
                let result = match (message.get("result"), message.get("error")) {
                    (Some(result), None) => Ok(result.clone()),
                    (None, Some(error)) => {
                        Err(serde_json::from_value::<RemoteError>(error.clone())
                            .map_or(Error::InvalidResponse, Error::Remote))
                    }
                    _ => Err(Error::InvalidResponse),
                };
                let _ = sender.send(result);
            }
        }
    }
    pending.lock().unwrap().take();
}
