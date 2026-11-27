#[cfg(feature = "claude")]
#[path = "fixtures/claude.rs"]
mod claude;
#[cfg(feature = "codex")]
#[path = "fixtures/codex.rs"]
mod codex;
mod fixtures;

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{BufRead, Write},
    net::SocketAddr,
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::{Duration, SystemTime, UNIX_EPOCH},
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
use verre_agent_api::Agent;
use verre_core::{home::Home, registry::Registry, server::Server, Core};

const REMOTE_HOST: &str = "verre.example.test:8443";
const REMOTE_ORIGIN: &str = "https://verre.example.test:8443";

fn main() {
    #[cfg(feature = "codex")]
    if std::env::args().nth(1).as_deref() == Some("app-server") {
        codex::fake();
        return;
    }
    #[cfg(feature = "claude")]
    if std::env::args().nth(1).as_deref() == Some("-p") {
        claude::fake();
        return;
    }
    if let Some(port) = std::env::var_os("VERRE_TEST_TAILSCALE_PORT") {
        fake_tailscale(port.to_str().unwrap());
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    // Temp paths can be spelled through a symlink (macOS) or an 8.3 short name (Windows).
    let canonical = temp.path().canonicalize().unwrap();
    let resolved = match canonical
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
    {
        Some(plain) => std::path::PathBuf::from(plain),
        None => canonical,
    };
    let path = resolved.as_path();
    std::env::set_var("HOME", path);
    std::env::set_var("USERPROFILE", path);
    std::env::remove_var("VERRE_HOME");
    std::env::remove_var("VERRE_WEB_DIR");
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
    let fake_bin = path.join("bin");
    fs::create_dir(&fake_bin).unwrap();
    let exe = if cfg!(windows) {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    fs::copy(std::env::current_exe().unwrap(), fake_bin.join(exe)).unwrap();
    let marker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let marker_port = marker.local_addr().unwrap().port();
    drop(marker);
    let before = tree(path);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            if std::env::args().nth(1).as_deref() == Some("--browser-fixture") {
                browser_fixture(path, &cwd).await;
                return;
            }
            tokio::time::timeout(
                Duration::from_secs(30),
                exercise(path, &cwd, &fake_bin, marker_port),
            )
            .await
            .unwrap();
            #[cfg(feature = "claude")]
            tokio::time::timeout(Duration::from_secs(30), claude::exercise())
                .await
                .unwrap();
            #[cfg(feature = "codex")]
            tokio::time::timeout(Duration::from_secs(30), codex::exercise())
                .await
                .unwrap();
        });
    assert_eq!(before, tree(path));
    assert!(path.join(".verre/config.json").exists());
    assert!(path.join(".verre/state.json").exists());
    assert!(path.join(".verre/raw").is_dir());
    assert!(path.join(".verre/index.db").is_file());
}

async fn browser_fixture(path: &Path, cwd: &Path) {
    let fake = Arc::new(Fake {
        starts: Default::default(),
        cwd: cwd.to_owned(),
        prompts: Default::default(),
        attaches: Default::default(),
        detaches: Default::default(),
        shutdowns: Default::default(),
        background: Arc::new(true.into()),
        authenticated: Default::default(),
        history_path: path.join(".claude/fake-transcript"),
        reads: Default::default(),
        option_reads: Default::default(),
    });
    let authenticated = fake.authenticated.clone();
    let home = Home::open(path.join(".verre"), path.to_owned()).unwrap();
    let registry = Registry::new([fake as Arc<dyn Agent>]).unwrap();
    let server = Server::bind(Core::new(home, registry), Some(0))
        .await
        .unwrap();
    println!("{}", server.local_url());
    std::io::stdout().flush().unwrap();
    server
        .serve(async move {
            let _ = tokio::task::spawn_blocking(move || {
                for line in std::io::stdin()
                    .lock()
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    assert_eq!(line, "complete-sign-in");
                    authenticated.store(true, Ordering::Relaxed);
                    println!("sign-in completed");
                }
            })
            .await;
        })
        .await
        .unwrap();
}

fn fake_tailscale(port: &str) {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let marker = format!("127.0.0.1:{port}");
    match args.as_slice() {
        ["status", "--json"] => println!(
            "{}",
            json!({"BackendState": "Running", "Self": {"DNSName": "verre.example.test."}})
        ),
        ["serve", "status", "--json"] => {
            let existing = json!({"TCP":{"443":{"HTTPS":true}}});
            if let Ok(mut connection) = std::net::TcpStream::connect(marker) {
                use std::io::Read;
                let mut target = String::new();
                connection.read_to_string(&mut target).unwrap();
                let mut status = existing;
                status["Foreground"] = json!({"verre":{
                    "TCP":{"8443":{"HTTPS":true}},
                    "Web":{(REMOTE_HOST):{"Handlers":{"/":{"Proxy":target}}}}
                }});
                println!("{status}");
            } else {
                println!("{existing}");
            }
        }
        ["serve", "--https=8443", target] => {
            let stop = std::path::PathBuf::from(std::env::var_os("USERPROFILE").unwrap())
                .join(".verre/stop-serve-fixture");
            let signal = std::net::TcpListener::bind(marker).unwrap();
            for mut connection in signal.incoming().flatten() {
                if fs::remove_file(&stop).is_ok() {
                    return;
                }
                connection.write_all(target.as_bytes()).unwrap();
            }
        }
        _ => panic!("Unexpected fake Tailscale invocation: {args:?}"),
    }
}

