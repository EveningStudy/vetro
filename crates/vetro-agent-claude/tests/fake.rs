use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, BufRead, Write},
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use vetro_agent_api::{Agent, SessionEvent, StartRequest};
use vetro_agent_claude::ClaudeAgent;
use vetro_proto::{
    ContentBlock, ItemKind, RawDirection, RequestResolution, StopReason, ThreadSource,
    ThreadStatus, ToolKind, ToolStatus,
};

const PROMPT: &str = "  Original prompt: \"你好\"\r\n\n  ";
const ID: &str = "00c02626-cafe-4abc-baba-000010000019";
const PREFIX: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--permission-prompts",
    "host",
    "--permission-prompt-tool=stdio",
    "--include-hook-events",
    "--replay-user-messages",
    "--forward-subagent-text",
];

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "-p") {
        fake(&args);
    } else if !args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--ignored" | "--list"))
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(test());
    }
}

async fn test() {
    let temp = tempfile::tempdir().unwrap();
    let agent = ClaudeAgent::new(env::current_exe().unwrap(), temp.path());
    let starter = agent.options(Some(temp.path())).await.unwrap();
    assert_eq!(starter.options[0].choices[0].value, "haiku");
    assert_eq!(starter.commands[0].name, "help");
    let info = agent.info().await;
    assert_eq!(info.auth.account.as_deref(), Some("synthetic-plan"));
    assert_eq!(info.version.as_deref(), Some("test-version"));
    assert!(info.capabilities.mid_turn_input);

    let (sink, mut receiver) = unbounded_channel();
    let session = agent
        .start(
            StartRequest {
                cwd: temp.path().to_path_buf(),
                content: vec![text(PROMPT)],
                options: BTreeMap::new(),
            },
            sink,
        )
        .await
        .unwrap();
    let mut streaming = Vec::new();
    let mut final_id = None;
    let mut completed = false;
    let mut resolved = false;
    for step in ["always", "questions", "plan"] {
        loop {
            match next(&mut receiver).await {
                SessionEvent::TextDelta { item_id, delta } => streaming.push((item_id, delta)),
                SessionEvent::Item(item) => match item.kind {
                    ItemKind::AgentMessage {
                        text,
                        streaming: false,
                    } if text == "hello" => final_id = Some(item.id),
                    ItemKind::ToolCall(call)
                        if call.kind == ToolKind::Agent && call.status == ToolStatus::Completed =>
                    {
                        assert_eq!(
                            call.output.unwrap()["subagentItems"][0]["kind"]["text"],
                            "child text"
                        );
                        completed = true;
                    }
                    ItemKind::Request(request)
                        if request.request_id == step && request.resolution.is_none() =>
                    {
                        break
                    }
                    ItemKind::Request(request) if request.resolution.is_some() => resolved = true,
                    _ => {}
                },
                _ => {}
            }
        }
        let resolution = match step {
            "always" => RequestResolution::Selected {
                option_id: "always".into(),
            },
            "questions" => RequestResolution::Answered {
                answers: BTreeMap::from([("Which?".into(), vec!["A".into(), "B".into()])]),
            },
            _ => RequestResolution::Selected {
                option_id: "deny".into(),
            },
        };
        if step == "plan" {
            session.send(vec![text("fold this turn")]).await.unwrap();
        }
        session.respond(step, resolution).await.unwrap();
    }
    assert_eq!(
        streaming,
        vec![
            ("message:0".into(), "thinking".into()),
            ("message:1".into(), "hello".into())
        ]
    );
    assert_eq!(final_id.as_deref(), Some("message:1"));
    let mut limit = false;
    loop {
        match next(&mut receiver).await {
            SessionEvent::Item(item) => match item.kind {
                ItemKind::Request(request) if request.resolution.is_some() => resolved = true,
                ItemKind::TurnEnd {
                    stop_reason: StopReason::EndTurn,
                    duration_ms: Some(123),
                    usage: Some(usage),
                } => {
                    assert_eq!(usage.input, Some(7));
                    break;
                }
                _ => {}
            },
            SessionEvent::Usage(usage) if !usage.rate_limits.is_empty() => {
                assert_eq!(usage.rate_limits[0].used_percent, 75.0);
                limit = true;
            }
            _ => {}
        }
    }
    assert!(completed && limit && resolved);
    assert_eq!(
        next(&mut receiver).await,
        SessionEvent::Status(ThreadStatus::Idle)
    );
    assert!(session
        .set_option("model", "haiku")
        .await
        .unwrap()
        .iter()
        .any(|option| option.current_value == "haiku"));
    session.set_option("mode", "plan").await.unwrap();
    session.send(vec![text("interrupt me")]).await.unwrap();
    session.interrupt().await.unwrap();
    let mut cumulative = None;
    loop {
        match next(&mut receiver).await {
            SessionEvent::Usage(usage) => cumulative = usage.total,
            SessionEvent::Item(item) => {
                if let ItemKind::TurnEnd {
                    stop_reason: StopReason::Cancelled,
                    usage,
                    ..
                } = item.kind
                {
                    assert_eq!(usage.unwrap().input, Some(8));
                    break;
                }
            }
            _ => {}
        }
    }
    let cumulative = cumulative.unwrap();
    assert_eq!(cumulative.input, Some(22));
    assert_eq!(cumulative.reasoning, Some(2));
    let log = session.wire_log().snapshot();
    let spawn: Value = serde_json::from_str(
        &log.iter()
            .find(|entry| entry.direction == RawDirection::Spawn)
            .unwrap()
            .text,
    )
    .unwrap();
    let mut expected = PREFIX.to_vec();
    expected.extend(["--session-id", session.native_id()]);
    assert_eq!(spawn["args"], json!(expected));
    assert_eq!(spawn["env"], json!({}));
    let sent: Vec<Value> = log
        .iter()
        .filter(|entry| entry.direction == RawDirection::Sent)
        .map(|entry| serde_json::from_str(&entry.text).unwrap())
        .collect();
    assert_eq!(sent[0]["request"], json!({"subtype":"initialize"}));
    assert_eq!(
        sent[1]["message"],
        json!({"role":"user","content":[{"type":"text","text":PROMPT}]})
    );
    session.detach().await;
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);

    let project = temp.path().join("encoded-project");
    let original = temp.path().join("original-cwd");
    fs::create_dir_all(&original).unwrap();
    let side = project.join(ID).join("subagents");
    fs::create_dir_all(&side).unwrap();
    let rows = [
        json!({"type":"user","uuid":"history-user","sessionId":ID,"cwd":original,"entrypoint":"sdk-cli","timestamp":"2026-11-23T00:00:00.100Z","message":{"content":"Initial title"}}),
        json!({"type":"summary","summary":"Generated summary"}),
        json!({"type":"custom-title","customTitle":"Own Title"}),
        json!({"type":"assistant","uuid":"history-assistant","message":{"id":"history-api","model":"haiku","content":[{"type":"tool_use","id":"call","name":"Agent","input":{}}]}}),
        json!({"type":"user","uuid":"history-result","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"done"}]},"toolUseResult":{"agentId":"child"}}),
        json!({"type":"assistant","uuid":"history-inline","isSidechain":true,"agentId":"child","message":{"content":[{"type":"text","text":"inline"}]}}),
        json!({"type":"system","subtype":"compact_boundary","uuid":"compact"}),
    ];
    let content = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let log_path = project.join(format!("{ID}.jsonl"));
    fs::write(&log_path, format!("{content}\n{{\"type\":\"user\"")).unwrap();
    fs::write(side.join("agent-child.jsonl"), rows[5].to_string()
        + "\n"
        + &json!({"type":"assistant","uuid":"history-child","isSidechain":true,"message":{"content":[{"type":"thinking","thinking":"child thoughts"}]}}).to_string()).unwrap();
    fs::write(
        side.join("agent-child.meta.json"),
        r#"{"toolUseId":"call"}"#,
    )
    .unwrap();
    let list = agent.list(None).await.unwrap();
    assert_eq!(list.sessions.len(), 1);
    let record = &list.sessions[0];
    assert_eq!(record.title, "Own Title");
    assert_eq!(record.preview.as_deref(), Some("Initial title"));
    assert_eq!(record.origin.as_deref(), Some("sdk-cli"));
    assert_eq!(record.source, ThreadSource::Exec);
    assert_eq!(record.cwd.as_deref(), Some(original.as_path()));
    assert_eq!(record.created_at, Some(1_795_392_000_100));
    let history = agent.read(ID, None).await.unwrap();
    assert_eq!(history.items.len(), 3);
    let ItemKind::ToolCall(call) = &history.items[1].kind else {
        panic!("missing Agent call")
    };
    assert_eq!(call.status, ToolStatus::Completed);
    let children = call.output.as_ref().unwrap()["subagentItems"]
        .as_array()
        .unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["kind"]["text"], "inline");
    assert_eq!(children[1]["kind"]["text"], "child thoughts");
    assert_eq!(
        fs::read_to_string(log_path).unwrap(),
        format!("{content}\n{{\"type\":\"user\"")
    );
    let (sink, _events) = unbounded_channel();
    let resumed = agent.attach(ID, Some(temp.path()), sink).await.unwrap();
    let log = resumed.wire_log().snapshot();
    let spawn: Value = serde_json::from_str(&log[0].text).unwrap();
    let mut expected = PREFIX.to_vec();
    expected.extend(["--resume", ID]);
    assert_eq!(spawn["args"], json!(expected));
    assert_eq!(spawn["cwd"], json!(original));
    agent.shutdown().await;
    println!(
        "fake Claude passed: no injection, streaming, permissions, interrupt, history, resume"
    );
}

