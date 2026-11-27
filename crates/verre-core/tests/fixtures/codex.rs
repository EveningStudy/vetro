use std::{
    env, fs,
    io::{self, BufRead, Write},
    sync::Arc,
};

use serde_json::{json, Value};
use tokio::sync::oneshot;
use verre_agent_api::{process::SpawnSpec, Agent};
use verre_agent_codex::CodexAgent;
use verre_core::{home::Home, registry::Registry, server::Server, Core};

use crate::{hello, http, Client};

const NATIVE_ID: &str = "01a07c95-7d45-7983-a759-ad414c92947e";
const THREAD_ID: &str = "codex:01a07c95-7d45-7983-a759-ad414c92947e";

pub async fn exercise() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.env.insert(
        "VERRE_FAKE_CODEX_REMAINING".into(),
        temp.path().join("remaining-turns").into_os_string(),
    );
    let registry = || {
        Registry::new([Arc::new(CodexAgent::from_spec(Some(spec.clone()))) as Arc<dyn Agent>])
            .unwrap()
    };
    let home = Home::open(temp.path().join(".verre"), temp.path().to_owned()).unwrap();
    let token = home.local_token();
    let server = Server::bind(Core::new(home, registry()), Some(0))
        .await
        .unwrap();
    let addr = server.address();
    let origin = format!("http://{addr}");
    let (stop, shutdown) = oneshot::channel();
    let serving = tokio::spawn(server.serve(async {
        let _ = shutdown.await;
    }));
    let auth = http(
        addr,
        "POST",
        "/api/auth/local",
        &[("Origin", &origin)],
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(auth.status, 200);
    let session = auth.body["session"].as_str().unwrap();
    let mut client = Client::connect(addr, session, &origin).await;
    let mut observer = Client::connect(addr, session, &origin).await;
    client.call("app.hello", hello()).await;
    observer.call("app.hello", hello()).await;
    let thread = json!({"threadId": THREAD_ID});
    let opened = observer.call("threads.open", thread.clone()).await;
    let items = opened["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    for (item, turn_id) in items.iter().zip(["turn-1", "turn-1", "turn-2", "turn-2"]) {
        assert_eq!(item["turnId"], turn_id);
    }
    assert_eq!(items[2]["type"], "user_message");
    assert_eq!(items[3]["type"], "turn_end");
    let error = client
        .request(
            "threads.rollback",
            json!({
                "threadId": THREAD_ID,
                "beforeTurnId": "missing-turn"
            }),
        )
        .await;
    assert_eq!(error["error"]["code"], "bad_request");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Codex thread/revert"));

    let rolled_back = client
        .call(
            "threads.rollback",
            json!({
                "threadId": THREAD_ID,
                "beforeTurnId": "turn-2"
            }),
        )
        .await;
    assert!(rolled_back["seq"].as_u64().unwrap() > opened["seq"].as_u64().unwrap());
    assert_eq!(rolled_back["items"], json!(items[..2]));
    loop {
        let frame = match observer.events.pop_front() {
            Some(frame) => frame,
            None => observer.recv().await,
        };
        if frame["event"] == "app.resync" {
            assert_eq!(frame["data"], json!({}));
            break;
        }
    }
    assert_eq!(
        observer.call("threads.open", thread.clone()).await["items"],
        rolled_back["items"]
    );
    let empty = client
        .call(
            "threads.rollback",
            json!({
                "threadId": THREAD_ID,
                "beforeTurnId": "turn-1"
            }),
        )
        .await;
    assert_eq!(empty["items"], json!([]));
    assert_eq!(
        observer.call("threads.open", thread).await["items"],
        json!([])
    );
    drop((client, observer));
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();

    let native_home = temp.path().join("fake-codex");
    fs::create_dir_all(native_home.join("sessions")).unwrap();
    fs::write(
        native_home
            .join("sessions")
            .join(format!("rollout-2026-09-07T00-00-00-{NATIVE_ID}.jsonl")),
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"Stale rollout"}}
"#,
    )
    .unwrap();
    let with_store = || registry().watch_history("codex", vec![native_home.clone()]);
    let clean_home = Home::open(temp.path().join(".verre-clean"), temp.path().to_owned()).unwrap();
    assert_eq!(
        Core::new(clean_home, with_store())
            .open_thread(THREAD_ID, None)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let reopened = Home::open(temp.path().join(".verre"), temp.path().to_owned()).unwrap();
    assert!(Core::new(reopened, with_store())
        .open_thread(THREAD_ID, None)
        .await
        .unwrap()
        .items
        .is_empty());
    println!("Codex rollback WebSocket fixture passed");
}

