use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vetro_agent_acp::{AcpAgent, SpawnSpec};
use vetro_agent_api::{Agent, Session, SessionEvent, StartRequest, WireLog};
use vetro_proto::{
    rpc::ErrorCode, ContentBlock, ItemKind, PermissionOptionKind, RawDirection, RequestBody,
    RequestResolution, StopReason,
};

fn scratch(agent: &str) -> PathBuf {
    let root = env::var_os("VETRO_ACP_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let cwd = root.join(".orchestra").join("acp-live").join(agent);
    fs::create_dir_all(&cwd).unwrap();
    std::path::absolute(cwd).unwrap()
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

fn trim_wire(log: &WireLog) {
    let mut requests = BTreeMap::new();
    for entry in log.snapshot() {
        if !matches!(entry.direction, RawDirection::Sent | RawDirection::Received) {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&entry.text) else {
            continue;
        };
        let id = &message["id"];
        if entry.direction == RawDirection::Sent {
            if let Some(method) = message["method"].as_str() {
                if let Some(id) = id.as_u64() {
                    requests.insert(id, method.to_owned());
                }
                let params = &message["params"];
                let summary = match method {
                    "initialize" => {
                        json!({"protocolVersion":params["protocolVersion"],"clientCapabilities":params["clientCapabilities"]})
                    }
                    "authenticate" => json!({"methodId":params["methodId"]}),
                    "session/set_config_option" => {
                        json!({"configId":params["configId"],"value":params["value"]})
                    }
                    "session/new" | "session/load" | "session/resume" => {
                        json!({"mcpServers":params["mcpServers"]})
                    }
                    "session/prompt" => {
                        json!({"contentBlocks":params["prompt"].as_array().map(Vec::len)})
                    }
                    _ => json!({}),
                };
                println!("> {method} {summary}");
            } else if message.get("result").is_some() {
                println!(
                    "> interaction response {}",
                    message["result"]["outcome"]["outcome"]
                );
            }
        } else if let Some(method) = message["method"].as_str() {
            let update = &message["params"]["update"];
            if method == "session/update" {
                let kind = &update["sessionUpdate"];
                let summary = if matches!(kind.as_str(), Some("tool_call" | "tool_call_update")) {
                    json!({"kind":update["kind"],"status":update["status"]})
                } else {
                    json!({})
                };
                println!("< {method} {kind} {summary}");
            } else {
                println!("< {method}");
            }
        } else if let Some(method) = id.as_u64().and_then(|id| requests.remove(&id)) {
            let result = &message["result"];
            let summary = if !message["error"].is_null() {
                json!({"errorCode":message["error"]["code"]})
            } else {
                match method.as_str() {
                    "initialize" => json!({
                        "protocolVersion":result["protocolVersion"],
                        "agentCapabilities":result["agentCapabilities"],
                        "authMethods":result["authMethods"].as_array().into_iter().flatten().filter_map(|value|value["id"].as_str()).collect::<Vec<_>>()
                    }),
                    "session/list" => json!({
                        "count":result["sessions"].as_array().map(Vec::len),
                        "hasCursor":result["nextCursor"].is_string()
                    }),
                    "session/prompt" => json!({"stopReason":result["stopReason"]}),
                    "session/new" | "session/load" | "session/resume" => json!({
                        "hasSessionId":result["sessionId"].is_string(),
                        "mode":result["modes"]["currentModeId"],
                        "modes":result["modes"]["availableModes"].as_array().into_iter().flatten().filter_map(|mode|mode["id"].as_str()).collect::<Vec<_>>(),
                        "configOptions":result["configOptions"].as_array().into_iter().flatten().map(|option|json!({
                            "id":option["id"], "choiceCount":option["options"].as_array().map(Vec::len)
                        })).collect::<Vec<_>>(),
                    }),
                    _ => json!({}),
                }
            };
            println!("< {method} {summary}");
        }
    }
}

async fn turn(
    session: &dyn Session,
    events: &mut mpsc::UnboundedReceiver<SessionEvent>,
) -> (String, bool, bool, StopReason) {
    tokio::time::timeout(Duration::from_secs(55), async {
        let mut output = String::new();
        let mut permission = false;
        let mut tool = false;
        loop {
            match events.recv().await.unwrap() {
                SessionEvent::Item(item) => match item.kind {
                    ItemKind::AgentMessage {
                        text,
                        streaming: false,
                    } => output.push_str(&text),
                    ItemKind::ToolCall(_) => tool = true,
                    ItemKind::Request(request) if request.resolution.is_none() => {
                        let mut resolution = RequestResolution::Cancelled;
                        if let RequestBody::Permission { options } = request.body {
                            permission = true;
                            if let Some(option) = options
                                .into_iter()
                                .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                            {
                                resolution = RequestResolution::Selected {
                                    option_id: option.id,
                                };
                            }
                        }
                        session
                            .respond(&request.request_id, resolution)
                            .await
                            .unwrap();
                    }
                    ItemKind::TurnEnd { stop_reason, .. } => {
                        break (output, tool, permission, stop_reason)
                    }
                    _ => {}
                },
                SessionEvent::Closed { error } => panic!("ACP process closed: {error:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("live ACP turn timed out")
}

#[tokio::test]
#[ignore = "launches the installed Devin agent and spends its plan quota"]
async fn devin_round_trip() {
    let cwd = scratch("devin");
    let program = env::var_os("VETRO_ACP_DEVIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                PathBuf::from(
                    "D:/Software/Devin/resources/app/extensions/windsurf/devin/bin/devin.exe",
                )
            } else {
                PathBuf::from("devin")
            }
        });
    let mut spec = SpawnSpec::new(program);
    spec.cwd = Some(cwd.clone());
    spec.args = ["acp", "--process-model", "in-process"]
        .into_iter()
        .map(Into::into)
        .collect();
    let agent = AcpAgent::new("devin", "Devin", spec);
    let info = agent.info().await;
    println!("Devin version {:?}", info.version);
    assert!(
        info.capabilities.resume && info.capabilities.list_sessions,
        "{info:?}"
    );
    let _ = agent.list(None).await.unwrap();
    let probe = agent.options(Some(&cwd), &BTreeMap::new()).await;
    assert_eq!(probe.unwrap_err().code, ErrorCode::AuthRequired);
    agent.authenticate("devin-browser").await.unwrap();
    let (sink, mut events) = mpsc::unbounded_channel();
    let session = agent
        .start(
            StartRequest {
                cwd: cwd.clone(),
                content: vec![text("Reply with exactly: OK")],
                options: BTreeMap::from([("model".into(), "swe-2-flash-medium".into())]),
            },
            sink,
        )
        .await
        .unwrap();
    let log = session.wire_log();
    let (output, _, _, reason) = turn(session.as_ref(), &mut events).await;
    if output.trim() != "OK" || reason != StopReason::EndTurn {
        trim_wire(&log);
    }
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(output.trim(), "OK");
    let id = session.native_id().to_owned();
    session.detach().await;
    let replay = agent.read(&id, None).await.unwrap();
    assert!(replay.items.iter().any(
        |item| matches!(&item.kind, ItemKind::AgentMessage { text, .. } if text.trim() == "OK")
    ));
    let (sink, mut events) = mpsc::unbounded_channel();
    let resumed = agent.attach(&id, Some(&cwd), sink).await.unwrap();
    assert_eq!(resumed.native_id(), id);
    let options = resumed.set_option("mode", "ask").await.unwrap();
    assert!(options
        .iter()
        .any(|option| option.id == "mode" && option.current_value == "ask"));
    resumed.send(vec![text("Run the single harmless shell command `echo OK` now. Use the shell tool once and then stop.")]).await.unwrap();
    let (_, tool, permission, reason) = turn(resumed.as_ref(), &mut events).await;
    resumed.detach().await;
    trim_wire(&log);
    agent.shutdown().await;
    assert_eq!(reason, StopReason::EndTurn);
    assert!(tool, "Devin did not emit a tool call");
    assert!(
        permission,
        "Devin ran the tool without requesting permission"
    );
}

#[tokio::test]
#[ignore = "launches the installed Antigravity agent for handshake calls only"]
async fn antigravity_handshake() {
    let cwd = scratch("antigravity");
    let program = env::var_os("VETRO_ACP_AGY").map_or_else(|| PathBuf::from("agy"), PathBuf::from);
    let mut spec = SpawnSpec::new(program);
    spec.cwd = Some(cwd.clone());
    spec.args = vec!["acp".into()];
    let agent = AcpAgent::new("antigravity", "Antigravity", spec);
    let info = agent.info().await;
    println!("Antigravity version {:?}", info.version);
    assert!(info.error.is_none(), "{info:?}");
    let initial = agent.options(Some(&cwd), &BTreeMap::new()).await.unwrap();
    println!(
        "Antigravity reported {} configuration options",
        initial.options.len()
    );
    let page = agent.list(None).await.unwrap();
    println!("Antigravity listed {} sessions", page.sessions.len());
    let log = agent.wire_log().await.unwrap();
    trim_wire(&log);
    let sent = log
        .snapshot()
        .into_iter()
        .filter(|entry| entry.direction == RawDirection::Sent)
        .filter_map(|entry| serde_json::from_str::<Value>(&entry.text).ok())
        .map(|message| message["method"].as_str().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    agent.shutdown().await;
    for method in ["initialize", "session/new", "session/list", "session/close"] {
        assert!(sent.iter().any(|sent| sent == method), "Missing {method}");
    }
    assert!(!sent.iter().any(|sent| sent == "session/prompt"));
}
