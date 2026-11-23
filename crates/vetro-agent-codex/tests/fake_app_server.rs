use std::{
    collections::BTreeMap,
    env,
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vetro_agent_api::{Agent, SessionEvent, StartRequest};
use vetro_agent_codex::CodexAgent;
use vetro_proto::{
    AuthState, ContentBlock, ItemKind, RequestBody, RequestResolution, StopReason, ThreadSource,
    ToolContent, ToolKind, ToolStatus,
};

fn main() {
    if env::args().nth(1).as_deref() == Some("app-server") {
        app_server();
    } else if !env::args().any(|arg| arg == "--ignored") {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(test_adapter());
    }
}

fn app_server() {
    let mut output = io::stdout().lock();
    let mut send = |message: Value| {
        writeln!(output, "{message}").unwrap();
        output.flush().unwrap();
    };
    let mut started = 0;
    let mut initialized = false;
    let mut cwd = Value::Null;
    for line in io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        assert!(message.get("jsonrpc").is_none());
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        if method == "initialized" {
            initialized = true;
            continue;
        }
        if message["id"] == "server-command" {
            assert_eq!(message["result"], json!({"decision": "accept"}));
            send(event(
                "serverRequest/resolved",
                json!({"threadId": "thread-1", "requestId": "server-command"}),
            ));
            send(event(
                "item/commandExecution/outputDelta",
                json!({
                    "threadId": "thread-1", "turnId": "turn-1", "itemId": "command", "delta": "approved\n",
                }),
            ));
            send(event(
                "item/completed",
                json!({"threadId": "thread-1", "turnId": "turn-1", "item":
                    json!({"type": "commandExecution", "id": "command", "command": "echo approved", "cwd": cwd,
                        "status": "completed", "exitCode": 0, "aggregatedOutput": "approved\n"}),
                }),
            ));
            continue;
        }
        assert!(initialized || method == "initialize");
        let result = match method {
            "initialize" => {
                assert_eq!(params["clientInfo"]["name"], "vetro");
                assert_eq!(params["capabilities"]["experimentalApi"], true);
                json!({"userAgent": "vetro/0.169.0 (Test) fake"})
            }
            "account/read" => {
                assert_eq!(params, &json!({"refreshToken": false}));
                json!({"account": {"type": "chatgpt", "planType": "pro"}, "requiresOpenaiAuth": true})
            }
            "account/login/start" => {
                assert_eq!(params, &json!({"type": "chatgptDeviceCode"}));
                json!({"type": "chatgptDeviceCode", "loginId": "fake-login", "userCode": "fake-code",
                    "verificationUrl": "https://example.test/device"})
            }
            "account/rateLimits/read" => {
                json!({"rateLimits": {"limitId": "main", "limitName": "Main",
                "primary": {"usedPercent": 7, "windowDurationMins": 300, "resetsAt": 60}}})
            }
            "configRequirements/read" => {
                json!({"requirements": {"allowedLoginMethods": ["chatgpt"]}})
            }
            "model/list" if params["cursor"] == "next-model" => {
                json!({"data": [fake_model("alternative", false, "high")]})
            }
            "model/list" => {
                json!({"data": [fake_model("fixture", true, "low")], "nextCursor": "next-model"})
            }
            "collaborationMode/list" => json!({"data": [{"mode": "plan", "name": "Plan"}]}),
            "permissionProfile/list" => json!({"data": [
                {"id": ":read-only", "allowed": true}, {"id": ":blocked", "allowed": false},
            ]}),
            "thread/start" => {
                started += 1;
                cwd = params["cwd"].clone();
                if started < 3 {
                    assert_eq!(params, &json!({"cwd": cwd}));
                } else {
                    assert_eq!(
                        params,
                        &json!({"cwd": cwd, "model": "alternative", "permissions": ":read-only"})
                    );
                }
                json!({"thread": {"id": format!("thread-{started}"), "status": {"type": "idle"}}, "model": "fixture"})
            }
            "turn/start" if params["threadId"] == "thread-1" => {
                assert_eq!(
                    params,
                    &json!({"threadId": "thread-1", "input": [
                        {"type": "text", "text": " user text\r\n第二行  ", "text_elements": []},
                        {"type": "image", "url": "https://example.invalid/test.png"},
                        {"type": "image", "url": "data:image/png;base64,AA=="},
                    ]})
                );
                send(event(
                    "turn/started",
                    json!({"threadId": "thread-1", "turn": {"id": "turn-1"}}),
                ));
                send(event(
                    "item/started",
                    json!({"threadId": "thread-1", "turnId": "turn-1", "item":
                    {"type": "userMessage", "id": "user", "content": params["input"]}}),
                ));
                send(event(
                    "item/agentMessage/delta",
                    json!({"threadId": "thread-1", "turnId": "turn-1", "itemId": "answer", "delta": "stream"}),
                ));
                send(event(
                    "item/completed",
                    json!({"threadId": "thread-1", "turnId": "turn-1", "item":
                    {"type": "agentMessage", "id": "answer", "text": "streamed"}}),
                ));
                for (channel, payload) in [
                    (
                        "summaryTextDelta",
                        json!({"summaryIndex": 0, "delta": "summary"}),
                    ),
                    (
                        "textDelta",
                        json!({"contentIndex": 0, "delta": "reasoning"}),
                    ),
                ] {
                    let mut payload = payload;
                    payload["threadId"] = json!("thread-1");
                    payload["turnId"] = json!("turn-1");
                    payload["itemId"] = json!("thinking");
                    send(event(&format!("item/reasoning/{channel}"), payload));
                }
                send(event(
                    "item/completed",
                    json!({"threadId": "thread-1", "turnId": "turn-1", "item":
                    {"type": "reasoning", "id": "thinking", "summary": ["summary"], "content": ["reasoning"]}}),
                ));
                send(event(
                    "item/started",
                    json!({"threadId": "thread-1", "turnId": "turn-1", "item":
                    {"type": "commandExecution", "id": "command", "command": "echo approved", "cwd": cwd, "status": "inProgress"}}),
                ));
                send(
                    json!({"id": "server-command", "method": "item/commandExecution/requestApproval", "params":
                    {"threadId": "thread-1", "turnId": "turn-1", "itemId": "command", "availableDecisions": ["accept", "decline"]}}),
                );
                json!({"turn": {"id": "turn-1", "status": "inProgress"}})
            }
            "turn/start" => {
                if params["threadId"] == "thread-3" {
                    assert_eq!(
                        params,
                        &json!({"threadId": "thread-3", "effort": "high", "input": [
                            {"type": "text", "text": "third", "text_elements": []},
                        ]})
                    );
                }
                json!({"turn": {"id": "turn-other", "status": "inProgress"}})
            }
            "turn/steer" => {
                assert_eq!(
                    params,
                    &json!({"threadId": "thread-1", "expectedTurnId": "turn-1", "input": [
                        {"type": "text", "text": "steer exactly", "text_elements": []},
                    ]})
                );
                json!({"turnId": "turn-1"})
            }
            "turn/interrupt" => {
                assert_eq!(params, &json!({"threadId": "thread-1", "turnId": "turn-1"}));
                send(event(
                    "thread/tokenUsage/updated",
                    json!({"threadId": "thread-1", "turnId": "turn-1",
                    "tokenUsage": {"last": {"inputTokens": 10}, "total": {"inputTokens": 20}, "modelContextWindow": 100}}),
                ));
                send(event(
                    "account/rateLimits/updated",
                    json!({"rateLimits": {"limitId": "main",
                    "secondary": {"usedPercent": 32, "windowDurationMins": 10080}}}),
                ));
                send(event(
                    "turn/completed",
                    json!({"threadId": "thread-1", "turn": {"id": "turn-1", "status": "interrupted"}}),
                ));
                json!({})
            }
            "thread/list" => {
                assert_eq!(params["sourceKinds"].as_array().unwrap().len(), 10);
                assert_eq!(params["modelProviders"], json!([]));
                assert_eq!(params["useStateDbOnly"], true);
                if params["cursor"] == "next-page" {
                    json!({"data": [{"id": "child", "source": {"subagent": {"thread_spawn": {"parent_thread_id": "saved"}}}}]})
                } else {
                    json!({"data": [{"id": "saved", "source": "vscode", "originator": "Codex Desktop",
                        "createdAt": 10, "updatedAt": 20, "preview": "Saved thread"}], "nextCursor": "next-page"})
                }
            }
            "thread/read" => {
                assert_eq!(params, &json!({"threadId": "saved", "includeTurns": true}));
                json!({"thread": {"id": "saved", "turns": [{"id": "history-turn", "status": "completed", "items": [
                    {"type": "userMessage", "id": "history-user", "content": [{"type": "text", "text": "saved"}]},
                    {"type": "fileChange", "id": "history-edit", "status": "completed",
                        "changes": [{"path": "foo.rs", "diff": "-old\n+new"}]},
                    {"type": "collabAgentToolCall", "id": "history-agent", "status": "completed",
                        "tool": "spawnAgent", "receiverThreadIds": ["child"]},
                ]}]}})
            }
            "thread/resume" => {
                assert_eq!(params, &json!({"threadId": "saved"}));
                json!({"thread": {"id": "saved", "name": "Renamed", "status": {"type": "idle"}}})
            }
            "thread/settings/update" => {
                assert_eq!(
                    params,
                    &json!({"threadId": "thread-1", "model": "alternative"})
                );
                json!({})
            }
            "thread/unsubscribe" if params["threadId"] == "saved" => break,
            "thread/unsubscribe" => json!({"status": "unsubscribed"}),
            _ => panic!("unexpected request: {message}"),
        };
        send(json!({"id": message["id"], "result": result}));
    }
}

