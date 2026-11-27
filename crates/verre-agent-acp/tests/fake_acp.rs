use std::{
    collections::BTreeMap,
    env,
    io::{self, BufRead, Write},
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::mpsc;
use verre_agent_acp::{AcpAgent, SpawnSpec};
use verre_agent_api::{Agent, SessionEvent, StartRequest};
use verre_proto::{
    rpc::ErrorCode, AuthState, ContentBlock, ItemKind, RequestBody, RequestResolution, StopReason,
    ToolStatus,
};

const PROMPT: &str = "  Keep this exactly.\r\n\t汉字 👋\n";
const DEVIN_CHUNKS: [&str; 5] = [
    "发现你已经",
    "切到了新版 `verre-desktop-new.exe`（PID 34260）并关掉了旧",
    "版 `verre-desktop.exe`！这正好省去了全量重编——直接增量编译最新版的",
    " `target\\release\\verre-desktop.exe`（与正在运行的 `verre-desktop-new.exe` 互不冲突",
    "，也可并存启动）。先取消临时目录编译，走主目录快速增量构建。",
];

fn main() {
    match env::args().nth(1).as_deref() {
        Some("--agent") => fake(env::args().nth(2).as_deref().unwrap_or_default()),
        Some("--version") => println!("fake-acp 1.0"),
        Some("auth") if env::args().nth(2).as_deref() == Some("status") => {
            if env::var_os("VERRE_FAKE_DEVIN_LOGGED_IN").is_some() {
                println!("Logged in");
            } else {
                println!("Not logged in");
            }
        }
        Some("--ignored") => {}
        _ => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(75), suites())
                    .await
                    .unwrap();
            }),
    }
}

fn output(mut message: Value) {
    message["jsonrpc"] = json!("2.0");
    println!("{message}");
    io::stdout().flush().unwrap();
}

fn update(id: &Value, value: Value) {
    output(json!({ "method": "session/update", "params": { "sessionId": id, "update": value } }));
}

fn chunk(id: &Value, kind: &str, text: &str) {
    update(
        id,
        json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } }),
    );
}

fn interleaved_devin_chunks(session: &Value) {
    let tool = "call_18086028619706889594";
    update(
        session,
        json!({
            "sessionUpdate":"tool_call", "toolCallId":tool, "title":"Execute shell command", "kind":"execute"
        }),
    );
    for (index, (text, updates)) in DEVIN_CHUNKS.into_iter().zip([7, 5, 6, 6, 0]).enumerate() {
        update(
            session,
            json!({
                "sessionUpdate":"agent_message_chunk", "content":{"type":"text","text":text},
                "_meta":{"cognition.ai/timestamp":"2026-11-25T10:21:35.093854300+00:00"}
            }),
        );
        if index == 0 {
            output(json!({
                "method":"_cognition.ai/thinking_complete",
                "params":{"durationMs":5698,"blockIndex":10,"sessionId":session}
            }));
        }
        for _ in 0..updates {
            update(
                session,
                json!({
                    "sessionUpdate":"tool_call_update","toolCallId":tool,
                    "content":[{"type":"content","content":{"type":"text","text":"Compiling"}}]
                }),
            );
        }
    }
    update(
        session,
        json!({"sessionUpdate":"tool_call","toolCallId":"next-tool"}),
    );
    chunk(session, "agent_message_chunk", "Separate message");
}

