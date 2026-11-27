use std::{
    env, fs,
    io::{self, BufRead, Write},
    path::Path,
    sync::Arc,
};

use serde_json::{json, Value};
use tokio::sync::oneshot;
use verre_agent_claude::ClaudeAgent;
use verre_core::{home::Home, registry::Registry, server::Server, Core};

use crate::{hello, http, Client};

const NATIVE_ID: &str = "00c02626-cafe-4abc-baba-000010000020";
const THREAD_ID: &str = "claude:00c02626-cafe-4abc-baba-000010000020";
const CHECKPOINT: &str = "00c02626-cafe-4abc-baba-000010000021";

pub async fn exercise() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let projects = temp.path().join("projects");
    let store = projects.join("encoded-project");
    fs::create_dir(&project).unwrap();
    fs::create_dir_all(&store).unwrap();
    fs::write(
        store.join(format!("{NATIVE_ID}.jsonl")),
        format!(
            "{}\n{}\n",
            json!({
                "type":"user", "uuid":CHECKPOINT, "sessionId":NATIVE_ID, "cwd":project,
                "entrypoint":"claude-desktop", "timestamp":"2026-11-23T00:00:00.000Z",
                "message":{"content":"Stored Claude turn"}
            }),
            json!({
                "type":"assistant", "uuid":"historical-answer", "sessionId":NATIVE_ID,
                "timestamp":"2026-11-23T00:00:01.000Z",
                "message":{"id":"historical-message","model":"claude-opus-5-5",
                    "content":[{"type":"text","text":"Stored Claude answer"}],
                    "usage":{"input_tokens":2,"cache_creation_input_tokens":5,
                        "cache_read_input_tokens":30,"output_tokens":3}}
            }),
        ),
    )
    .unwrap();
    let home = temp.path().join(".verre");
    let (mut client, stop, serving) = start_core(&home, temp.path(), &projects).await;
    let thread = json!({"threadId": THREAD_ID});
    let opened = client.call("threads.open", thread.clone()).await;
    assert_eq!(opened["thread"]["source"], "desktop");
    assert_eq!(opened["thread"]["resumable"], true);
    assert_eq!(opened["thread"]["live"], false);
    assert_eq!(opened["items"][0]["id"], CHECKPOINT);
    assert_eq!(
        opened["usage"],
        json!({
            "lastTurn":{"input":37,"cachedInput":30,"output":3,"reasoning":null,
                "contextUsed":40,"contextWindow":1_000_000},
            "total":{"input":37,"cachedInput":30,"output":3,"reasoning":null,
                "contextUsed":null,"contextWindow":1_000_000},
            "rateLimits":[]
        })
    );

    let checkpoint = json!({"threadId": THREAD_ID, "userMessageId": CHECKPOINT});
    let mut preview = checkpoint.clone();
    preview["dryRun"] = json!(true);
    let result = client.call("threads.rewindFiles", preview.clone()).await;
    assert_eq!(
        result,
        json!({
            "canRewind": true, "dryRun": true, "error": null, "filesChanged": ["tracked.rs"],
            "insertions": 2, "deletions": null, "skippedLinks": []
        })
    );
    assert_eq!(
        client.call("threads.open", thread.clone()).await["thread"]["live"],
        true
    );
    let result = client.call("threads.rewindFiles", checkpoint).await;
    assert_eq!(result["canRewind"], true);
    assert_eq!(result["dryRun"], false);
    assert_eq!(result["skippedLinks"], json!(["escape"]));
    client.call("threads.detach", thread.clone()).await;

    let options = set_option(&mut client, "effort", "low").await;
    assert_eq!(current(&options, "effort"), "low");
    assert_eq!(current(&options, "mode"), "default");
    assert_spawn(&mut client, "low", None, None).await;

    let options = set_option(&mut client, "mode", "bypassPermissions").await;
    assert_eq!(current(&options, "mode"), "bypassPermissions");
    assert_spawn(&mut client, "low", None, None).await;
    set_option(&mut client, "model", "sonnet").await;
    client.call("threads.detach", thread.clone()).await;

    let options = set_option(&mut client, "effort", "high").await;
    assert_eq!(current(&options, "effort"), "high");
    assert_eq!(current(&options, "model"), "sonnet");
    assert_eq!(current(&options, "mode"), "bypassPermissions");
    assert_spawn(
        &mut client,
        "high",
        Some("sonnet"),
        Some("bypassPermissions"),
    )
    .await;

    let options = set_option(&mut client, "effort", "low").await;
    assert_eq!(current(&options, "effort"), "low");
    assert_eq!(current(&options, "model"), "sonnet");
    assert_eq!(current(&options, "mode"), "bypassPermissions");
    assert_spawn(
        &mut client,
        "low",
        Some("sonnet"),
        Some("bypassPermissions"),
    )
    .await;
    let reopened = client.call("threads.open", thread.clone()).await;
    assert_eq!(reopened["thread"]["live"], true);
    assert_eq!(current(&reopened, "effort"), "low");
    assert_eq!(
        reopened["items"][0]["content"][0]["text"],
        "Stored Claude turn"
    );

    client.call("threads.detach", thread.clone()).await;
    client.events.clear();
    client
        .call(
            "threads.send",
            json!({"threadId":THREAD_ID,"content":[{"type":"text","text":"Hold active turn"}]}),
        )
        .await;
    let response = client
        .request(
            "threads.setOption",
            json!({"threadId":THREAD_ID,"optionId":"effort","value":"high"}),
        )
        .await;
    assert_eq!(response["error"]["code"], "busy");
    assert_eq!(
        response["error"]["message"],
        "Wait for the current turn to finish before changing effort"
    );
    assert_spawn(
        &mut client,
        "low",
        Some("sonnet"),
        Some("bypassPermissions"),
    )
    .await;
    let active = client.call("threads.open", thread.clone()).await;
    assert_eq!(active["thread"]["status"], "running");
    assert_eq!(
        client.request("threads.rewindFiles", preview).await["error"]["code"],
        "busy"
    );
    client.call("threads.interrupt", thread.clone()).await;
    client
        .until_event(THREAD_ID, |event| {
            event["item"]["stopReason"] == "cancelled"
        })
        .await;
    client
        .until_event(THREAD_ID, |event| event["thread"]["status"] == "idle")
        .await;

    let state: Value =
        serde_json::from_slice(&fs::read(temp.path().join(".verre/state.json")).unwrap()).unwrap();
    assert_eq!(
        state["threads"][THREAD_ID]["options"],
        json!({"effort":"low","model":"sonnet","mode":"bypassPermissions"})
    );
    client.call("threads.detach", thread).await;
    let fork = client
        .call(
            "threads.fork",
            json!({"threadId": THREAD_ID, "turnId": CHECKPOINT}),
        )
        .await;
    let fork = &fork["thread"];
    let fork_native_id = fork["nativeId"].as_str().unwrap();
    let fork_thread_id = fork["id"].as_str().unwrap();
    assert_ne!(fork_native_id, NATIVE_ID);
    assert_eq!(fork["source"], "verre");
    assert_eq!(fork["internal"], false);
    assert_eq!(fork["model"], "default");
    let opened = client
        .call("threads.open", json!({"threadId": fork_thread_id}))
        .await;
    assert_eq!(current(&opened, "mode"), "default");
    client
        .call("threads.detach", json!({"threadId": fork_thread_id}))
        .await;
    drop(client);
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();

    fs::write(
        store.join(format!("{fork_native_id}.jsonl")),
        format!(
            "{}\n",
            json!({
                "type":"user", "uuid":CHECKPOINT, "sessionId":fork_native_id, "cwd":project,
                "entrypoint":"sdk-cli", "timestamp":"2026-11-23T01:00:00.000Z",
                "message":{"content":"Stored Claude fork"}
            }),
        ),
    )
    .unwrap();
    let reindexed_home = temp.path().join(".verre-reindexed");
    fs::create_dir(&reindexed_home).unwrap();
    fs::copy(home.join("state.json"), reindexed_home.join("state.json")).unwrap();
    let (mut client, stop, serving) = start_core(&reindexed_home, temp.path(), &projects).await;
    let list = client.call("threads.list", json!({})).await;
    let indexed = list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|thread| thread["id"] == fork_thread_id)
        .expect("Verre fork remains in the default root list after reindexing");
    assert_eq!(indexed["source"], "verre");
    assert_eq!(indexed["internal"], false);
    let reopened = client
        .call("threads.open", json!({"threadId": fork_thread_id}))
        .await;
    assert_eq!(reopened["thread"]["live"], false);
    assert_eq!(
        reopened["items"][0]["content"][0]["text"],
        "Stored Claude fork"
    );
    drop(client);
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
    println!("Claude rewind and fork WebSocket fixture passed");
}

