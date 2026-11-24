use std::{
    collections::BTreeMap,
    env,
    io::{self, BufRead, Write},
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vetro_agent_acp::{AcpAgent, SpawnSpec};
use vetro_agent_api::{Agent, SessionEvent, StartRequest};
use vetro_proto::{
    rpc::ErrorCode, AuthState, ContentBlock, ItemKind, RequestBody, RequestResolution, StopReason,
    ToolStatus,
};

const PROMPT: &str = "  Keep this exactly.\r\n\t汉字 👋\n";

fn main() {
    match env::args().nth(1).as_deref() {
        Some("--agent") => fake(env::args().nth(2).as_deref().unwrap_or_default()),
        Some("--version") => println!("fake-acp 1.0"),
        Some("auth") if env::args().nth(2).as_deref() == Some("status") => {
            println!("Not logged in");
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

fn fake(scenario: &str) {
    if scenario == "startup-failure" {
        eprintln!("duplicate --process-model");
        eprintln!("failed before initialize");
        std::process::exit(71);
    }
    if scenario == "env" {
        assert_eq!(
            env::var("VETRO_FAKE_ACP_VALUE").as_deref(),
            Ok("synthetic secret")
        );
    }
    let mut authenticated = false;
    let mut session_count = 0;
    let mut prompt_id = Value::Null;
    let mut expected_session = Value::Null;
    let mut permission = false;
    let config = json!([{ "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "cheap", "options": [{ "group":"fast", "name":"Fast", "options":[{"value":"cheap","name":"Cheap"},{"value":"other","name":"Other"}] }] }]);
    for line in io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = &message["id"];
        let params = &message["params"];
        assert_eq!(message["jsonrpc"], "2.0");
        let result = match message["method"].as_str() {
            Some("initialize") => {
                assert_eq!(
                    params,
                    &json!({
                        "protocolVersion": 1,
                        "clientCapabilities": { "fs": {"readTextFile":false,"writeTextFile":false}, "terminal":false, "elicitation":{"form":{}} },
                        "clientInfo": {"name":"vetro","version":env!("CARGO_PKG_VERSION")}
                    })
                );
                let method = if scenario == "devin" {
                    "devin-browser"
                } else {
                    "test"
                };
                json!({ "protocolVersion":1, "agentCapabilities":{"loadSession":true, "sessionCapabilities":{"list":{},"close":{},"delete":{},"resume":{}}, "promptCapabilities":{"image":true}}, "authMethods":[{"id":method,"name":"Test"}], "_meta":{"ignored":true} })
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
                assert_eq!(params.as_object().unwrap().len(), 2);
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
                json!({"sessionId":session,"configOptions":config})
            }
            Some("session/list") => {
                assert!(params.as_object().is_some());
                let cwd = env::current_dir().unwrap();
                json!({"sessions":[{"sessionId":"stored","cwd":cwd,"title":"Saved","updatedAt":"2026-11-23T00:00:00.250Z"}]})
            }
            Some("session/load" | "session/resume") => {
                if scenario == "devin" && !authenticated {
                    output(
                        json!({"id":id,"error":{"code":-32000,"message":"Authentication required"}}),
                    );
                    continue;
                }
                assert_eq!(params.as_object().unwrap().len(), 3);
                assert_eq!(params["sessionId"], "stored");
                assert_eq!(params["mcpServers"], json!([]));
                if message["method"] == "session/load" {
                    chunk(&params["sessionId"], "user_message_chunk", "old prompt");
                    chunk(&params["sessionId"], "agent_message_chunk", "old ");
                    chunk(&params["sessionId"], "agent_message_chunk", "answer");
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
                output(json!({"id":prompt_id,"result":{"stopReason":"cancelled"}}));
                continue;
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

async fn suites() {
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
    let devin = AcpAgent::new("devin", "Devin", spec);
    assert_eq!(devin.info().await.auth.state, AuthState::Required);
    assert!(!devin.recover_auth().await.unwrap());
    devin.authenticate("devin-browser").await.unwrap();
    assert_eq!(devin.process_auth(), Some(AuthState::Ok));
    devin.shutdown().await;
    assert_eq!(devin.process_auth(), Some(AuthState::Required));
    let (sink, _) = mpsc::unbounded_channel();
    assert!(
        matches!(devin.attach("stored", None, sink.clone()).await, Err(error) if error.code == ErrorCode::AuthRequired)
    );
    assert!(devin.recover_auth().await.unwrap());
    let recovered = devin.attach("stored", None, sink).await.unwrap();
    recovered.detach().await;
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
    assert_eq!(log[0].direction, vetro_proto::RawDirection::Spawn);
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
        .insert("VETRO_FAKE_ACP_VALUE".into(), "synthetic secret".into());
    let custom = AcpAgent::new("custom", "Custom ACP", spec);
    assert!(custom.info().await.error.is_none());
    let entries = custom.wire_log().await.unwrap().snapshot();
    let spawn: Value = serde_json::from_str(&entries[0].text).unwrap();
    assert_eq!(spawn["env"]["VETRO_FAKE_ACP_VALUE"], "<redacted>");
    assert!(entries
        .iter()
        .any(|entry| entry.text.contains("\"initialize\"")));
    assert!(entries
        .iter()
        .all(|entry| !entry.text.contains("synthetic secret")));
    custom.shutdown().await;
    println!("ACP fake-agent suites passed");
}