fn fake(scenario: &str) {
    if scenario == "startup-failure" {
        eprintln!("duplicate --process-model");
        eprintln!("failed before initialize");
        std::process::exit(71);
    }
    if scenario == "env" {
        assert_eq!(
            env::var("VERRE_FAKE_ACP_VALUE").as_deref(),
            Ok("synthetic secret")
        );
    }
    let mut authenticated = false;
    let mut session_count = 0;
    let mut load_count = 0;
    let mut prompt_id = Value::Null;
    let mut expected_session = Value::Null;
    let mut permission = false;
    let mut dynamic = ("auto".to_owned(), "medium".to_owned());
    let dynamic_config = |model: &str, effort: &str| {
        let mut opts = vec![json!({
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": model,
            "options": [{"value": "auto", "name": "Auto"}, {"value": "pro", "name": "Pro"}]
        })];
        if model == "pro" {
            opts.push(json!({
                "id": "effortLevel",
                "name": "Effort",
                "category": "thought_level",
                "type": "select",
                "currentValue": effort,
                "options": [{"value": "low", "name": "Low"}, {"value": "medium", "name": "Medium"}, {"value": "high", "name": "High"}]
            }));
        }
        Value::Array(opts)
    };
    let config = json!([{ "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "cheap", "options": [{ "group":"fast", "name":"Fast", "options":[{"value":"cheap","name":"Cheap"},{"value":"other","name":"Other"}] }] }]);
    for line in io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = &message["id"];
        let params = &message["params"];
        assert_eq!(message["jsonrpc"], "2.0");
        if matches!(
            scenario,
            "devin-locked" | "devin-legacy-lock" | "devin-load-error"
        ) {
            assert!(matches!(
                message["method"].as_str(),
                Some("initialize" | "session/load" | "session/resume")
            ));
        }
        let result = match message["method"].as_str() {
            Some("initialize") => {
                assert_eq!(
                    params,
                    &json!({
                        "protocolVersion": 1,
                        "clientCapabilities": { "fs": {"readTextFile":false,"writeTextFile":false}, "terminal":false, "elicitation":{"form":{}} },
                        "clientInfo": {"name":"verre","version":env!("CARGO_PKG_VERSION")}
                    })
                );
                let method = if scenario == "devin" {
                    "devin-browser"
                } else {
                    "test"
                };
                let mut result = json!({ "protocolVersion":1, "agentCapabilities":{"loadSession":true, "sessionCapabilities":{"list":{},"close":{},"delete":{},"resume":{}}, "promptCapabilities":{"image":true}}, "authMethods":[{"id":method,"name":"Test"}], "_meta":{"ignored":true} });
                if scenario == "kiro-native-options" {
                    result["agentCapabilities"]["_meta"] = json!({
                        "kiro": {"extensionMethods": ["_kiro/permissions/presets/list"]}
                    });
                    result["agentCapabilities"]["sessionCapabilities"]
                        .as_object_mut()
                        .unwrap()
                        .remove("resume");
                }
                result
            }
            Some("_kiro/permissions/presets/list") if scenario == "kiro-native-options" => {
                assert!(params["cwd"].is_string());
                json!({"presets": [{"id": "allow-all", "name": "Allow all"}]})
            }
            Some("authenticate") => {
                assert_eq!(
                    params,
                    &json!({"methodId": if scenario == "devin" { "devin-browser" } else { "test" }})
                );
                authenticated = true;
                json!({})
            }
            Some("session/new") if matches!(scenario, "auth" | "devin") && !authenticated => {
                output(
                    json!({"id":id,"error":{"code":-32000,"message":"Authentication required"}}),
                );
                continue;
            }
            Some("session/new") => {
                if scenario == "kiro-native-options" && session_count > 0 {
                    assert_eq!(
                        params["_meta"],
                        json!({"kiro": {"policyPreset": ["allow-all"]}})
                    );
                } else {
                    assert_eq!(params.as_object().unwrap().len(), 2);
                }
                assert!(params["cwd"].as_str().is_some());
                assert_eq!(params["mcpServers"], json!([]));
                session_count += 1;
                let session = json!(format!("new-{session_count}"));
                for _ in 0..300 {
                    update(&session, json!({"sessionUpdate":"unknown_update"}));
                }
                update(
                    &session,
                    json!({"sessionUpdate":"available_commands_update","availableCommands":[{"name":"help","input":{"hint":"topic"}}]}),
                );
                if scenario == "dynamic-options" {
                    dynamic = ("auto".to_owned(), "medium".to_owned());
                    json!({"sessionId":session,"configOptions":dynamic_config(&dynamic.0, &dynamic.1)})
                } else {
                    json!({"sessionId":session,"configOptions":config})
                }
            }
            Some("session/list") => {
                if scenario == "devin-list-error" {
                    output(
                        json!({"id":id,"error":{"code":-32603,"message":"Failed to list sessions"}}),
                    );
                    continue;
                }
                assert!(params.as_object().is_some());
                let cwd = env::current_dir().unwrap();
                let session = if scenario == "kiro-history" {
                    "sess_fixture"
                } else {
                    "stored"
                };
                json!({"sessions":[{"sessionId":session,"cwd":cwd,"title":"Saved","updatedAt":"2026-11-23T00:00:00.250Z"}]})
            }
            Some("session/load" | "session/resume") => {
                if matches!(
                    scenario,
                    "devin-locked" | "devin-legacy-lock" | "devin-load-error"
                ) {
                    assert_eq!(params["sessionId"], "stored");
                    let (code, message, data) = match scenario {
                        "devin-locked" => (-32015, "Session is already open in another process", json!({
                            "cognition.ai/errorKind": "session_locked",
                            "cognition.ai/retryable": true
                        })),
                        "devin-legacy-lock" => (-32603, "Internal error", json!(
                            "session initialization failed: Failed to lock session 'stored': Access is denied. (os error 5)"
                        )),
                        _ => (-32603, "Internal error", json!("session initialization failed")),
                    };
                    output(json!({"id":id,"error":{
                        "code":code,
                        "message":message,
                        "data":data
                    }}));
                    continue;
                }
                if scenario == "devin" && !authenticated {
                    output(
                        json!({"id":id,"error":{"code":-32000,"message":"Authentication required"}}),
                    );
                    continue;
                }
                if scenario == "kiro-native-options" {
                    load_count += 1;
                    if load_count == 2 {
                        assert_eq!(
                            params["_meta"],
                            json!({"kiro": {"policyPreset": ["allow-all"]}})
                        );
                    } else {
                        assert_eq!(params.as_object().unwrap().len(), 3);
                    }
                } else {
                    assert_eq!(params.as_object().unwrap().len(), 3);
                }
                let session = if scenario == "kiro-history" {
                    "sess_fixture"
                } else {
                    "stored"
                };
                assert_eq!(params["sessionId"], session);
                assert_eq!(params["mcpServers"], json!([]));
                if message["method"] == "session/load" {
                    if scenario == "replay-boundaries" {
                        for line in include_str!("fixtures/acp_replay.jsonl").lines() {
                            output(serde_json::from_str(line).unwrap());
                        }
                        for _ in 0..2 {
                            update(
                                &params["sessionId"],
                                json!({"sessionUpdate":"fixture_future_update","content":{"text":"excluded from diagnostics"}}),
                            );
                        }
                    } else {
                        chunk(&params["sessionId"], "user_message_chunk", "old prompt");
                        if scenario == "devin" {
                            interleaved_devin_chunks(&params["sessionId"]);
                        } else {
                            chunk(&params["sessionId"], "agent_message_chunk", "old ");
                            chunk(&params["sessionId"], "agent_message_chunk", "answer");
                        }
                    }
                    output(json!({"id":id,"result":{"configOptions":config}}));
                    chunk(
                        &params["sessionId"],
                        "agent_message_chunk",
                        "after load response",
                    );
                    continue;
                }
                json!({"configOptions":config})
            }
            Some("session/set_config_option") if scenario == "dynamic-options" => {
                match params["configId"].as_str() {
                    Some("model") => {
                        dynamic.0 = params["value"].as_str().unwrap().to_owned();
                        dynamic.1 = "medium".to_owned();
                    }
                    Some("effortLevel") => {
                        assert_eq!(dynamic.0, "pro");
                        dynamic.1 = params["value"].as_str().unwrap().to_owned();
                    }
                    other => panic!("unexpected configId: {other:?}"),
                }
                json!({"configOptions": dynamic_config(&dynamic.0, &dynamic.1)})
            }
            Some("session/set_config_option") => {
                assert_eq!(params["configId"], "model");
                let mut config = config.clone();
                config[0]["currentValue"] = params["value"].clone();
                json!({"configOptions":config})
            }
            Some("session/close" | "session/delete") => {
                assert_eq!(params.as_object().unwrap().len(), 1);
                assert!(params["sessionId"].is_string());
                json!({})
            }
            Some("session/prompt") => {
                assert_eq!(params.as_object().unwrap().len(), 2);
                let session = &params["sessionId"];
                expected_session = session.clone();
                match params["prompt"][0]["text"].as_str().unwrap() {
                    "crash" => std::process::exit(71),
                    "cancel" => {
                        prompt_id = id.clone();
                        continue;
                    }
                    "Interact" => {
                        prompt_id = id.clone();
                        update(
                            session,
                            json!({"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Read file","kind":"read","status":"pending","locations":[{"path":"example.txt","line":1}]}),
                        );
                        output(
                            json!({"id":"permission","method":"session/request_permission","params":{"sessionId":session,"toolCall":{"toolCallId":"tool-1","title":"Read file"},"options":[{"optionId":"once","kind":"allow_once","name":"Allow once"}]}}),
                        );
                        continue;
                    }
                    PROMPT => {
                        let blocks = params["prompt"].as_array().unwrap();
                        assert_eq!(blocks.len(), 3);
                        assert_eq!(blocks[0], json!({"type":"text","text":PROMPT}));
                        assert_eq!(
                            blocks[1],
                            json!({"type":"image","mimeType":"image/png","data":"AAAA"})
                        );
                        let uri = blocks[2]["uri"].as_str().unwrap();
                        assert!(uri.starts_with("file://") && uri.ends_with("/hello%20%23.txt"));
                        assert_eq!(
                            blocks[2],
                            json!({"type":"resource_link","name":"hello #.txt","uri":uri})
                        );
                        output(json!({"id": "unknown", "method":"_example/unknown","params":{}}));
                        update(
                            session,
                            json!({"sessionUpdate":"new_unknown_update","_meta":{"ignored":1}}),
                        );
                        chunk(session, "agent_thought_chunk", "Thinking");
                        chunk(session, "agent_message_chunk", "O");
                        chunk(session, "agent_message_chunk", "K");
                        update(
                            session,
                            json!({"sessionUpdate":"plan","entries":[{"status":"completed","content":"Answer","priority":"high"}]}),
                        );
                        update(
                            session,
                            json!({"sessionUpdate":"usage_update","used":4,"size":128}),
                        );
                        update(
                            session,
                            json!({"sessionUpdate":"session_info_update","title":"New title"}),
                        );
                        update(
                            session,
                            json!({"sessionUpdate":"config_option_update","configOptions":config}),
                        );
                    }
                    "hi" if scenario == "dynamic-options" => {
                        chunk(session, "agent_message_chunk", "ok");
                    }
                    "interleaved" if scenario == "devin" => interleaved_devin_chunks(session),
                    other => panic!("unexpected or modified user input {other:?}"),
                }
                json!({"stopReason":"end_turn"})
            }
            Some("session/cancel") => {
                assert!(id.is_null());
                assert_eq!(params, &json!({"sessionId":expected_session}));
                if scenario == "cancel-timeout" {
                    continue;
                }
                if scenario == "kiro-native-options" {
                    update(
                        &expected_session,
                        json!({"sessionUpdate": "session_info_update",
                        "_meta": {"kiro": {"kind": "steering_cleared", "messageIds": ["cleared"]}}}),
                    );
                }
                output(json!({"id":prompt_id,"result":{"stopReason":"cancelled"}}));
                continue;
            }
            Some("_session/steer") if scenario == "kiro-native-options" => {
                assert_eq!(params["sessionId"], expected_session);
                let native_id = match params["message"].as_str() {
                    Some(PROMPT) => {
                        update(
                            &expected_session,
                            json!({"sessionUpdate": "session_info_update",
                            "_meta": {"kiro": {"kind": "steering_injected", "messageId": "delivered"}}}),
                        );
                        "delivered"
                    }
                    Some("Cancel this steering") => "cleared",
                    other => panic!("Unexpected steering message {other:?}"),
                };
                json!({"queued": true, "messageId": native_id})
            }
            None if id == "unknown" => {
                assert_eq!(message["error"]["code"], -32601);
                continue;
            }
            None if id == "permission" => {
                assert_eq!(
                    message["result"],
                    json!({"outcome":{"outcome":"selected","optionId":"once"}})
                );
                permission = true;
                update(
                    &expected_session,
                    json!({"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"in_progress"}),
                );
                output(
                    json!({"id":17,"method":"elicitation/create","params":{"sessionId":expected_session,"toolCallId":"tool-1","mode":"form","message":"Confirm","requestedSchema":{"type":"object","required":["confirm"],"properties":{"confirm":{"type":"boolean"}}}}}),
                );
                continue;
            }
            None if id == 17 && permission => {
                assert_eq!(
                    message["result"],
                    json!({"action":"accept","content":{"confirm":true}})
                );
                update(
                    &expected_session,
                    json!({"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed","rawOutput":{"ok":true}}),
                );
                output(json!({"id":prompt_id,"result":{"stopReason":"end_turn"}}));
                continue;
            }
            _ => panic!("unexpected outgoing ACP envelope {message}"),
        };
        output(json!({"id":id,"result":result}));
    }
}

fn agent(scenario: &str) -> AcpAgent {
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), scenario.into()];
    AcpAgent::new("fake", "Fake ACP", spec)
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

fn request(content: Vec<ContentBlock>) -> StartRequest {
    StartRequest {
        cwd: env::current_dir().unwrap(),
        content,
        options: BTreeMap::new(),
    }
}

async fn until_end(
    events: &mut mpsc::UnboundedReceiver<SessionEvent>,
) -> (StopReason, Vec<SessionEvent>) {
    let mut seen = Vec::new();
    loop {
        let event = events.recv().await.unwrap();
        if let SessionEvent::Item(item) = &event {
            if let ItemKind::TurnEnd { stop_reason, .. } = item.kind {
                return (stop_reason, seen);
            }
        }
        seen.push(event);
    }
}

async fn devin_history_replay() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "devin-locked".into()];
    spec.env
        .insert("APPDATA".into(), temp.path().as_os_str().to_owned());
    let locked = AcpAgent::new("devin", "Devin", spec.clone());
    let cwd = env::current_dir().unwrap();
    let error = locked.read("stored", Some(&cwd)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.message.contains("already open in another process"));

    spec.args[1] = "devin-legacy-lock".into();
    let legacy = AcpAgent::new("devin", "Devin", spec.clone());
    let error = legacy.read("stored", Some(&cwd)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.message.contains("Failed to lock session 'stored'"));
    legacy.shutdown().await;

    spec.args[1] = "devin".into();
    let devin = AcpAgent::new("devin", "Devin", spec.clone());
    assert_eq!(
        devin.read("stored", None).await.unwrap_err().code,
        ErrorCode::AuthRequired
    );
    devin.authenticate("devin-browser").await.unwrap();
    let transcript = devin.read("stored", Some(&cwd)).await.unwrap();
    assert!(matches!(
        &transcript.items[0].kind,
        ItemKind::UserMessage { content } if *content == [text("old prompt")]
    ));
    assert!(transcript.items.iter().any(|item| {
        matches!(&item.kind, ItemKind::AgentMessage { text, streaming: false }
            if text == &DEVIN_CHUNKS.concat())
    }));

    spec.args[1] = "devin-load-error".into();
    let failed = AcpAgent::new("devin", "Devin", spec.clone());
    assert_eq!(
        failed.read("stored", Some(&cwd)).await.unwrap_err().code,
        ErrorCode::Internal
    );
    spec.args[1] = "devin-list-error".into();
    let unlisted = AcpAgent::new("devin", "Devin", spec.clone());
    assert!(unlisted
        .read("stored", None)
        .await
        .unwrap_err()
        .message
        .contains("Failed to list sessions"));
    spec.args[1] = "startup-failure".into();
    let offline = AcpAgent::new("devin", "Devin", spec);

    let root = temp.path().join("Devin/cli");
    std::fs::create_dir_all(&root).unwrap();
    let db = rusqlite::Connection::open(root.join("sessions.db")).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA wal_autocheckpoint=0;
         CREATE TABLE sessions (id TEXT PRIMARY KEY, main_chain_id INTEGER, title TEXT,
            working_directory TEXT, created_at INTEGER, last_activity_at INTEGER,
            model TEXT, hidden INTEGER);
         CREATE TABLE message_nodes (
            session_id TEXT, node_id INTEGER, parent_node_id INTEGER,
            chat_message TEXT, created_at INTEGER, PRIMARY KEY (session_id, node_id)
         );
         CREATE TABLE tool_call_state (
            session_id TEXT, tool_call_id TEXT, tool_call_json TEXT,
            tool_call_update_json TEXT, PRIMARY KEY (session_id, tool_call_id)
         );
         INSERT INTO sessions VALUES ('stored', 7, 'Saved', 'project', 10, 20, 'fake', 0);",
    )
    .unwrap();
    let nodes = [
        (
            0,
            None,
            json!({"role":"system","content":"private context"}),
        ),
        (
            1,
            Some(0),
            json!({
                "role":"user","content":"two attachments",
                "images":[
                    {"mime_type":"image/png","base64_data":"aW1hZ2Ux"},
                    {"mime_type":"image/png","base64_data":"aW1hZ2Uy"}
                ],
                "metadata":{"created_at":"2026-11-25T12:00:00Z","is_user_input":true,"extensions":{
                    "chisel/acp-content-blocks":[{"type":"text","text":"two attachments"}]
                }}
            }),
        ),
        (
            2,
            Some(1),
            json!({"role":"assistant","content":"off the main chain"}),
        ),
        (
            3,
            Some(1),
            json!({"role":"assistant","tool_calls":[
                {"id":"tool-id","name":"test"},
                {"id":"shell","name":"get_output","arguments":{"shell_id":"job-1"}}
            ]}),
        ),
        (
            4,
            Some(3),
            json!({"role":"tool","tool_call_id":"shell","content":"session_id=sess_child"}),
        ),
        (
            5,
            Some(4),
            json!({"role":"assistant","content":"saved response","metadata":{
                "created_at":"2026-11-25T12:00:03Z",
                "finish_reason":"stop", "request_id":"request",
                "metrics":{"input_tokens":42,"output_tokens":7}
            }}),
        ),
        (
            6,
            Some(5),
            json!({"role":"user","content":"second prompt","metadata":{"is_user_input":true}}),
        ),
        (
            7,
            Some(6),
            json!({"role":"assistant","content":"interrupted"}),
        ),
    ];
    for (node_id, parent, message) in nodes {
        db.execute(
            "INSERT INTO message_nodes VALUES ('stored', ?1, ?2, ?3, 10)",
            rusqlite::params![node_id, parent, message.to_string()],
        )
        .unwrap();
    }
    let output = "x".repeat(123_943);
    db.execute(
        "INSERT INTO tool_call_state VALUES ('stored', 'tool-id', ?1, ?2)",
        [
            json!({"title":"Long output","kind":"execute"}).to_string(),
            json!({
                "status":"completed",
                "content":[{"type":"content","content":{"type":"text","text":output}}]
            })
            .to_string(),
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO tool_call_state VALUES ('stored', 'shell', NULL, '{\"status\":\"completed\"}')",
        [],
    )
    .unwrap();

    assert_eq!(
        offline.list(None).await.unwrap().sessions[0].native_id,
        "stored"
    );
    for agent in [&locked, &devin, &failed, &unlisted, &offline] {
        let transcript = agent.read("stored", None).await.unwrap();
        let [user, tool, shell, answer, end, _, interrupted] = transcript.items.as_slice() else {
            panic!("expected two historical turns, one completed");
        };
        assert!(matches!(&user.kind, ItemKind::UserMessage { content } if
            matches!(content.as_slice(), [
                ContentBlock::Text { text },
                ContentBlock::Image { data: Some(first), .. },
                ContentBlock::Image { data: Some(second), .. },
            ] if text == "two attachments" && first == "aW1hZ2Ux" && second == "aW1hZ2Uy")
        ));
        assert!(matches!(&tool.kind, ItemKind::ToolCall(tool) if
            tool.status == ToolStatus::Completed
                && tool.content == [verre_proto::ToolContent::Text { text: output.clone() }]
        ));
        assert!(matches!(&shell.kind, ItemKind::ToolCall(tool) if
            tool.input.as_ref().unwrap()["shell_id"] == "job-1"
                && tool.content == [verre_proto::ToolContent::Text { text: "session_id=sess_child".into() }]
        ));
        assert!(
            matches!(&answer.kind, ItemKind::AgentMessage { text, streaming: false }
            if text == "saved response")
        );
        assert_eq!(end.ts, answer.ts);
        assert_eq!(end.turn_id, user.turn_id);
        assert_ne!(end.turn_id, interrupted.turn_id);
        assert!(matches!(
            &end.kind,
            ItemKind::TurnEnd {
                stop_reason: StopReason::EndTurn,
                duration_ms: Some(3000),
                usage: Some(usage),
            } if usage.input == Some(42) && usage.output == Some(7)
        ));
    }
    assert!(offline.wire_log().await.is_none());

    let (sink, _) = mpsc::unbounded_channel();
    assert!(matches!(
        locked.attach("stored", Some(&cwd), sink).await,
        Err(error) if error.code == ErrorCode::Busy
    ));
    locked.shutdown().await;
    devin.shutdown().await;
    failed.shutdown().await;
    unlisted.shutdown().await;
}

async fn acp_replay_boundaries() {
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "replay-boundaries".into()];
    let agent = AcpAgent::new("custom", "Custom", spec);
    let transcript = agent.read("stored", None).await.unwrap();
    let [user, answer, end, second, partial, tool] = transcript.items.as_slice() else {
        panic!("expected completed turn and interrupted replay tail");
    };
    assert_eq!(end.turn_id, user.turn_id);
    assert_eq!(end.turn_id, answer.turn_id);
    assert_eq!(tool.turn_id, second.turn_id);
    assert_eq!(partial.turn_id, second.turn_id);
    assert_eq!(end.ts, None);
    assert_eq!(
        verre_agent_api::drift::drain()
            .into_iter()
            .filter(|line| line
                == "Native format drift: custom ACP sessionUpdate (fixture_future_update)")
            .count(),
        1
    );
    assert!(matches!(
        end.kind,
        ItemKind::TurnEnd {
            stop_reason: StopReason::EndTurn,
            duration_ms: None,
            usage: None,
        }
    ));
    agent.shutdown().await;
}