async fn start_core(
    home: &Path,
    root: &Path,
    projects: &Path,
) -> (
    Client,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<io::Result<()>>,
) {
    let agent = Arc::new(ClaudeAgent::new(
        env::current_exe().unwrap(),
        projects.to_owned(),
    ));
    let registry = Registry::new([agent as Arc<dyn verre_agent_api::Agent>]).unwrap();
    let home = Home::open(home.to_owned(), root.to_owned()).unwrap();
    let token = home.local_token();
    let core = Core::new(home, registry);
    core.refresh_history().await;
    let server = Server::bind(core, Some(0)).await.unwrap();
    let addr = server.address();
    let (stop, shutdown) = oneshot::channel();
    let serving = tokio::spawn(server.serve(async {
        let _ = shutdown.await;
    }));
    let origin = format!("http://{addr}");
    let auth = http(
        addr,
        "POST",
        "/api/auth/local",
        &[("Origin", &origin)],
        Some(json!({"token":token})),
    )
    .await;
    assert_eq!(auth.status, 200);
    let mut client = Client::connect(addr, auth.body["session"].as_str().unwrap(), &origin).await;
    client.call("app.hello", hello()).await;
    (client, stop, serving)
}

async fn set_option(client: &mut Client, id: &str, value: &str) -> Value {
    client
        .call(
            "threads.setOption",
            json!({"threadId":THREAD_ID,"optionId":id,"value":value}),
        )
        .await
}

