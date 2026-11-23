mod fixtures;

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::Write,
    net::SocketAddr,
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use fixtures::Fake;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    MaybeTlsStream, WebSocketStream,
};
use vetro_agent_api::Agent;
use vetro_core::{home::Home, registry::Registry, server::Server, Core};

fn main() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    std::env::set_var("HOME", path);
    std::env::set_var("USERPROFILE", path);
    std::env::remove_var("VETRO_HOME");
    std::env::remove_var("VETRO_WEB_DIR");
    let cwd = path.join("workspace");
    fs::create_dir_all(&cwd).unwrap();
    fs::write(cwd.join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(cwd.join("file.rs"), "sentinel").unwrap();
    fs::write(cwd.join("ignored.rs"), "sentinel").unwrap();
    for agent_dir in [".claude", ".codex", ".gemini", ".agents", ".devin"] {
        fs::create_dir(path.join(agent_dir)).unwrap();
        fs::write(path.join(agent_dir).join("sentinel"), agent_dir).unwrap();
    }
    fs::write(path.join(".claude/fake-transcript"), "\n\n").unwrap();
    let before = tree(path);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(25), exercise(path, &cwd))
                .await
                .unwrap();
        });
    assert_eq!(before, tree(path));
    assert!(path.join(".vetro/config.json").exists());
    assert!(path.join(".vetro/state.json").exists());
    assert!(path.join(".vetro/raw").is_dir());
    assert!(path.join(".vetro/index.db").is_file());
}