async fn kiro_native_history_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join(".kiro/sessions/project/sess_fixture");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("session.json"),
        include_str!("fixtures/kiro_session.json"),
    )
    .unwrap();
    let events: Vec<Value> = include_str!("fixtures/kiro_messages.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let log = session.join("messages.jsonl");
    let write_log = |events: &[Value]| {
        let text = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&log, text).unwrap();
    };
    write_log(&events);
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "kiro-history".into()];
    spec.env.insert(
        if cfg!(windows) { "USERPROFILE" } else { "HOME" }.into(),
        temp.path().as_os_str().to_owned(),
    );
    let agent = AcpAgent::new("kiro", "Kiro", spec);
    let listing = agent.list(None).await.unwrap();
    assert_eq!(listing.sessions[0].created_at, Some(1_795_608_000_250));
    assert_eq!(listing.sessions[0].updated_at, 1_795_608_006_000);
    assert!(listing.sessions[0].internal);
    let transcript = agent.read("sess_fixture", None).await.unwrap();
    let [user, answer, end] = transcript.items.as_slice() else {
        panic!("expected native-backed terminal footer");
    };
    assert_eq!(end.ts, Some(1_795_608_006_000));
    assert_eq!(answer.turn_id, user.turn_id);
    assert_eq!(end.turn_id, user.turn_id);
    assert!(matches!(
        end.kind,
        ItemKind::TurnEnd {
            stop_reason: StopReason::EndTurn,
            duration_ms: Some(4242),
            usage: None,
        }
    ));
    write_log(&events[..3]);
    let interrupted = agent.read("sess_fixture", None).await.unwrap();
    assert_eq!(interrupted.items.len(), 2);

    let mut mismatched = events;
    mismatched[0]["payload"]["content"] = json!("different prompt");
    write_log(&mismatched);
    let unmatched = agent.read("sess_fixture", None).await.unwrap();
    assert_eq!(unmatched.items.len(), 2);
    agent.shutdown().await;
}