fn current<'a>(result: &'a Value, id: &str) -> &'a str {
    result["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == id)
        .unwrap()["currentValue"]
        .as_str()
        .unwrap()
}

async fn assert_spawn(client: &mut Client, effort: &str, model: Option<&str>, mode: Option<&str>) {
    let raw = client
        .call("threads.raw", json!({"threadId": THREAD_ID}))
        .await;
    let spawn: Value = serde_json::from_str(raw["entries"][0]["text"].as_str().unwrap()).unwrap();
    let args: Vec<_> = spawn["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    assert!(args.contains(&"--allow-dangerously-skip-permissions"));
    assert!(!args.contains(&"--dangerously-skip-permissions"));
    assert_eq!(flag(&args, "--resume"), Some(NATIVE_ID));
    assert_eq!(flag(&args, "--effort"), Some(effort));
    assert_eq!(flag(&args, "--model"), model);
    assert_eq!(flag(&args, "--permission-mode"), mode);
}

fn flag<'a>(args: &[&'a str], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find_map(|pair| (pair[0] == name).then_some(pair[1]))
}

pub fn fake() {
    let args: Vec<_> = env::args().collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    if args.contains(&"--fork-session") {
        assert_eq!(flag(&args, "--resume"), Some(NATIVE_ID));
        assert_eq!(flag(&args, "--resume-session-at"), Some(CHECKPOINT));
        assert!(flag(&args, "--session-id").is_some_and(|id| id != NATIVE_ID));
    }
    let mode = flag(&args, "--permission-mode").unwrap_or("default");
    let mut active = false;
    for line in io::stdin().lock().lines() {
        let row: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if row["type"] == "user" {
            assert_eq!(row["message"]["content"][0]["text"], "Hold active turn");
            active = true;
            continue;
        }
        let request = &row["request"];
        let id = &row["request_id"];
        let response = match request["subtype"].as_str().unwrap() {
            "initialize" => json!({
                "current_permission_mode":mode,
                "claude_code_version":"legacy-fixture",
                "models":[
                    {"value":"default","supportedEffortLevels":["low","high"],"defaultEffortLevel":"low"},
                    {"value":"sonnet","supportedEffortLevels":["low","high"],"defaultEffortLevel":"low"},
                ]
            }),
            "set_permission_mode" => {
                assert!(args.contains(&"--allow-dangerously-skip-permissions"));
                assert_eq!(request["mode"], "bypassPermissions");
                json!({"mode":request["mode"]})
            }
            "set_model" => json!({"model":request["model"]}),
            "rewind_files" => {
                assert!(!active);
                assert_eq!(request["user_message_id"], CHECKPOINT);
                match request["dry_run"].as_bool().unwrap() {
                    true => {
                        json!({"canRewind": true, "filesChanged": ["tracked.rs"], "insertions": 2})
                    }
                    false => {
                        json!({"canRewind": true, "filesChanged": ["tracked.rs"], "skippedLinks": ["escape"]})
                    }
                }
            }
            "apply_flag_settings" => {
                assert!(!active);
                emit(json!({"type":"control_response","response":{
                    "request_id":id,"subtype":"error",
                    "error":"Unsupported control request subtype: apply_flag_settings"
                }}));
                continue;
            }
            "interrupt" => {
                assert!(active);
                active = false;
                json!({})
            }
            _ => panic!("Unexpected Claude request: {request}"),
        };
        emit(json!({"type":"control_response","response":{
            "request_id":id,"subtype":"success","response":response
        }}));
        if request["subtype"] == "interrupt" {
            emit(json!({"type":"result","uuid":"interrupted","is_error":true}));
        }
    }
}

fn emit(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