pub fn fake() {
    let saved = env::var_os("VERRE_FAKE_CODEX_REMAINING").unwrap();
    let mut remaining = fs::read_to_string(&saved).map_or(2, |value| value.parse().unwrap());
    let mut loaded = false;
    let mut stdout = io::stdout().lock();
    let mut send = |response: Value| {
        writeln!(stdout, "{response}").unwrap();
        stdout.flush().unwrap();
    };
    for line in io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let params = &message["params"];
        let response = match message["method"].as_str().unwrap() {
            "initialize" => json!({"userAgent": "verre/0.169.0"}),
            "initialized" => continue,
            "account/read" => json!({"account": {"type": "chatgpt"}}),
            "account/rateLimits/read" | "configRequirements/read" | "config/read" => json!({}),
            "thread/list" => json!({"data": []}),
            "thread/turns/list" => {
                assert_eq!(params["threadId"], NATIVE_ID);
                let turns = (1..=remaining)
                    .map(|index| json!({"id": format!("turn-{index}"), "status": "completed"}))
                    .collect::<Vec<_>>();
                json!({"data": turns})
            }
            "thread/items/list" => {
                assert_eq!(params["threadId"], NATIVE_ID);
                let turn_id = params["turnId"].as_str().unwrap();
                let index: usize = turn_id.strip_prefix("turn-").unwrap().parse().unwrap();
                assert!(index <= remaining);
                json!({"data": [{"item": {"type":"userMessage", "id": format!("user-{index}"),
                    "content": [{"type":"text", "text": format!("Message {index}")}]}}]})
            }
            "thread/resume" => {
                assert_eq!(
                    params,
                    &json!({"threadId": NATIVE_ID, "excludeTurns": true,
                    "initialTurnsPage": {"limit": 1, "itemsView": "notLoaded"}})
                );
                loaded = true;
                json!({"thread": {"id": NATIVE_ID, "status": {"type": "idle"}}})
            }
            "thread/revert" => {
                assert!(loaded);
                let target = params["beforeTurnId"].as_str().unwrap();
                assert_eq!(
                    params,
                    &json!({"threadId": NATIVE_ID, "beforeTurnId": target})
                );
                remaining = match target {
                    "turn-2" => 1,
                    "turn-1" => 0,
                    "missing-turn" => {
                        send(
                            json!({"id": message["id"], "error": {"code": -32602, "message": "Unknown turn"}}),
                        );
                        continue;
                    }
                    _ => panic!("Unexpected rollback target {target}"),
                };
                fs::write(&saved, remaining.to_string()).unwrap();
                send(json!({"method": "thread/reverted", "params": {"threadId": NATIVE_ID}}));
                json!({"thread": {"id": NATIVE_ID, "turns": []}, "turnsBackwardsCursor": null,
                    "itemsBackwardsCursor": null})
            }
            "thread/unsubscribe" => {
                assert_eq!(params, &json!({"threadId": NATIVE_ID}));
                assert!(loaded);
                loaded = false;
                json!({"status": "unsubscribed"})
            }
            method => panic!("Unexpected Codex method {method}"),
        };
        send(json!({"id": message["id"], "result": response}));
    }
}