async fn kiro_native_options_and_steering() {
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "kiro-native-options".into()];
    let agent = AcpAgent::new("kiro", "Kiro", spec);
    assert!(agent.info().await.capabilities.mid_turn_input);
    let desired = BTreeMap::from([("permissionPreset".into(), "allow-all".into())]);
    let cwd = env::current_dir().unwrap();
    let prepared = agent.prepare_options(Some(&cwd), &desired).await.unwrap();
    assert_eq!(
        prepared.options.last().unwrap().current_value,
        "native-default"
    );
    let (sink, mut events) = mpsc::unbounded_channel();
    let mut start = request(vec![text("cancel")]);
    start.options = desired.clone();
    let session = agent.start(start, sink).await.unwrap();
    assert_eq!(
        session
            .current_options()
            .unwrap()
            .last()
            .unwrap()
            .current_value,
        "allow-all"
    );
    assert_eq!(
        session
            .initial_options()
            .unwrap()
            .options
            .last()
            .unwrap()
            .current_value,
        "native-default"
    );
    assert!(session.can_send_during_turn(&[text(PROMPT)], true));
    assert!(!session.can_send_during_turn(
        &[
            text(PROMPT),
            ContentBlock::File {
                path: "fixture.txt".into(),
                name: None
            }
        ],
        true
    ));
    session.send(vec![text(PROMPT)]).await.unwrap();
    session
        .send(vec![text("Cancel this steering")])
        .await
        .unwrap();
    session.interrupt().await.unwrap();
    let (reason, events) = until_end(&mut events).await;
    assert_eq!(reason, StopReason::Cancelled);
    let messages: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Item(item) => match &item.kind {
                ItemKind::UserMessage { content } => Some(content.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        serde_json::to_value(messages).unwrap(),
        json!([[text("cancel")], [text(PROMPT)]])
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event,
                SessionEvent::Item(item) if matches!(&item.kind, ItemKind::Notice {
                    detail: Some(detail), ..
                } if *detail == json!([text("Cancel this steering")]))
            ))
            .count(),
        1
    );
    session.detach().await;
    agent.read("stored", Some(&cwd)).await.unwrap();
    let (sink, _events) = mpsc::unbounded_channel();
    let attached = agent
        .attach_with_options("stored", Some(&cwd), desired, sink.clone())
        .await
        .unwrap();
    assert_eq!(
        attached
            .current_options()
            .unwrap()
            .last()
            .unwrap()
            .current_value,
        "allow-all"
    );
    attached.detach().await;
    let restored = agent.attach("stored", Some(&cwd), sink).await.unwrap();
    assert_eq!(
        restored
            .current_options()
            .unwrap()
            .last()
            .unwrap()
            .current_value,
        "native-default"
    );
    restored.detach().await;
    agent.shutdown().await;
}