async fn exercise(path: &Path, cwd: &Path, fake_bin: &Path, marker_port: u16) {
    fs::create_dir(path.join(".verre")).unwrap();
    fs::write(
        path.join(".verre/config.json"),
        json!({
            "agents": {"fake": {"enabled": true, "extraArgs": ["--fixture"], "env": {"CUSTOM": "local-value"}}},
            "customAcpAgents": [{"id": "example", "name": "Example", "command": "example", "env": {"CUSTOM": "custom-value"}}]
        }).to_string(),
    ).unwrap();
    let fake = Arc::new(Fake {
        starts: Default::default(),
        cwd: cwd.to_owned(),
        prompts: Default::default(),
        attaches: Default::default(),
        detaches: Default::default(),
        shutdowns: Default::default(),
        background: Default::default(),
        authenticated: Default::default(),
        history_path: path.join(".claude/fake-transcript"),
        reads: Default::default(),
        option_reads: Default::default(),
    });
    let home = Home::discover().unwrap();
    assert_eq!(home.root(), path.join(".verre"));
    let local_token = home.local_token();
    let registry = || {
        Registry::new([fake.clone() as Arc<dyn Agent>])
            .unwrap()
            .watch_history("fake", vec![path.join(".claude")])
    };
    let core = Core::new(home, registry());
    for _ in 0..2 {
        verre_agent_api::drift::report("fake", "record", Some("unmapped_fixture"));
        core.refresh_history().await;
    }
    let log = fs::read_to_string(path.join(".verre/logs/core.log")).unwrap();
    assert_eq!(
        log.matches("Native format drift: fake record (unmapped_fixture)")
            .count(),
        1
    );
    core.refresh_history().await;
    assert_eq!(fake.reads.load(Ordering::Relaxed), 2);
    core.refresh_history().await;
    assert_eq!(fake.reads.load(Ordering::Relaxed), 2);
    drop(core);
    let index = path.join(".verre/index.db");
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
    let paths = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        std::env::join_paths(
            std::iter::once(fake_bin.to_owned()).chain(std::env::split_paths(&paths)),
        )
        .unwrap(),
    );
    std::env::set_var("VERRE_TEST_TAILSCALE_PORT", marker_port.to_string());
    let server = Server::bind(core, Some(0)).await.unwrap();
    std::env::set_var("PATH", paths);
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
    let mut unauthenticated = Client::connect(addr, "", &origin).await;
    assert_eq!(
        unauthenticated.request("threads.list", json!({})).await["error"]["code"],
        "unauthorized"
    );
    assert_eq!(
        unauthenticated.request("app.ping", json!({})).await["error"]["code"],
        "unauthorized"
    );
    assert_eq!(
        unauthenticated.request("app.hello", hello()).await["error"]["code"],
        "unauthorized"
    );
    unauthenticated
        .wait_closed_within(Duration::from_secs(6))
        .await;
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
    let session = local.body["session"].as_str().unwrap().to_owned();
    assert_eq!(socket_error(addr, "http://untrusted.test").await, 403);
    assert_eq!(
        http(
            addr,
            "GET",
            "/api/auth/session",
            &[("Authorization", &format!("Bearer {session}"))],
            None
        )
        .await
        .body["remoteClient"],
        false
    );

    let mut first = Client::connect(addr, &session, &origin).await;
    let mut second = Client::connect(addr, &session, &origin).await;
    assert_eq!(first.call("app.hello", hello()).await["protocolVersion"], 1);
    second.call("app.hello", hello()).await;
    assert_eq!(first.call("app.ping", json!({})).await, json!({}));
    let agents = first.call("agents.list", json!({})).await;
    assert_eq!(agents["agents"][0]["id"], "fake");
    let options = first
        .call("agents.options", json!({"agentId": "fake", "cwd": cwd}))
        .await;
    assert_eq!(options["needsSession"], true);
    assert_eq!(fake.option_reads.load(Ordering::Relaxed), 0);
    let options = first
        .call(
            "agents.options",
            json!({"agentId": "fake", "cwd": cwd, "prepare": true}),
        )
        .await;
    assert_eq!(options["needsSession"], false);
    assert_eq!(options["options"][0]["currentValue"], "native");
    let remote_headers = [("Host", REMOTE_HOST), ("Origin", REMOTE_ORIGIN)];
    assert_eq!(
        http(addr, "GET", "/api/health", &remote_headers, None)
            .await
            .status,
        403
    );
    let before = first.call("remote.status", json!({})).await;
    assert_eq!(before["enabled"], false);
    assert_eq!(before["tailscale"]["running"], true);
    let settings = json!({"enabled": true, "tailscaleServe": true});
    let enabled = first.call("remote.set", settings.clone()).await;
    assert_eq!(enabled["tailscale"]["serveUrl"], REMOTE_ORIGIN);
    assert_eq!(
        http(addr, "GET", "/api/health", &remote_headers, None)
            .await
            .status,
        200
    );
    assert_eq!(
        http(
            addr,
            "GET",
            "/api/health",
            &[
                ("Host", REMOTE_HOST),
                ("Origin", "http://verre.example.test:8443")
            ],
            None
        )
        .await
        .status,
        403
    );
    let mut wrong_host = Client::connect(addr, &session, REMOTE_ORIGIN).await;
    assert_eq!(
        wrong_host.request("app.hello", hello()).await["error"]["code"],
        "unauthorized"
    );
    let pairing = first.call("remote.createPairing", json!({})).await;
    assert_eq!(
        pairing["url"],
        format!(
            "{REMOTE_ORIGIN}/#pair={}",
            pairing["code"].as_str().unwrap()
        )
    );
    let pair = json!({"code": pairing["code"], "deviceName": "Test device"});
    let paired = http(
        addr,
        "POST",
        "/api/auth/pair",
        &remote_headers,
        Some(pair.clone()),
    )
    .await;
    assert_eq!(paired.status, 200);
    let paired_session = paired.body["session"].as_str().unwrap();
    let token = paired_session.split_once('.').unwrap().1;
    let persisted = fs::read_to_string(path.join(".verre/state.json")).unwrap();
    assert!(!persisted.contains(token));
    assert!(persisted.contains("sha256:"));
    assert_eq!(
        http(
            addr,
            "POST",
            "/api/auth/pair",
            &remote_headers,
            Some(pair.clone())
        )
        .await
        .status,
        401
    );
    let mut remote = Client::connect(addr, paired_session, REMOTE_ORIGIN).await;
    assert_eq!(
        remote.call("app.hello", hello()).await["remoteClient"],
        true
    );
    let local_settings = first.call("settings.get", json!({})).await;
    let remote_settings = remote.call("settings.get", json!({})).await;
    assert_eq!(
        local_settings["agents"]["fake"]["env"]["CUSTOM"],
        "local-value"
    );
    assert_eq!(remote_settings["agents"]["fake"]["env"]["CUSTOM"], "***");
    assert_eq!(
        remote_settings["customAcpAgents"][0]["env"]["CUSTOM"],
        "***"
    );
    assert_eq!(
        remote_settings["agents"]["fake"]["extraArgs"],
        json!(["--fixture"])
    );
    first
        .call("prefs.set", json!({"prefs": {"theme": "dark"}}))
        .await;
    assert!(remote.call("prefs.get", json!({})).await["prefs"].is_null());
    remote
        .call("prefs.set", json!({"prefs": {"theme": "light"}}))
        .await;
    assert_eq!(
        second.call("prefs.get", json!({})).await["prefs"]["theme"],
        "dark"
    );
    let devices = remote.call("remote.status", json!({})).await;
    assert_eq!(devices["devices"][0]["current"], true);
    assert_eq!(
        remote.call("threads.list", json!({})).await["threads"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for (method, params) in [
        ("remote.set", settings.clone()),
        ("remote.createPairing", json!({})),
        ("settings.update", json!({"patch": {}})),
        ("fs.list", json!({"path": path})),
        ("fs.find", json!({"cwd": path, "query": "sentinel"})),
        ("workspace.gitStatus", json!({"cwd": path})),
        ("workspace.worktrees", json!({"cwd": path})),
        (
            "workspace.readFile",
            json!({"cwd": path, "path": "file.rs"}),
        ),
        (
            "workspace.readFile",
            json!({"cwd": "", "path": cwd.join("file.rs")}),
        ),
        (
            "workspace.gitDiff",
            json!({"cwd": path, "path": "sentinel"}),
        ),
        (
            "workspace.openExternal",
            json!({"cwd": cwd, "target": "folder"}),
        ),
        ("app.notify", json!({"title": "Test", "body": "Test"})),
        ("agents.options", json!({"agentId": "fake", "cwd": path})),
        ("agents.authenticate", json!({})),
        ("projects.add", json!({"path": cwd})),
        (
            "threads.start",
            json!({"agentId": "fake", "cwd": path, "content": [], "options": {}}),
        ),
    ] {
        assert_eq!(
            remote.request(method, params).await["error"]["code"],
            "unauthorized"
        );
    }
    assert!(remote.call("fs.list", json!({"path": cwd})).await["parent"].is_null());
    assert_eq!(
        remote
            .call("workspace.readFile", json!({"cwd": cwd, "path": "file.rs"}))
            .await,
        json!({"path": "file.rs", "resolvedPath": "file.rs", "sizeBytes": 8,
            "content": {"kind": "text", "text": "sentinel", "language": "rust", "truncated": false}})
    );
    assert_eq!(
        remote
            .request(
                "workspace.readFile",
                json!({"cwd": cwd, "path": cwd.join("file.rs")})
            )
            .await["error"]["code"],
        "bad_request"
    );
    assert_eq!(
        remote
            .call("workspace.worktrees", json!({"cwd": cwd}))
            .await,
        json!({"isRepo": false, "mainRoot": null, "worktrees": []})
    );
    assert_eq!(
        remote
            .call("agents.options", json!({"agentId": "fake", "cwd": cwd}))
            .await["needsSession"],
        false
    );
    first
        .call(
            "remote.revokeDevice",
            json!({"deviceId": devices["devices"][0]["id"]}),
        )
        .await;
    remote.wait_closed().await;
    let mut revoked = Client::connect(addr, paired_session, REMOTE_ORIGIN).await;
    assert_eq!(
        revoked.request("app.hello", hello()).await["error"]["code"],
        "unauthorized"
    );
    for _ in 0..8 {
        assert_eq!(
            http(
                addr,
                "POST",
                "/api/auth/pair",
                &remote_headers,
                Some(pair.clone())
            )
            .await
            .status,
            401
        );
    }
    assert_eq!(
        http(
            addr,
            "POST",
            "/api/auth/pair",
            &remote_headers,
            Some(pair.clone())
        )
        .await
        .status,
        429
    );
    let other_peer = [
        remote_headers[0],
        remote_headers[1],
        ("X-Forwarded-For", "100.64.0.5"),
    ];
    assert_eq!(
        http(addr, "POST", "/api/auth/pair", &other_peer, Some(pair))
            .await
            .status,
        401
    );
    let replacement = first.call("remote.createPairing", json!({})).await;
    let replacement_device = http(
        addr,
        "POST",
        "/api/auth/pair",
        &remote_headers,
        Some(json!({"code": replacement["code"], "deviceName": "Replacement"})),
    )
    .await;
    assert_eq!(replacement_device.status, 200);
    let replacement_session = replacement_device.body["session"].as_str().unwrap();
    let replacement_id = replacement_session.split_once('.').unwrap().0;
    let mut replacement_client = Client::connect(addr, replacement_session, REMOTE_ORIGIN).await;
    replacement_client.call("app.hello", hello()).await;
    let interrupted_code = first.call("remote.createPairing", json!({})).await;
    let stop_serve = path.join(".verre/stop-serve-fixture");
    fs::write(&stop_serve, []).unwrap();
    drop(
        TcpStream::connect(("127.0.0.1", marker_port))
            .await
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while stop_serve.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the fake Serve process receives the stop signal");
    replacement_client.wait_closed().await;
    let disabled = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = second.recv().await;
            let status = &event["data"]["status"];
            if event["event"] == "remote.updated" && status["enabled"] == false {
                break status.clone();
            }
        }
    })
    .await
    .expect("desktop receives remote status after the Serve process exits");
    assert!(disabled["tailscale"]["serveUrl"].is_null());
    assert_eq!(disabled["devices"][0]["id"], replacement_id);
    assert_eq!(socket_error(addr, REMOTE_ORIGIN).await, 403);
    let restarted = first.call("remote.set", settings.clone()).await;
    assert!(restarted["pairingExpiresAt"].is_null());
    assert_eq!(
        http(
            addr,
            "POST",
            "/api/auth/pair",
            &remote_headers,
            Some(json!({
                "code": interrupted_code["code"], "deviceName": "Interrupted"
            }))
        )
        .await
        .status,
        401
    );
    let mut replacement_client = Client::connect(addr, replacement_session, REMOTE_ORIGIN).await;
    assert_eq!(
        replacement_client.call("app.hello", hello()).await["remoteClient"],
        true
    );
    let status = replacement_client
        .call("remote.revokeDevice", json!({"deviceId": replacement_id}))
        .await;
    assert_eq!(status["devices"], json!([]));
    let close = tokio::time::timeout(Duration::from_secs(2), replacement_client.socket.next())
        .await
        .unwrap();
    assert!(matches!(close, Some(Ok(Message::Close(_)))));
    let disabled = first
        .call(
            "remote.set",
            json!({"enabled": false, "tailscaleServe": true}),
        )
        .await;
    assert_eq!(disabled, before);
    let marker = format!("127.0.0.1:{marker_port}");
    assert!(std::net::TcpStream::connect(&marker).is_err());
    let occupied = std::net::TcpListener::bind(&marker).unwrap();
    let occupier = std::thread::spawn(move || {
        let (mut socket, _) = occupied.accept().unwrap();
        socket.write_all(b"http://127.0.0.1:1").unwrap();
    });
    assert_eq!(
        first.request("remote.set", settings.clone()).await["error"]["code"],
        "bad_request"
    );
    occupier.join().unwrap();
    assert_eq!(first.call("remote.status", json!({})).await, before);
    first.call("remote.set", settings).await;
    exercise_external_proxy(addr, &mut first, &session).await;

    let content = json!([{"type": "text", "text": "  Exact \r\nテキスト \"\u{1f30d}\"\n"}, {"type": "file", "path": "file.rs", "name": null}]);
    let start =
        json!({"agentId": "fake", "cwd": cwd, "content": content, "options": {"model": "chosen"}});
    assert_eq!(
        first.request("threads.start", start.clone()).await["error"]["code"],
        "auth_required"
    );
    first.until_agent_auth("required").await;
    let auth_id = first
        .send_request(
            "agents.authenticate",
            json!({"agentId": "fake", "methodId": "fake-login"}),
        )
        .await;
    first.call("projects.list", json!({})).await;
    assert!(!fake.authenticated.load(Ordering::Relaxed));
    second.call("projects.add", json!({"path": fake_bin})).await;
    let updated = first.recv().await;
    assert_eq!(updated["event"], "projects.updated");
    let auth = first.recv_response(auth_id).await;
    assert!(auth["error"].is_null());
    first.until_agent_auth("ok").await;
    let mut start = start;
    start["clientMessageId"] = json!("initial-message");
    let mut recovery = start.clone();
    recovery["recoverOnly"] = json!(true);
    assert_eq!(
        second.request("threads.start", recovery.clone()).await["error"]["code"],
        "delivery_unknown"
    );
    assert_eq!(fake.starts.load(Ordering::Relaxed), 0);
    let (started, duplicate) = tokio::join!(
        first.call("threads.start", start.clone()),
        second.call("threads.start", start),
    );
    let id = started["thread"]["id"].as_str().unwrap();
    assert_eq!(id, "fake:live");
    assert_eq!(duplicate["thread"]["id"], id);
    assert_eq!(
        second.call("threads.start", recovery.clone()).await["thread"]["id"],
        id
    );
    assert_eq!(fake.starts.load(Ordering::Relaxed), 1);
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
    let filtered = second
        .call("threads.list", json!({"sources": ["cli"]}))
        .await;
    assert!(filtered["threads"]
        .as_array()
        .unwrap()
        .iter()
        .all(|thread| thread["id"] != id));
    let attention = second.call("threads.active", json!({})).await;
    assert_eq!(attention["threads"][0]["id"], id);
    assert_eq!(attention["threads"][0]["status"], "awaiting_input");
    assert!(attention["nextCursor"].is_null());
    assert_eq!(
        first
            .request("threads.detach", json!({"threadId": id}))
            .await["error"]["code"],
        "busy"
    );
    assert_eq!(fake.detaches.load(Ordering::Relaxed), 0);
    fake.background.store(true, Ordering::Relaxed);
    let queued = json!({
        "threadId": id,
        "content": [{"type": "text", "text": "Queued"}],
        "clientMessageId": "queued-message"
    });
    tokio::join!(
        first.call("threads.send", queued.clone()),
        second.call("threads.send", queued),
    );
    second
        .call(
            "threads.send",
            json!({"threadId": id, "content": [], "clientMessageId": "queued-message", "recoverOnly": true}),
        )
        .await;
    assert_eq!(
        second
            .request(
                "threads.send",
                json!({"threadId": id, "content": content, "clientMessageId": "absent-message", "recoverOnly": true}),
            )
            .await["error"]["code"],
        "delivery_unknown"
    );
    let snapshot = second.call("threads.open", json!({"threadId": id})).await;
    assert_eq!(snapshot["queue"].as_array().unwrap().len(), 1);
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
    let mut second = Client::connect(addr, &session, &origin).await;
    second.call("app.hello", hello()).await;
    let reopened = second.call("threads.open", json!({"threadId": id})).await;
    assert_eq!(original, reopened);
    assert_eq!(reopened["thread"]["model"], "updated");
    first.call("threads.close", json!({"threadId": id})).await;
    first.events.clear();
    assert_eq!(
        first
            .request("threads.detach", json!({"threadId": id}))
            .await["error"]["code"],
        "background_work"
    );
    assert_eq!(fake.detaches.load(Ordering::Relaxed), 0);
    first
        .call("threads.detach", json!({"threadId": id, "force": true}))
        .await;
    fake.background.store(false, Ordering::Relaxed);
    let (_, detached) = second
        .until_event(id, |event| event["thread"]["live"] == false)
        .await;
    assert_eq!(detached["thread"]["live"], false);
    let dormant = second.call("threads.open", json!({"threadId": id})).await;
    assert_eq!(dormant["options"][0]["currentValue"], "updated");
    assert!(fake.shutdowns.load(Ordering::Relaxed) > 0);
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
    let resumed = second.call("threads.open", json!({"threadId": id})).await;
    assert_eq!(resumed["options"][0]["currentValue"], "updated");
    assert_eq!(fake.detaches.load(Ordering::Relaxed), 1);
    let resumed_items = resumed["items"].as_array().unwrap();
    let resumed_ids: std::collections::HashSet<&str> = resumed_items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(resumed_ids.len(), resumed_items.len());
    assert_eq!(
        resumed_items
            .iter()
            .filter(|item| item["type"] == "user_message" && item["content"][0]["text"] == "Resumed")
            .count(),
        1
    );
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
    assert_eq!(history["options"][0]["currentValue"], "native");
    assert_eq!(history["commands"][0]["name"], "help");
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
    let refreshed_history = first
        .call("threads.open", json!({"threadId": "fake:history"}))
        .await;
    let history_items = refreshed_history["items"].as_array().unwrap();
    assert_eq!(history_items.len(), 6);
    let history_ids: std::collections::HashSet<&str> = history_items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(history_ids.len(), history_items.len());
    assert_eq!(
        history_items
            .iter()
            .filter(|item| item["id"] == "external-0")
            .count(),
        1
    );
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
    first.call("projects.add", json!({"path": path})).await;
    first
        .call("projects.update", json!({"path": path, "hidden": true}))
        .await;
    assert_eq!(
        first
            .call("search.threads", json!({"query": "Renamed", "limit": 1}))
            .await["hits"],
        json!([])
    );
    let hidden = first
        .call(
            "search.threads",
            json!({"query": "Renamed", "includeHidden": true}),
        )
        .await;
    assert_eq!(hidden["hits"][0]["thread"]["id"], "fake:history");
    first.call("projects.remove", json!({"path": path})).await;
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
    assert!(projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|project| { project["path"] == json!(cwd) && project["threadCount"] == 3 }));
    assert_eq!(
        first.request("projects.remove", json!({"path": cwd})).await["error"]["code"],
        "bad_request"
    );
    first
        .call("projects.remove", json!({"path": fake_bin}))
        .await;
    let projects = first.call("projects.list", json!({})).await;
    assert!(projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .all(|project| project["path"] != json!(fake_bin)));
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

    let proxy_origin = "https://restart.example.test";
    first
        .call(
            "remote.set",
            json!({"enabled": true, "externalUrl": proxy_origin}),
        )
        .await;
    let pairing = first.call("remote.createPairing", json!({})).await;
    let pairing_headers = [("Host", "restart.example.test"), ("Origin", proxy_origin)];
    let paired_before_restart = http(
        addr,
        "POST",
        "/api/auth/pair",
        &pairing_headers,
        Some(json!({"code": pairing["code"], "deviceName": "Before restart"})),
    )
    .await;
    assert_eq!(paired_before_restart.status, 200);
    let retained_session = paired_before_restart.body["session"].as_str().unwrap();
    let mut paired = Client::connect(addr, retained_session, proxy_origin).await;
    paired.call("app.hello", hello()).await;
    drop(paired);

    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
    assert!(std::net::TcpStream::connect(&marker).is_err());
    let home = Home::discover().unwrap();
    assert_eq!(home.settings().idle_release_minutes, 0);
    assert_eq!(home.local_token(), local_token);
    assert_eq!(fake.option_reads.load(Ordering::Relaxed), 1);
    let state = fs::read_to_string(home.root().join("state.json")).unwrap();
    assert!(!state.contains(&session));
    assert_eq!(
        serde_json::from_str::<Value>(&state).unwrap()["threads"][id]["options"]["model"],
        "updated"
    );
    let restarted = Server::bind(Core::new(home, registry()), Some(0))
        .await
        .unwrap();
    let restarted_addr = restarted.address();
    let (stop, shutdown) = oneshot::channel();
    let serving = tokio::spawn(restarted.serve(async {
        let _ = shutdown.await;
    }));
    let mut resumed = Client::connect(
        restarted_addr,
        &session,
        &format!("http://{restarted_addr}"),
    )
    .await;
    resumed.call("app.hello", hello()).await;
    assert_eq!(socket_error(restarted_addr, proxy_origin).await, 403);
    resumed
        .call(
            "remote.set",
            json!({"enabled": true, "externalUrl": proxy_origin}),
        )
        .await;
    let mut retained = Client::connect(restarted_addr, retained_session, proxy_origin).await;
    assert_eq!(
        retained.call("app.hello", hello()).await["remoteClient"],
        true
    );
    let mut revoked_before_restart =
        Client::connect(restarted_addr, replacement_session, proxy_origin).await;
    assert_eq!(
        revoked_before_restart.request("app.hello", hello()).await["error"]["code"],
        "unauthorized"
    );
    let prompt_count = fake.prompts.lock().unwrap().len();
    assert_eq!(
        resumed.request("threads.start", recovery).await["error"]["code"],
        "delivery_unknown"
    );
    assert_eq!(
        resumed
            .request(
                "threads.send",
                json!({"threadId": id, "content": [], "clientMessageId": "queued-message", "recoverOnly": true}),
            )
            .await["error"]["code"],
        "delivery_unknown"
    );
    assert_eq!(fake.starts.load(Ordering::Relaxed), 1);
    assert_eq!(fake.prompts.lock().unwrap().len(), prompt_count);
    assert_eq!(
        resumed.call("prefs.get", json!({})).await["prefs"]["theme"],
        "dark"
    );
    resumed
        .call("agents.options", json!({"agentId": "fake", "cwd": cwd}))
        .await;
    assert_eq!(fake.option_reads.load(Ordering::Relaxed), 1);
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