fn event(method: &str, params: Value) -> Value {
    json!({"method": method, "params": params})
}

fn fake_model(model: &str, default: bool, effort: &str) -> Value {
    json!({"id": model, "model": model, "isDefault": default, "defaultReasoningEffort": effort,
        "supportedReasoningEfforts": [{"reasoningEffort": effort}]})
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

async fn test_adapter() {
    let agent = CodexAgent::new(Some(env::current_exe().unwrap()));
    let info = agent.info().await;
    assert_eq!(info.version.as_deref(), Some("0.169.0"));
    assert_eq!(info.auth.state, AuthState::Ok);
    assert_eq!(info.auth.methods[0].id, "chatgpt");
    let auth = agent.authenticate("chatgptDeviceCode").await.unwrap();
    assert_eq!(auth.code.as_deref(), Some("fake-code"));
    assert_eq!(auth.url.as_deref(), Some("https://example.test/device"));
    let options = agent.options(None, &BTreeMap::new()).await.unwrap().options;
    let choices = BTreeMap::from([("model".into(), "alternative".into())]);
    let preview = agent.options(None, &choices).await.unwrap();
    let effort = preview
        .options
        .iter()
        .find(|option| option.id == "effort")
        .unwrap();
    assert_eq!(effort.choices[0].value, "high");
    assert!(options
        .iter()
        .any(|option| option.id == "model" && option.choices.len() == 2));
    assert!(options
        .iter()
        .any(|option| option.id == "permissions" && option.choices.len() == 1));
    let cwd = PathBuf::from("scratch");
    let (sink, mut events) = mpsc::unbounded_channel();
    let first = agent
        .start(
            StartRequest {
                cwd: cwd.clone(),
                options: BTreeMap::new(),
                content: vec![
                    text(" user text\r\n第二行  "),
                    ContentBlock::Image {
                        mime_type: "image/png".into(),
                        data: None,
                        uri: Some("https://example.invalid/test.png".into()),
                    },
                    ContentBlock::Image {
                        mime_type: "image/png".into(),
                        data: Some("AA==".into()),
                        uri: None,
                    },
                ],
            },
            sink,
        )
        .await
        .unwrap();
    let (sink, mut other_events) = mpsc::unbounded_channel();
    let second = agent
        .start(
            StartRequest {
                cwd: cwd.clone(),
                options: BTreeMap::new(),
                content: vec![text("other")],
            },
            sink,
        )
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first.wire_log(), &second.wire_log()));
    assert!(first.wire_log().snapshot().iter().all(|entry| {
        !entry.text.contains("fake-code") && !entry.text.contains("https://example.test/device")
    }));
    let (sink, _) = mpsc::unbounded_channel();
    let third = agent
        .start(
            StartRequest {
                cwd,
                options: [
                    ("model".into(), "alternative".into()),
                    ("permissions".into(), ":read-only".into()),
                    ("effort".into(), "high".into()),
                ]
                .into(),
                content: vec![text("third")],
            },
            sink,
        )
        .await
        .unwrap();
    third.detach().await;
    let mut deltas = Vec::new();
    let request = loop {
        match recv(&mut events).await {
            SessionEvent::TextDelta { delta, .. } => deltas.push(delta),
            SessionEvent::Item(item) => {
                if let ItemKind::Request(request) = item.kind {
                    break request;
                }
            }
            _ => {}
        }
    };
    assert_eq!(deltas, ["stream", "summary", "\n\nreasoning"]);
    assert_eq!(request.detail.as_deref(), Some("echo approved"));
    assert!(
        matches!(request.body, RequestBody::Permission { options } if options.len() == 2 && options[0].id == "accept")
    );
    assert!(first
        .respond(
            &request.request_id,
            RequestResolution::Selected {
                option_id: "acceptForSession".into()
            }
        )
        .await
        .is_err());
    first
        .respond(
            &request.request_id,
            RequestResolution::Selected {
                option_id: "accept".into(),
            },
        )
        .await
        .unwrap();
    let mut running_output = false;
    loop {
        if let SessionEvent::Item(item) = recv(&mut events).await {
            if let ItemKind::ToolCall(tool) = item.kind {
                if tool.kind == ToolKind::Execute
                    && matches!(&tool.content[0], ToolContent::Terminal { output, .. } if output == "approved\n")
                {
                    if tool.status == ToolStatus::Completed {
                        break;
                    }
                    running_output = true;
                }
            }
        }
    }
    assert!(running_output);
    let options = first.set_option("model", "alternative").await.unwrap();
    assert!(options
        .iter()
        .any(|option| option.id == "effort" && option.current_value == "high"));
    first.send(vec![text("steer exactly")]).await.unwrap();
    first.interrupt().await.unwrap();
    let mut merged_usage = false;
    loop {
        match recv(&mut events).await {
            SessionEvent::Usage(usage) if usage.rate_limits.len() == 2 => {
                assert_eq!(usage.last_turn.unwrap().input, Some(10));
                merged_usage = true;
            }
            SessionEvent::Item(item) => {
                if matches!(
                    item.kind,
                    ItemKind::TurnEnd {
                        stop_reason: StopReason::Cancelled,
                        ..
                    }
                ) {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(merged_usage);
    let first_page = agent.list(None).await.unwrap();
    assert_eq!(first_page.sessions[0].source, ThreadSource::Desktop);
    assert_eq!(first_page.sessions[0].created_at, Some(10_000));
    let second_page = agent.list(first_page.next_cursor).await.unwrap();
    assert_eq!(
        second_page.sessions[0].parent_native_id.as_deref(),
        Some("saved")
    );
    let history = agent.read("saved", None).await.unwrap();
    assert_eq!(history.items[0].id, "history-user");
    assert!(
        matches!(&history.items[1].kind, ItemKind::ToolCall(tool) if tool.kind == ToolKind::Edit)
    );
    assert!(
        matches!(&history.items[2].kind, ItemKind::ToolCall(tool) if tool.child_thread_ids == ["codex:child"])
    );
    let (sink, mut resumed_events) = mpsc::unbounded_channel();
    let resumed = agent.attach("saved", None, sink).await.unwrap();
    assert_eq!(resumed.native_id(), "saved");
    while recv(&mut resumed_events).await != SessionEvent::Title("Renamed".into()) {}
    resumed.detach().await;
    loop {
        if let SessionEvent::Closed { error } = recv(&mut other_events).await {
            assert!(error.is_some());
            break;
        }
    }
    assert!(agent.info().await.error.is_none());
    agent.shutdown().await;
    println!("Codex app-server fixture passed");
}

async fn recv(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap()
}