async fn suites() {
    devin_history_replay().await;
    acp_replay_boundaries().await;
    kiro_native_history_metadata().await;
    kiro_native_options_and_steering().await;
    let silent = agent("cancel-timeout");
    let (sink, mut events) = mpsc::unbounded_channel();
    let session = silent
        .start(request(vec![text("cancel")]), sink)
        .await
        .unwrap();
    session.interrupt().await.unwrap();
    let (reason, _) = tokio::time::timeout(Duration::from_secs(35), until_end(&mut events))
        .await
        .unwrap();
    assert_eq!(reason, StopReason::Cancelled);
    silent.shutdown().await;

    let failure = agent("startup-failure").info().await.error.unwrap();
    assert!(
        failure.contains("71")
            && failure.contains("duplicate --process-model")
            && failure.contains("failed before initialize"),
        "{failure}"
    );
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "devin".into()];
    spec.env.insert("APPDATA".into(), "".into());
    let devin = AcpAgent::new("devin", "Devin", spec);
    assert_eq!(devin.info().await.auth.state, AuthState::Required);
    assert!(!devin.recover_auth().await.unwrap());
    devin.authenticate("devin-browser").await.unwrap();
    assert_eq!(devin.process_auth(), Some(AuthState::Ok));
    devin.shutdown().await;
    assert_eq!(devin.process_auth(), None);
    let (sink, _) = mpsc::unbounded_channel();
    assert!(
        matches!(devin.attach("stored", None, sink.clone()).await, Err(error) if error.code == ErrorCode::AuthRequired)
    );
    assert!(devin.recover_auth().await.unwrap());
    let recovered = devin.attach("stored", None, sink).await.unwrap();
    recovered.detach().await;
    devin.shutdown().await;

    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "devin".into()];
    spec.env.insert("APPDATA".into(), "".into());
    spec.env
        .insert("VERRE_FAKE_DEVIN_LOGGED_IN".into(), "1".into());
    let devin = AcpAgent::new("devin", "Devin", spec);
    assert_eq!(devin.info().await.auth.state, AuthState::Ok);
    assert_eq!(devin.process_auth(), Some(AuthState::Unknown));
    assert!(!devin.recover_auth().await.unwrap());
    assert!(matches!(
        devin.read("stored", None).await,
        Err(error) if error.code == ErrorCode::AuthRequired
    ));
    assert_eq!(devin.info().await.auth.state, AuthState::Required);
    assert!(devin.recover_auth().await.unwrap());
    assert_eq!(devin.process_auth(), Some(AuthState::Ok));
    let transcript = devin.read("stored", None).await.unwrap();
    let messages: Vec<_> = transcript
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::AgentMessage { text, streaming } => {
                assert!(!streaming);
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    let expected = [DEVIN_CHUNKS.concat(), "Separate message".into()];
    assert_eq!(messages, expected);
    let (sink, mut events) = mpsc::unbounded_channel();
    let session = devin
        .start(request(vec![text("interleaved")]), sink)
        .await
        .unwrap();
    let (reason, events) = until_end(&mut events).await;
    assert_eq!(reason, StopReason::EndTurn);
    let deltas: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::TextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, DEVIN_CHUNKS[1..]);
    let finalized: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Item(item) => match &item.kind {
                ItemKind::AgentMessage {
                    text,
                    streaming: false,
                } => Some(text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(finalized, expected);
    session.detach().await;
    devin.shutdown().await;

    let agent = Arc::new(agent("auth"));
    let info = agent.info().await;
    assert_eq!(info.version.as_deref(), Some("fake-acp 1.0"));
    assert_eq!(info.auth.state, AuthState::Ok);
    assert_eq!(info.auth.methods[0].id, "test");
    assert_eq!(
        agent.list(None).await.unwrap().sessions[0].updated_at,
        1_795_392_000_250
    );
    assert_eq!(agent.info().await.auth.state, AuthState::Ok);
    assert!(agent
        .options(None, &BTreeMap::new())
        .await
        .unwrap()
        .options
        .is_empty());
    assert_eq!(agent.info().await.auth.state, AuthState::Ok);
    assert_eq!(
        agent
            .prepare_options(None, &BTreeMap::new())
            .await
            .unwrap_err()
            .code,
        ErrorCode::AuthRequired
    );
    assert_eq!(agent.info().await.auth.state, AuthState::Required);
    agent.authenticate("test").await.unwrap();
    assert_eq!(agent.info().await.auth.state, AuthState::Ok);
    let options = agent.prepare_options(None, &BTreeMap::new()).await.unwrap();
    assert_eq!(options.options[0].choices[0].group.as_deref(), Some("Fast"));
    assert_eq!(options.commands[0].name, "help");
    agent.prepare_options(None, &BTreeMap::new()).await.unwrap();

    let (sink, mut events) = mpsc::unbounded_channel();
    let session = agent
        .start(
            request(vec![
                text(PROMPT),
                ContentBlock::Image {
                    mime_type: "image/png".into(),
                    data: Some("AAAA".into()),
                    uri: None,
                },
                ContentBlock::File {
                    path: "hello #.txt".into(),
                    name: None,
                },
            ]),
            sink,
        )
        .await
        .unwrap();
    assert_eq!(session.native_id(), "new-1");
    assert_eq!(
        session.initial_options().unwrap().options[0].current_value,
        "cheap"
    );
    let (reason, seen) = until_end(&mut events).await;
    assert_eq!(reason, StopReason::EndTurn);
    assert!(seen
        .iter()
        .any(|event| matches!(event, SessionEvent::TextDelta { delta, .. } if delta == "K")));
    assert!(seen.iter().any(|event| matches!(event, SessionEvent::Usage(usage) if usage.last_turn.as_ref().unwrap().context_used == Some(4))));
    assert!(seen
        .iter()
        .any(|event| matches!(event, SessionEvent::Title(title) if title == "New title")));
    assert!(seen.iter().any(|event| matches!(event, SessionEvent::Item(item) if matches!(&item.kind, ItemKind::AgentMessage { text, streaming: false } if text == "OK"))));
    assert_eq!(
        session.set_option("model", "other").await.unwrap()[0].current_value,
        "other"
    );

    session.send(vec![text("Interact")]).await.unwrap();
    let mut tool = Vec::new();
    loop {
        if let SessionEvent::Item(item) = events.recv().await.unwrap() {
            match item.kind {
                ItemKind::Request(request) if request.resolution.is_none() => {
                    let resolution = match request.body {
                        RequestBody::Permission { .. } => {
                            let invalid = RequestResolution::Selected {
                                option_id: "invalid".into(),
                            };
                            assert_eq!(
                                session
                                    .respond(&request.request_id, invalid)
                                    .await
                                    .unwrap_err()
                                    .code,
                                ErrorCode::BadRequest
                            );
                            RequestResolution::Selected {
                                option_id: "once".into(),
                            }
                        }
                        RequestBody::Questions { .. } => RequestResolution::Answered {
                            answers: BTreeMap::from([("confirm".into(), vec!["true".into()])]),
                        },
                    };
                    session
                        .respond(&request.request_id, resolution)
                        .await
                        .unwrap();
                }
                ItemKind::ToolCall(call) => {
                    assert_eq!(item.id, "tool:tool-1");
                    assert_eq!(call.title, "Read file");
                    assert_eq!(call.locations[0].line, Some(1));
                    tool.push(call.status);
                }
                ItemKind::TurnEnd { .. } => break,
                _ => {}
            }
        }
    }
    assert_eq!(
        tool,
        [
            ToolStatus::Pending,
            ToolStatus::InProgress,
            ToolStatus::Completed
        ]
    );

    let transcript = agent.read("stored", None).await.unwrap();
    assert_eq!(transcript.items.len(), 2);
    assert!(
        matches!(&transcript.items[1].kind, ItemKind::AgentMessage { text, streaming: false } if text == "old answer")
    );
    let (other_sink, mut other_events) = mpsc::unbounded_channel();
    let attached = agent.attach("stored", None, other_sink).await.unwrap();
    assert!(!Arc::ptr_eq(&session.wire_log(), &attached.wire_log()));
    let log = attached.wire_log().snapshot();
    assert_eq!(log[0].direction, verre_proto::RawDirection::Spawn);
    assert!(log
        .iter()
        .any(|entry| entry.text.contains("\"initialize\"")));
    assert!(log
        .iter()
        .any(|entry| entry.text.contains("\"session/resume\"")));
    assert!(log.iter().all(|entry| !entry.text.contains("new-1")
        && !entry.text.contains("\"session/list\"")
        && !entry.text.contains("old prompt")));
    assert!(std::iter::from_fn(|| other_events.try_recv().ok())
        .all(|event| !matches!(event, SessionEvent::Item(_))));
    attached.send(vec![text("cancel")]).await.unwrap();
    attached.interrupt().await.unwrap();
    let (_, attached_events) = until_end(&mut other_events).await;
    attached.detach().await;

    session.send(vec![text("cancel")]).await.unwrap();
    assert_eq!(
        session.send(vec![text("cancel")]).await.unwrap_err().code,
        ErrorCode::Busy
    );
    session.interrupt().await.unwrap();
    let (reason, original_events) = until_end(&mut events).await;
    assert_eq!(reason, StopReason::Cancelled);
    let user_id = |events: &[SessionEvent]| {
        events
            .iter()
            .find_map(|event| match event {
                SessionEvent::Item(item) if matches!(item.kind, ItemKind::UserMessage { .. }) => {
                    Some(item.id.clone())
                }
                _ => None,
            })
            .unwrap()
    };
    assert_ne!(user_id(&attached_events), user_id(&original_events));
    assert_ne!(user_id(&attached_events), transcript.items[0].id);
    session.send(vec![text("crash")]).await.unwrap();
    while !matches!(
        events.recv().await.unwrap(),
        SessionEvent::Closed { error: Some(_) }
    ) {}
    assert_eq!(agent.info().await.auth.state, AuthState::Ok);
    agent.authenticate("test").await.unwrap();
    agent.prepare_options(None, &BTreeMap::new()).await.unwrap();
    agent.shutdown().await;

    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "env".into()];
    spec.env
        .insert("VERRE_FAKE_ACP_VALUE".into(), "synthetic secret".into());
    let custom = AcpAgent::new("custom", "Custom ACP", spec);
    assert!(custom.info().await.error.is_none());
    let entries = custom.wire_log().await.unwrap().snapshot();
    let spawn: Value = serde_json::from_str(&entries[0].text).unwrap();
    assert_eq!(spawn["env"]["VERRE_FAKE_ACP_VALUE"], "<redacted>");
    assert!(entries
        .iter()
        .any(|entry| entry.text.contains("\"initialize\"")));
    assert!(entries
        .iter()
        .all(|entry| !entry.text.contains("synthetic secret")));
    custom.shutdown().await;

    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args = vec!["--agent".into(), "dynamic-options".into()];
    let dynamic = AcpAgent::new("dynamic", "Dynamic ACP", spec);
    let base = dynamic
        .prepare_options(None, &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(base.options.len(), 1);
    assert_eq!(base.options[0].current_value, "auto");

    let pro = dynamic
        .prepare_options(None, &BTreeMap::from([("model".into(), "pro".into())]))
        .await
        .unwrap();
    assert!(pro.options.iter().any(|o| o.id == "effortLevel"));

    let (sink, mut events) = mpsc::unbounded_channel();
    let reset_session = dynamic
        .start(
            StartRequest {
                cwd: env::current_dir().unwrap(),
                content: vec![text("hi")],
                options: BTreeMap::new(),
            },
            sink,
        )
        .await
        .unwrap();
    let initial = reset_session.initial_options().unwrap();
    assert_eq!(initial.options.len(), 1);
    assert_eq!(initial.options[0].current_value, "auto");
    let mut final_model = None;
    while let Some(event) = events.recv().await {
        match event {
            SessionEvent::Options(options) => {
                final_model = options
                    .into_iter()
                    .find(|o| o.id == "model")
                    .map(|o| o.current_value);
            }
            SessionEvent::Item(item) if matches!(item.kind, ItemKind::TurnEnd { .. }) => break,
            _ => {}
        }
    }
    assert_eq!(final_model.as_deref(), Some("auto"));
    reset_session.detach().await;

    let (sink, mut events) = mpsc::unbounded_channel();
    let ordered = dynamic
        .start(
            StartRequest {
                cwd: env::current_dir().unwrap(),
                content: vec![text("hi")],
                options: BTreeMap::from([
                    ("effortLevel".into(), "high".into()),
                    ("model".into(), "pro".into()),
                ]),
            },
            sink,
        )
        .await
        .unwrap();
    let mut saw_high = false;
    while let Some(event) = events.recv().await {
        match event {
            SessionEvent::Options(options) => {
                if options
                    .iter()
                    .any(|o| o.id == "effortLevel" && o.current_value == "high")
                {
                    saw_high = true;
                }
            }
            SessionEvent::Item(item) if matches!(item.kind, ItemKind::TurnEnd { .. }) => break,
            _ => {}
        }
    }
    assert!(saw_high);
    ordered.detach().await;
    dynamic.shutdown().await;
    println!("ACP fake-agent suites passed");
}