fn hello() -> Value {
    json!({"clientName": "e2e", "protocolVersion": 1, "futureField": true})
}

struct Client {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    events: VecDeque<Value>,
    session: String,
}

impl Client {
    async fn connect(addr: SocketAddr, session: &str, origin: &str) -> Self {
        let (socket, _) = connect_async(ws_request(addr, origin)).await.unwrap();
        Self {
            socket,
            next_id: 0,
            events: VecDeque::new(),
            session: session.into(),
        }
    }

    async fn recv(&mut self) -> Value {
        let frame = self.socket.next().await.unwrap().unwrap();
        serde_json::from_str(frame.to_text().unwrap()).unwrap()
    }

    async fn wait_closed(&mut self) {
        self.wait_closed_within(Duration::from_secs(2)).await;
    }

    async fn wait_closed_within(&mut self, timeout: Duration) {
        tokio::time::timeout(timeout, async {
            while let Some(Ok(message)) = self.socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params).await;
        self.recv_response(id).await
    }

    async fn send_request(&mut self, method: &str, mut params: Value) -> u64 {
        if method == "app.hello" {
            params["session"] = json!(self.session);
        }
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
        id
    }

    async fn recv_response(&mut self, id: u64) -> Value {
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

async fn exercise_external_proxy(addr: SocketAddr, desktop: &mut Client, local_session: &str) {
    let origin = "https://proxy.example.test";
    let host = "proxy.example.test";
    let headers = [("Host", host), ("Origin", origin)];
    for invalid in [
        "http://proxy.example.test",
        "https://proxy.example.test/subpath",
        "https://proxy.example.test/#fragment",
        "https://proxy.example.test/?query",
        "https://user:password@proxy.example.test",
        "https://localhost.",
        "https://127.1",
        "https://[::ffff:127.0.0.1]",
    ] {
        let params = json!({"enabled": true, "externalUrl": invalid});
        assert_eq!(
            desktop.request("remote.set", params).await["error"]["code"],
            "bad_request",
            "{invalid}"
        );
    }
    assert_eq!(
        desktop.call("remote.status", json!({})).await["tailscale"]["serveUrl"],
        REMOTE_ORIGIN
    );
    let previous = desktop.call("remote.createPairing", json!({})).await;
    let status = desktop
        .call(
            "remote.set",
            json!({"enabled": true, "externalUrl": "https://PROXY.example.test:443/"}),
        )
        .await;
    assert_eq!(status["externalUrl"], origin);
    assert!(status["tailscale"]["serveUrl"].is_null());
    assert!(status["pairingExpiresAt"].is_null());
    assert_eq!(socket_error(addr, REMOTE_ORIGIN).await, 403);
    assert_eq!(
        socket_error(addr, "https://proxy.example.test:8443").await,
        403
    );
    assert_eq!(
        http(
            addr,
            "GET",
            "/api/health",
            &[("Host", host), ("Origin", "https://elsewhere.example.test")],
            None
        )
        .await
        .status,
        403
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "/api/auth/pair",
            &headers,
            Some(json!({"code": previous["code"], "deviceName": "Old"}))
        )
        .await
        .status,
        401
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "/api/auth/local",
            &headers,
            Some(json!({"token": "unused"}))
        )
        .await
        .status,
        403
    );
    let mut wrong = Client::connect(addr, local_session, origin).await;
    assert_eq!(
        wrong.request("app.hello", hello()).await["error"]["code"],
        "unauthorized"
    );
    drop(wrong);
    let pairing = desktop.call("remote.createPairing", json!({})).await;
    let code = pairing["code"].as_str().unwrap();
    assert_eq!(pairing["url"], format!("{origin}/#pair={code}"));
    assert_eq!(
        desktop.call("remote.status", json!({})).await["pairingExpiresAt"],
        pairing["expiresAt"]
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "/api/auth/pair",
            &headers,
            Some(json!({"code": code, "deviceName": "Proxy device", "proposedSession": "invalid"})),
        )
        .await
        .status,
        400
    );
    let proposed = format!("{}.{}", "a".repeat(32), "b".repeat(64));
    let device = json!({"code": code, "deviceName": "Proxy device", "proposedSession": proposed});
    let paired = http(
        addr,
        "POST",
        "/api/auth/pair",
        &headers,
        Some(device.clone()),
    )
    .await;
    assert_eq!(paired.status, 200);
    assert_eq!(paired.body["session"], proposed);
    let recovered = http(
        addr,
        "POST",
        "/api/auth/pair",
        &headers,
        Some(device.clone()),
    )
    .await;
    assert_eq!(recovered.status, 200);
    assert_eq!(recovered.body, paired.body);
    let mut wrong_token = device.clone();
    wrong_token["proposedSession"] = json!(format!("{}.{}", "a".repeat(32), "c".repeat(64)));
    assert_eq!(
        http(addr, "POST", "/api/auth/pair", &headers, Some(wrong_token))
            .await
            .status,
        401
    );
    let pending = desktop.call("remote.status", json!({})).await;
    assert_eq!(pending["devices"].as_array().unwrap().len(), 1);
    assert!(pending["pairingExpiresAt"].is_null());
    assert!(pending["devices"][0]["lastSeenAt"].is_null());
    let session = paired.body["session"].as_str().unwrap();
    let mut remote = Client::connect(addr, session, origin).await;
    assert_eq!(
        remote.call("app.hello", hello()).await["remoteClient"],
        true
    );
    let status = remote.call("remote.status", json!({})).await;
    assert_eq!(status["devices"].as_array().unwrap().len(), 1);
    assert_eq!(status["devices"][0]["current"], true);
    let connected = status["devices"][0]["lastSeenAt"].as_i64().unwrap();
    assert!(connected >= status["devices"][0]["createdAt"].as_i64().unwrap());
    desktop
        .call(
            "remote.set",
            json!({"enabled": true, "externalUrl": "https://second.example.test:8443"}),
        )
        .await;
    remote.wait_closed().await;
    assert_eq!(socket_error(addr, origin).await, 403);
    let next_tick = UNIX_EPOCH + Duration::from_millis(connected as u64 + 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while SystemTime::now() < next_tick {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Wall clock did not advance past the first connection");
    let mut remote = Client::connect(addr, session, "https://second.example.test:8443").await;
    assert_eq!(
        remote.call("app.hello", hello()).await["remoteClient"],
        true
    );
    let devices = desktop.call("remote.status", json!({})).await;
    let reconnected = devices["devices"][0]["lastSeenAt"].as_i64().unwrap();
    assert!(reconnected > connected, "{reconnected} <= {connected}");
    desktop.call("remote.set", json!({"enabled": false})).await;
    remote.wait_closed().await;
    assert_eq!(
        socket_error(addr, "https://second.example.test:8443").await,
        403
    );
    let id = status["devices"][0]["id"].as_str().unwrap();
    desktop
        .call("remote.revokeDevice", json!({"deviceId": id}))
        .await;
    desktop
        .call(
            "remote.set",
            json!({"enabled": true, "externalUrl": origin}),
        )
        .await;
    assert_eq!(
        http(addr, "POST", "/api/auth/pair", &headers, Some(device))
            .await
            .status,
        401
    );
    desktop.call("remote.set", json!({"enabled": false})).await;
}

fn ws_request(
    addr: SocketAddr,
    origin: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = format!("ws://{addr}/api/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_str(origin).unwrap());
    if let Some(host) = origin.strip_prefix("https://") {
        request
            .headers_mut()
            .insert("host", HeaderValue::from_str(host).unwrap());
    }
    request
}

async fn socket_error(addr: SocketAddr, origin: &str) -> u16 {
    match connect_async(ws_request(addr, origin)).await.unwrap_err() {
        tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
        error => panic!("Unexpected WebSocket error: {error}"),
    }
}

struct HttpReply {
    status: u16,
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
    assert!(!headers.to_ascii_lowercase().contains("\r\nset-cookie:"));
    HttpReply {
        status,
        body: serde_json::from_str(body).unwrap_or(Value::Null),
    }
}

fn tree(path: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, output: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap();
            if relative.starts_with(".verre") {
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