async fn exercise(path: &Path, cwd: &Path) {
    let fake = Arc::new(Fake {
        cwd: cwd.to_owned(),
        prompts: Default::default(),
        attaches: Default::default(),
        detaches: Default::default(),
        shutdowns: Default::default(),
        authenticated: Default::default(),
        history_path: path.join(".claude/fake-transcript"),
        reads: Default::default(),
    });
    let home = Home::discover().unwrap();
    assert_eq!(home.root(), path.join(".vetro"));
    let local_token = home.local_token();
    let registry = || {
        Registry::new([fake.clone() as Arc<dyn Agent>])
            .unwrap()
            .watch_history("fake", vec![path.join(".claude")])
    };
    let core = Core::new(home, registry());
    core.refresh_history().await;
    assert_eq!(fake.reads.load(Ordering::Relaxed), 2);
    core.refresh_history().await;
    assert_eq!(fake.reads.load(Ordering::Relaxed), 2);
    drop(core);
    let index = path.join(".vetro/index.db");
    fs::remove_file(&index).unwrap();
    let core = Core::new(Home::discover().unwrap(), registry());
    core.refresh_history().await;
    assert_eq!(fake.reads.load(Ordering::Relaxed), 4);
    drop(core);
    fs::write(&index, b"not a SQLite database").unwrap();
    let core = Core::new(Home::discover().unwrap(), registry());
    core.refresh_history().await;
    assert_eq!(fake.reads.load(Ordering::Relaxed), 6);
    drop(core);
    let core = Core::new(Home::discover().unwrap(), registry());
    assert_eq!(core.threads(Default::default()).unwrap().threads.len(), 2);
    core.refresh_history().await;
    assert_eq!(fake.reads.load(Ordering::Relaxed), 6);
    let server = Server::bind(core, Some(0)).await.unwrap();
    let addr = server.address();
    assert_eq!(addr.ip().to_string(), "127.0.0.1");
    assert_eq!(
        server.local_url(),
        format!("http://{addr}/#token={local_token}")
    );
    let origin = format!("http://{addr}");
    let (stop, shutdown) = oneshot::channel();
    let serving = tokio::spawn(server.serve(async {
        let _ = shutdown.await;
    }));

    let health = http(addr, "GET", "/api/health", &[], None).await;
    assert_eq!(health.status, 200);
    assert_eq!(
        http(
            addr,
            "GET",
            "/api/health",
            &[("Host", "untrusted.test")],
            None
        )
        .await
        .status,
        403
    );
    assert_eq!(
        http(addr, "GET", "/some/ui/route", &[], None).await.status,
        200
    );
    assert_eq!(
        http(addr, "GET", "/api/auth/session", &[], None)
            .await
            .status,
        401
    );
    assert_eq!(socket_error(addr, None, &origin).await, 401);
    assert_eq!(
        http(
            addr,
            "POST",
            "/api/auth/local",
            &[],
            Some(json!({"token": "wrong"}))
        )
        .await
        .status,
        401
    );
    let local = http(
        addr,
        "POST",
        "/api/auth/local",
        &[("Origin", &origin)],
        Some(json!({"token": local_token})),
    )
    .await;
    assert_eq!(local.status, 200);
    let cookie = local.cookie.unwrap();
    assert_eq!(
        socket_error(addr, Some(&cookie), "http://untrusted.test").await,
        403
    );
    assert_eq!(
        http(
            addr,
            "GET",
            "/api/auth/session",
            &[("Cookie", &cookie)],
            None
        )
        .await
        .body["remoteClient"],
        false
    );

    let mut first = Client::connect(addr, &cookie, &origin).await;
    let mut second = Client::connect(addr, &cookie, &origin).await;
    assert_eq!(first.call("app.hello", hello()).await["protocolVersion"], 1);
    second.call("app.hello", hello()).await;
    let agents = first.call("agents.list", json!({})).await;
    assert_eq!(agents["agents"][0]["id"], "fake");
    let options = first
        .call("agents.options", json!({"agentId": "fake", "cwd": cwd}))
        .await;
    assert_eq!(options["options"][0]["currentValue"], "native");
    let pairing = first.call("remote.createPairing", json!({})).await;
    let pair = json!({"code": pairing["code"], "deviceName": "Test device"});
    let paired = http(
        addr,
        "POST",
        "/api/auth/pair",
        &[("Origin", &origin)],
        Some(pair.clone()),
    )
    .await;
    assert_eq!(paired.status, 200);
    let paired_cookie = paired.cookie.unwrap();
    assert_eq!(
        http(addr, "POST", "/api/auth/pair", &[], Some(pair))
            .await
            .status,
        401
    );
    let mut remote = Client::connect(addr, &paired_cookie, &origin).await;
    assert_eq!(
        remote.call("app.hello", hello()).await["remoteClient"],
        true
    );
    let devices = remote.call("remote.status", json!({})).await;
    assert_eq!(devices["devices"][0]["current"], true);
    first
        .call(
            "remote.revokeDevice",
            json!({"deviceId": devices["devices"][0]["id"]}),
        )
        .await;
    assert_eq!(socket_error(addr, Some(&paired_cookie), &origin).await, 401);
    drop(remote);

    let content = json!([{"type": "text", "text": "  Exact \r\nテキスト \"\u{1f30d}\"\n"}, {"type": "file", "path": "file.rs", "name": null}]);
    let start =
        json!({"agentId": "fake", "cwd": cwd, "content": content, "options": {"model": "chosen"}});
    assert_eq!(
        first.request("threads.start", start.clone()).await["error"]["code"],
        "auth_required"
    );
    first.until_agent_auth("required").await;
    first
        .call(
            "agents.authenticate",
            json!({"agentId": "fake", "methodId": "fake-login"}),
        )
        .await;
    first.until_agent_auth("ok").await;
    let started = first.call("threads.start", start).await;
    let id = started["thread"]["id"].as_str().unwrap();
    assert_eq!(id, "fake:live");
    let mut seq = 0;
    let mut delta = String::new();
    loop {
        let (next_seq, event) = first.thread_event(id).await;
        assert_eq!(next_seq, seq + 1);
        seq = next_seq;
        if event["type"] == "text_delta" {
            delta.push_str(event["delta"].as_str().unwrap());
        }
        if event["thread"]["status"] == "awaiting_input" {
            break;
        }
    }
    assert_eq!(delta, "Fake stream");
    assert_eq!(
        serde_json::to_value(&fake.prompts.lock().unwrap()[0]).unwrap(),
        content
    );
    let opened = second.call("threads.open", json!({"threadId": id})).await;
    assert_eq!(opened["seq"], seq);
    assert_eq!(opened["thread"]["title"], "Agent title");
    assert_eq!(opened["thread"]["model"], "chosen");
    assert_eq!(opened["items"][0]["content"], content);
    assert_eq!(opened["items"][1]["text"], "Fake stream");
    assert_eq!(opened["items"][2]["resolution"], Value::Null);
    assert_eq!(
        first
            .request("threads.detach", json!({"threadId": id}))
            .await["error"]["code"],
        "busy"
    );
    assert_eq!(fake.detaches.load(Ordering::Relaxed), 0);
    first
        .call(
            "threads.send",
            json!({"threadId": id, "content": [{"type": "text", "text": "Queued"}]}),
        )
        .await;
    assert_eq!(fake.prompts.lock().unwrap().len(), 1);
    second.call("threads.respond", json!({"threadId": id, "requestId": "permission", "resolution": {"type": "selected", "optionId": "allow"}})).await;
    let mut resolved = false;
    loop {
        let (next_seq, event) = first.thread_event(id).await;
        assert_eq!(next_seq, seq + 1);
        seq = next_seq;
        resolved |= event["item"]["resolution"]["optionId"] == "allow";
        if event["item"]["content"][0]["text"] == "Queued" {
            break;
        }
    }
    assert!(resolved);
    second
        .call("threads.interrupt", json!({"threadId": id}))
        .await;
    loop {
        let (_, event) = first.thread_event(id).await;
        if event["item"]["stopReason"] == "cancelled" {
            break;
        }
    }
    first
        .call(
            "threads.setOption",
            json!({"threadId": id, "optionId": "model", "value": "updated"}),
        )
        .await;
    let original = second.call("threads.open", json!({"threadId": id})).await;
    drop(second);
    let mut second = Client::connect(addr, &cookie, &origin).await;
    second.call("app.hello", hello()).await;
    let reopened = second.call("threads.open", json!({"threadId": id})).await;
    assert_eq!(original, reopened);
    assert_eq!(reopened["thread"]["model"], "updated");
    first.call("threads.close", json!({"threadId": id})).await;
    first.events.clear();
    first.call("threads.detach", json!({"threadId": id})).await;
    let (_, detached) = second
        .until_event(id, |event| event["thread"]["live"] == false)
        .await;
    assert_eq!(detached["thread"]["live"], false);
    tokio::time::timeout(Duration::from_secs(8), async {
        while fake.shutdowns.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    first
        .call(
            "threads.send",
            json!({"threadId": id, "content": [{"type": "text", "text": "Resumed"}]}),
        )
        .await;
    second
        .until_event(id, |event| event["item"]["content"][0]["text"] == "Resumed")
        .await;
    first.call("agents.list", json!({})).await;
    assert!(first
        .events
        .iter()
        .all(|event| event["event"] != "thread.event"));
    assert_eq!(fake.attaches.load(Ordering::Relaxed), 1);
    assert_eq!(fake.detaches.load(Ordering::Relaxed), 1);
    fake.authenticated.store(false, Ordering::Relaxed);
    let send = json!({"threadId": id, "content": [{"type": "text", "text": "Retry after login"}]});
    first
        .call("threads.interrupt", json!({"threadId": id}))
        .await;
    second
        .until_event(id, |event| event["thread"]["status"] == "idle")
        .await;
    assert_eq!(
        first.request("threads.send", send.clone()).await["error"]["code"],
        "auth_required"
    );
    first.until_agent_auth("required").await;
    first
        .call(
            "agents.authenticate",
            json!({"agentId": "fake", "methodId": "fake-login"}),
        )
        .await;
    first.until_agent_auth("ok").await;
    first.call("threads.send", send).await;
    assert_eq!(
        first.call("threads.raw", json!({"threadId": id})).await["entries"],
        json!([])
    );

    let page = first
        .call("threads.list", json!({"sources": ["cli"], "limit": 1}))
        .await;
    assert_eq!(page["threads"][0]["id"], "fake:history");
    let next = first
        .call(
            "threads.list",
            json!({"sources": ["cli"], "limit": 1, "cursor": page["nextCursor"]}),
        )
        .await;
    assert_eq!(next["threads"][0]["id"], "fake:readonly");
    assert_eq!(
        first
            .request(
                "threads.send",
                json!({"threadId": "fake:readonly", "content": []})
            )
            .await["error"]["code"],
        "not_resumable"
    );
    let history = first
        .call(
            "threads.open",
            json!({"threadId": "fake:history", "limit": 2}),
        )
        .await;
    assert_eq!(history["hasMore"], true);
    assert_eq!(history["items"][0]["ordinal"], 4);
    let older = first
        .call(
            "threads.items",
            json!({"threadId": "fake:history", "before": 4, "limit": 4}),
        )
        .await;
    assert_eq!(older["hasMore"], false);
    assert_eq!(older["items"].as_array().unwrap().len(), 3);
    let reads = fake.reads.load(Ordering::Relaxed);
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(&fake.history_path)
            .unwrap(),
        "External writer says 中文全文检索 <unsafe>."
    )
    .unwrap();
    let (seq, event) = tokio::time::timeout(
        Duration::from_secs(5),
        first.until_event("fake:history", |event| {
            event["type"] == "item_added" && event["item"]["id"] == "external-0"
        }),
    )
    .await
    .unwrap();
    assert_eq!(seq, history["seq"].as_u64().unwrap() + 1);
    assert_eq!(event["item"]["ordinal"], 6);
    assert_eq!(fake.reads.load(Ordering::Relaxed), reads + 1);
    for (query, marked) in [
        ("external writer", "<mark>External writer</mark>"),
        ("全文检索", "<mark>全文检索</mark>"),
        ("中", "<mark>中</mark>"),
    ] {
        let hits = first.call("search.threads", json!({"query": query})).await;
        assert_eq!(hits["hits"][0]["thread"]["id"], "fake:history");
        let snippet = hits["hits"][0]["snippet"].as_str().unwrap();
        assert!(snippet.contains(marked), "{snippet}");
        assert!(snippet.contains("&lt;unsafe&gt;"));
    }
    let rename = first
        .call(
            "threads.update",
            json!({"threadId": "fake:history", "title": "Renamed"}),
        )
        .await;
    assert_eq!(rename["thread"]["title"], "Renamed");
    let hits = first
        .call("search.threads", json!({"query": "Renamed"}))
        .await;
    assert_eq!(hits["hits"][0]["snippet"], "<mark>Renamed</mark>");
    first
        .call(
            "threads.update",
            json!({"threadId": "fake:history", "archived": true}),
        )
        .await;
    let list = first
        .call("threads.list", json!({"sources": ["cli"]}))
        .await;
    assert_eq!(list["threads"].as_array().unwrap().len(), 1);
    let archived = first
        .call(
            "threads.list",
            json!({"sources": ["cli"], "includeArchived": true}),
        )
        .await;
    assert_eq!(archived["threads"].as_array().unwrap().len(), 2);
    let projects = first.call("projects.list", json!({})).await;
    assert_eq!(projects["projects"][0]["threadCount"], 3);
    first.call("projects.add", json!({"path": cwd})).await;
    let project = first
        .call(
            "projects.update",
            json!({"path": cwd, "name": "Pinned", "pinned": true, "hidden": true}),
        )
        .await;
    assert_eq!(project["name"], "Pinned");
    assert_eq!(project["hidden"], true);
    let projects = first.call("projects.list", json!({})).await;
    assert_eq!(projects["projects"][0]["hidden"], true);
    first.call("threads.open", json!({"threadId": id})).await;
    let child_store = fake.history_path.with_file_name("fake-child");
    fs::write(&child_store, "Child started\n").unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        first.until_event(id, |event| {
            event["type"] == "item_updated"
                && event["item"]["id"] == "child-tool"
                && event["item"]["childThreadIds"] == json!(["fake:external-child"])
        }),
    )
    .await
    .unwrap();
    let children = first.call("threads.list", json!({"parentId": id})).await;
    assert_eq!(children["threads"][0]["id"], "fake:external-child");
    assert_eq!(children["threads"][0]["parent"]["confidence"], "exact");
    assert_eq!(children["threads"][0]["parent"]["callId"], "spawn");
    assert_eq!(
        first.call("threads.open", json!({"threadId": id})).await["thread"]["childCount"],
        1
    );
    fs::remove_file(child_store).unwrap();
    fs::write(&fake.history_path, "\n\n").unwrap();
    let files = first.call("fs.list", json!({"path": cwd})).await;
    assert_eq!(files["entries"].as_array().unwrap().len(), 3);
    assert_eq!(
        first
            .call("fs.find", json!({"cwd": cwd, "query": ".rs"}))
            .await["paths"],
        json!(["file.rs"])
    );
    let settings = first
        .call(
            "settings.update",
            json!({"patch": {"idleReleaseMinutes": 0}}),
        )
        .await;
    assert_eq!(settings["idleReleaseMinutes"], 0);

    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
    let home = Home::discover().unwrap();
    assert_eq!(home.settings().idle_release_minutes, 0);
    assert_eq!(home.local_token(), local_token);
}

