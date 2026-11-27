use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{File, OpenOptions},
    io::{self, Write},
    mem::size_of,
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use verre_proto::{RawDirection, RawEntry};

struct LogState {
    entries: VecDeque<RawEntry>,
    bytes: usize,
    file: Option<File>,
    file_error: Option<String>,
}

pub struct WireLog {
    max_bytes: usize,
    state: Mutex<LogState>,
    filter: fn(&str) -> bool,
    prefix: Vec<RawEntry>,
    routing: Option<Mutex<Routing>>,
}

#[derive(Default)]
struct Routing {
    prefix: Vec<RawEntry>,
    sessions: HashMap<String, Weak<WireLog>>,
    unclaimed: HashMap<String, Arc<WireLog>>,
    early: VecDeque<(String, RawEntry)>,
    sent: HashMap<String, Route>,
    received: HashMap<String, Route>,
    private_config_reads: HashSet<u64>,
}

enum Route {
    Handshake,
    Create(RawEntry),
    Session(String),
}

impl Default for WireLog {
    fn default() -> Self {
        Self::new(4 * 1024 * 1024)
    }
}

impl WireLog {
    pub fn new(max_bytes: usize) -> Self {
        Self::build(max_bytes, None)
    }

    pub fn with_file(max_bytes: usize, path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self::build(max_bytes, Some(file)))
    }

    fn build(max_bytes: usize, file: Option<File>) -> Self {
        Self {
            max_bytes,
            filter: |_| true,
            prefix: Vec::new(),
            routing: None,
            state: Mutex::new(LogState {
                entries: VecDeque::new(),
                bytes: 0,
                file,
                file_error: None,
            }),
        }
    }

    pub fn with_filter(mut self, filter: fn(&str) -> bool) -> Self {
        self.filter = filter;
        self
    }

    pub fn multiplexed(mut self) -> Self {
        self.routing = Some(Mutex::new(Routing::default()));
        self
    }

    pub fn session(&self, id: &str) -> Arc<Self> {
        let routing = self.routing.as_ref().expect("multiplexed wire log");
        let mut routing = routing.lock().unwrap();
        routing.sessions.retain(|_, log| log.strong_count() > 0);
        if let Some(log) = routing.sessions.get(id).and_then(Weak::upgrade) {
            return log;
        }
        let log = routing.unclaimed.remove(id).unwrap_or_else(|| {
            Arc::new(Self {
                prefix: routing.prefix.clone(),
                ..Self::new(self.max_bytes)
            })
        });
        routing.sessions.insert(id.to_owned(), Arc::downgrade(&log));
        log
    }

    pub fn snapshot(&self) -> Vec<RawEntry> {
        let state = self.state.lock().unwrap();
        self.prefix.iter().chain(&state.entries).cloned().collect()
    }

    pub fn file_error(&self) -> Option<String> {
        self.state.lock().unwrap().file_error.clone()
    }

    pub(crate) fn record(&self, direction: RawDirection, mut text: String) {
        if !(self.filter)(&text) {
            return;
        }
        text.shrink_to_fit();
        let mut entry = RawEntry {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
            direction,
            text,
        };
        if let Some(routing) = &self.routing {
            let mut routing = routing.lock().unwrap();
            routing.redact_config_reply(&mut entry);
            routing.record(&entry, self.max_bytes);
        }
        self.push(entry);
    }

    fn push(&self, entry: RawEntry) {
        let mut state = self.state.lock().unwrap();
        if let Some(file) = state.file.as_mut() {
            if let Err(error) = append(file, &entry) {
                state.file_error = Some(error.to_string());
                state.file = None;
            }
        }

        let bytes = entry_size(&entry);
        if bytes > self.max_bytes {
            return;
        }
        while state.bytes > self.max_bytes - bytes {
            let oldest = state.entries.pop_front().unwrap();
            state.bytes -= entry_size(&oldest);
        }
        state.bytes += bytes;
        state.entries.push_back(entry);
    }
}

impl Routing {
    fn redact_config_reply(&mut self, entry: &mut RawEntry) {
        let sent = match entry.direction {
            RawDirection::Sent if entry.text.contains("\"config/read\"") => true,
            RawDirection::Received if !self.private_config_reads.is_empty() => false,
            _ => return,
        };
        let Ok(message) = serde_json::from_str::<Value>(&entry.text) else {
            return;
        };
        let Some(id) = message["id"].as_u64() else {
            return;
        };
        if sent {
            if message["method"] == "config/read" {
                self.private_config_reads.insert(id);
            }
        } else if message.get("method").is_none() && self.private_config_reads.remove(&id) {
            entry.text = serde_json::json!({ "id": id, "redacted": true }).to_string();
        }
    }