async fn next(receiver: &mut UnboundedReceiver<SessionEvent>) -> SessionEvent {
    let event = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(event, SessionEvent::Closed { .. }), "{event:?}");
    event
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

fn fake(args: &[String]) {
    assert_eq!(&args[..PREFIX.len()], PREFIX);
    assert!(matches!(
        args[PREFIX.len()].as_str(),
        "--session-id" | "--resume"
    ));
    assert_eq!(args.len(), PREFIX.len() + 2);
    let mut first = true;
    let mut folded = None;
    for line in io::stdin().lock().lines() {
        let row: Value = serde_json::from_str(&line.unwrap()).unwrap();
        match row["type"].as_str().unwrap() {
            "control_request" => {
                let req = &row["request"];
                let response = match req["subtype"].as_str().unwrap() {
                    "initialize" => {
                        assert_eq!(req, &json!({"subtype":"initialize"}));
                        json!({
                            "models":[{"value":"haiku","displayName":"Haiku","supportedEffortLevels":["low"]}],
                            "commands":[{"name":"help","description":"Help","argumentHint":""}],
                            "claude_code_version":"test-version","current_permission_mode":"manual",
                            "account":{"subscriptionType":"synthetic-plan"}
                        })
                    }
                    "set_model" => {
                        assert_eq!(req["model"], "haiku");
                        json!({})
                    }
                    "set_permission_mode" => {
                        assert_eq!(req["mode"], "plan");
                        json!({})
                    }
                    "interrupt" => json!({}),
                    _ => panic!("unexpected control request"),
                };
                emit(
                    json!({"type":"control_response","response":{"subtype":"success","request_id":row["request_id"],"response":response}}),
                );
                if req["subtype"] == "interrupt" {
                    emit(json!({
                        "type":"result","uuid":"interrupted","is_error":true,
                        "usage":{"input_tokens":8,"output_tokens":2},
                        "modelUsage":{
                            "primary":{"inputTokens":10,"outputTokens":6,"cacheReadInputTokens":4,"cacheCreationInputTokens":1},
                            "child":{"inputTokens":7,"outputTokens":3,"thinkingTokens":2}
                        }
                    }));
                }
            }
            "user" if first => {
                first = false;
                assert_eq!(
                    row["message"],
                    json!({"role":"user","content":[{"type":"text","text":PROMPT}]})
                );
                emit(row);
                stream(json!({"type":"message_start","message":{"id":"message"}}));
                stream(
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
                );
                stream(
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"thinking"}}),
                );
                assistant(
                    "thought",
                    None,
                    json!({"type":"thinking","thinking":"thinking"}),
                );
                stream(
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
                );
                stream(
                    json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hello"}}),
                );
                assistant("answer", None, json!({"type":"text","text":"hello"}));
                assistant(
                    "agent",
                    None,
                    json!({"type":"tool_use","name":"Agent","id":"call","input":{"description":"Child"}}),
                );
                assistant(
                    "child",
                    Some("call"),
                    json!({"type":"text","text":"child text"}),
                );
                emit(
                    json!({"type":"user","uuid":"output","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"done"}]}}),
                );
                permission(
                    "always",
                    "Bash",
                    json!({"command":"echo answer"}),
                    json!([suggestion()]),
                );
            }
            "user" if row["message"]["content"][0]["text"] == "fold this turn" => {
                folded = Some(row);
            }
            "user" => assert_eq!(row["message"]["content"][0]["text"], "interrupt me"),
            "control_response" => {
                let response = &row["response"];
                assert_eq!(response["subtype"], "success");
                let answer = &response["response"];
                match response["request_id"].as_str().unwrap() {
                    "always" => {
                        assert_eq!(
                            answer,
                            &json!({"behavior":"allow","updatedInput":{"command":"echo answer"},"toolUseID":"always-call","updatedPermissions":[suggestion()]})
                        );
                        permission(
                            "questions",
                            "AskUserQuestion",
                            json!({"questions":[{"question":"Which?","multiSelect":true}]}),
                            json!([]),
                        );
                    }
                    "questions" => {
                        assert_eq!(answer["updatedInput"]["answers"], json!({"Which?":"A, B"}));
                        permission(
                            "plan",
                            "ExitPlanMode",
                            json!({"plan":"Example plan"}),
                            json!([]),
                        );
                    }
                    "plan" => {
                        assert_eq!(answer["behavior"], "deny");
                        let mut replay = folded.take().unwrap();
                        replay["isReplay"] = json!(true);
                        emit(replay);
                        emit(
                            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour","utilization":0.75,"resetsAt":1795392000}}),
                        );
                        emit(
                            json!({"type":"result","uuid":"result","subtype":"success","duration_ms":123,"usage":{"input_tokens":2,"cache_creation_input_tokens":1,"cache_read_input_tokens":4,"output_tokens":4}}),
                        );
                    }
                    _ => panic!("unexpected permission answer"),
                }
            }
            _ => panic!("unexpected frame"),
        }
    }
}

fn suggestion() -> Value {
    json!({"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"echo answer"}],"behavior":"allow","destination":"localSettings"})
}

fn permission(id: &str, tool: &str, input: Value, suggestions: Value) {
    emit(json!({"type":"control_request","request_id":id,"request":{
        "subtype":"can_use_tool","tool_name":tool,"tool_use_id":format!("{id}-call"),
        "input":input,"permission_suggestions":suggestions
    }}));
}

fn stream(event: Value) {
    emit(json!({"type":"stream_event","event":event,"parent_tool_use_id":null}));
}

fn assistant(id: &str, parent: Option<&str>, block: Value) {
    let message_id = if parent.is_some() {
        "subagent-message"
    } else {
        "message"
    };
    emit(
        json!({"type":"assistant","uuid":id,"parent_tool_use_id":parent,
        "message":{"id":message_id,"content":[block]}}),
    );
}

fn emit(row: Value) {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{row}").unwrap();
    stdout.flush().unwrap();
}