fn hello() -> Value {
    json!({"clientName": "e2e", "protocolVersion": 1, "futureField": true})
}

struct Client {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    events: VecDeque<Value>,
}

impl Client {
    async fn connect(addr: SocketAddr, cookie: &str, origin: &str) -> Self {
        let (socket, _) = connect_async(ws_request(addr, Some(cookie), origin))
            .await
            .unwrap();
        Self {
            socket,
            next_id: 0,
            events: VecDeque::new(),
        }
    }

    async fn recv(&mut self) -> Value {
        let frame = self.socket.next().await.unwrap().unwrap();
        serde_json::from_str(frame.to_text().unwrap()).unwrap()
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.socket
            .send(Message::Text(
                json!({"id": id, "method": method, "params": params})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let frame = self.recv().await;
            if frame["id"] == id {
                return frame;
            }
            assert!(frame["event"].is_string(), "{frame}");
            self.events.push_back(frame);
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params).await;
        assert!(response["error"].is_null(), "{method}: {response}");
        response["result"].clone()
    }

    async fn thread_event(&mut self, id: &str) -> (u64, Value) {
        self.until_event(id, |_| true).await
    }

    async fn until_agent_auth(&mut self, state: &str) {
        loop {
            let frame = match self.events.pop_front() {
                Some(frame) => frame,
                None => self.recv().await,
            };
            if frame["event"] == "agents.updated"
                && frame["data"]["agents"][0]["auth"]["state"] == state
            {
                return;
            }
        }
    }

    async fn until_event(&mut self, id: &str, predicate: impl Fn(&Value) -> bool) -> (u64, Value) {
        loop {
            let frame = match self.events.pop_front() {
                Some(frame) => frame,
                None => self.recv().await,
            };
            if frame["event"] == "thread.event"
                && frame["data"]["threadId"] == id
                && predicate(&frame["data"]["event"])
            {
                return (
                    frame["data"]["seq"].as_u64().unwrap(),
                    frame["data"]["event"].clone(),
                );
            }
        }
    }
}

fn ws_request(
    addr: SocketAddr,
    cookie: Option<&str>,
    origin: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = format!("ws://{addr}/api/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_str(origin).unwrap());
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert("cookie", HeaderValue::from_str(cookie).unwrap());
    }
    request
}

async fn socket_error(addr: SocketAddr, cookie: Option<&str>, origin: &str) -> u16 {
    match connect_async(ws_request(addr, cookie, origin))
        .await
        .unwrap_err()
    {
        tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
        error => panic!("Unexpected WebSocket error: {error}"),
    }
}

struct HttpReply {
    status: u16,
    cookie: Option<String>,
    body: Value,
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> HttpReply {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let mut request = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    if !headers
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("host"))
    {
        request.push_str(&format!("Host: {addr}\r\n"));
    }
    for (key, value) in headers {
        request.push_str(&format!("{key}: {value}\r\n"));
    }
    request.push_str(&format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut data = Vec::new();
    socket.read_to_end(&mut data).await.unwrap();
    let data = String::from_utf8(data).unwrap();
    let (headers, body) = data.split_once("\r\n\r\n").unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    let cookie = headers
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .and_then(|line| line.split(';').next())
        .map(str::to_owned);
    HttpReply {
        status,
        cookie,
        body: serde_json::from_str(body).unwrap_or(Value::Null),
    }
}

fn tree(path: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, output: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap();
            if relative.starts_with(".vetro") {
                continue;
            }
            let directory = path.is_dir();
            output.insert(
                relative.to_string_lossy().into_owned(),
                (!directory).then(|| fs::read(&path).unwrap()),
            );
            if directory {
                walk(root, &path, output);
            }
        }
    }
    let mut output = BTreeMap::new();
    walk(path, path, &mut output);
    output
}