    fn record(&mut self, entry: &RawEntry, max_bytes: usize) {
        let sent = match entry.direction {
            RawDirection::Spawn => {
                self.prefix.push(entry.clone());
                return;
            }
            RawDirection::Sent => true,
            RawDirection::Received => false,
            RawDirection::Stderr => return,
        };
        let Ok(message) = serde_json::from_str::<Value>(&entry.text) else {
            return;
        };
        let id = message.get("id").map(Value::to_string);
        if let Some(method) = message["method"].as_str() {
            let route = match method {
                "initialize" | "initialized" if sent => {
                    self.prefix.push(entry.clone());
                    Some(Route::Handshake)
                }
                "thread/start" | "session/new" if sent => Some(Route::Create(entry.clone())),
                "thread/read" | "thread/turns/list" | "thread/items/list" => None,
                _ => session_id(&message["params"]).and_then(|session| {
                    if self.push(session, entry) {
                        return Some(Route::Session(session.to_owned()));
                    }
                    if !sent
                        && self
                            .sent
                            .values()
                            .any(|route| matches!(route, Route::Create(_)))
                        && entry_size(entry) <= 16 * 1024
                    {
                        if self.early.len() == 64 {
                            self.early.pop_front();
                        }
                        self.early.push_back((session.to_owned(), entry.clone()));
                    }
                    None
                }),
            };
            if let (Some(id), Some(route)) = (id, route) {
                let pending = if sent {
                    &mut self.sent
                } else {
                    &mut self.received
                };
                if pending.len() < 512 {
                    pending.insert(id, route);
                }
            }
        } else if let Some(id) = id {
            let pending = if sent {
                &mut self.received
            } else {
                &mut self.sent
            };
            match pending.remove(&id) {
                Some(Route::Handshake) => self.prefix.push(entry.clone()),
                Some(Route::Session(session)) => {
                    self.push(&session, entry);
                }
                Some(Route::Create(request)) => {
                    if let Some(session) = session_id(&message["result"]) {
                        if self.unclaimed.len() >= 8 {
                            self.unclaimed.clear();
                        }
                        let log = Arc::new(WireLog {
                            prefix: self.prefix.clone(),
                            ..WireLog::new(max_bytes)
                        });
                        log.push(request);
                        self.early.retain(|(id, entry)| {
                            if id != session {
                                return true;
                            }
                            log.push(entry.clone());
                            false
                        });
                        log.push(entry.clone());
                        self.unclaimed.insert(session.to_owned(), log);
                    }
                    if !self
                        .sent
                        .values()
                        .any(|route| matches!(route, Route::Create(_)))
                    {
                        self.early.clear();
                    }
                }
                None => {}
            }
        }
    }

    fn push(&self, id: &str, entry: &RawEntry) -> bool {
        if let Some(log) = self.sessions.get(id).and_then(Weak::upgrade) {
            log.push(entry.clone());
        } else if let Some(log) = self.unclaimed.get(id) {
            log.push(entry.clone());
        } else {
            return false;
        }
        true
    }
}

fn session_id(value: &Value) -> Option<&str> {
    value["sessionId"]
        .as_str()
        .or_else(|| value["threadId"].as_str())
        .or_else(|| value["thread"]["id"].as_str())
}

fn entry_size(entry: &RawEntry) -> usize {
    size_of::<RawEntry>() + entry.text.capacity()
}

fn append(file: &mut File, entry: &RawEntry) -> io::Result<()> {
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    file.write_all(&line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scoped_prefix_survives_rollover_and_unrelated_calls() {
        let log = WireLog::new(512).multiplexed();
        log.record(RawDirection::Spawn, "spawn".into());
        log.record(
            RawDirection::Sent,
            json!({"id": 1, "method": "initialize"}).to_string(),
        );
        log.record(
            RawDirection::Received,
            json!({"id": 1, "result": {"protocolVersion": 1}}).to_string(),
        );
        let session = log.session("first");
        for index in 0..50 {
            log.record(
                RawDirection::Received,
                json!({
                    "method": "session/update", "params": {"sessionId": "first", "marker": index}
                })
                .to_string(),
            );
            log.record(
                RawDirection::Received,
                json!({
                    "method": "session/update", "params": {"sessionId": "second", "secret": index}
                })
                .to_string(),
            );
        }
        let entries = session.snapshot();
        assert_eq!(entries[0].direction, RawDirection::Spawn);
        assert!(entries[1].text.contains("initialize"));
        assert!(entries[2].text.contains("protocolVersion"));
        assert!(entries.last().unwrap().text.contains("\"marker\":49"));
        assert!(entries
            .iter()
            .all(|entry| !entry.text.contains("\"marker\":0") && !entry.text.contains("second")));
        assert!(log
            .snapshot()
            .iter()
            .all(|entry| entry.direction != RawDirection::Spawn));
    }

    #[test]
    fn new_session_captures_early_updates_only_for_its_id() {
        let log = WireLog::default().multiplexed();
        log.record(
            RawDirection::Sent,
            json!({"id": 1, "method": "session/new"}).to_string(),
        );
        for session_id in ["other", "created"] {
            log.record(
                RawDirection::Received,
                json!({
                    "method": "session/update", "params": {"sessionId": session_id}
                })
                .to_string(),
            );
        }
        log.record(
            RawDirection::Received,
            json!({"id": 1, "result": {"sessionId": "created"}}).to_string(),
        );
        let entries = log.session("created").snapshot();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].text.contains("session/new"));
        assert!(entries[1].text.contains("session/update") && entries[1].text.contains("created"));
        assert!(entries[2].text.contains("result"));
    }

    #[test]
    fn config_replies_are_redacted_from_the_process_log() {
        let log = WireLog::default().multiplexed();
        log.record(
            RawDirection::Sent,
            r#"{"id":5,"method":"config/read","params":{}}"#.into(),
        );
        log.record(
            RawDirection::Received,
            r#"{"id":6,"result":{"version":"safe"}}"#.into(),
        );
        log.record(
            RawDirection::Received,
            r#"{"id":5,"result":{"config":{"base_url":"https://user:secret@example.com"}}}"#.into(),
        );
        let entries = log.snapshot();
        assert!(entries[1].text.contains("safe"));
        assert_eq!(entries[2].text, r#"{"id":5,"redacted":true}"#);
    }
}
