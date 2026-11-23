use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use vetro_agent_api::{Agent, Session, SessionEvent, StartRequest};
use vetro_agent_claude::ClaudeAgent;
use vetro_proto::{
    ContentBlock, InteractionRequest, ItemKind, RawDirection, RequestResolution, StopReason,
};

#[tokio::test]
#[ignore = "Uses installed Claude Code and sends at most three short prompts to Haiku"]
async fn native_stream_permission_queue_and_resume() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = root.join(".orchestra/smoke");
    fs::create_dir_all(&scratch).unwrap();
    let cwd = tempfile::Builder::new()
        .prefix("claude-adapter-")
        .tempdir_in(scratch)
        .unwrap();
    let agent = ClaudeAgent::default();
    let (sink, mut events) = unbounded_channel();
    let session = agent
        .start(
            StartRequest {
                cwd: cwd.path().to_path_buf(),
                content: prompt("Reply with exactly: OK"),
                options: BTreeMap::from([
                    ("model".into(), "haiku".into()),
                    ("mode".into(), "manual".into()),
                ]),
            },
            sink,
        )
        .await
        .unwrap();
    let first = turn(&mut events).await;
    let hook = first.hook;
    let permission = session.send(prompt(
        "Use the Write tool once to create vetro-permission-probe.txt with the text OK in the current directory, then reply with exactly: P",
    )).await.is_ok();
    assert!(permission);
    let mut request = None;
    while let Some(event) = next(&mut events).await {
        if let SessionEvent::Item(item) = event {
            match item.kind {
                ItemKind::Request(value) if value.resolution.is_none() => {
                    request = Some(value);
                    break;
                }
                ItemKind::TurnEnd { .. } => break,
                _ => {}
            }
        }
    }
    let mut queue_replies = Vec::new();
    if let Some(InteractionRequest { request_id, .. }) = &request {
        session
            .send(prompt("Reply with exactly: QUEUE"))
            .await
            .unwrap();
        session
            .respond(
                request_id,
                RequestResolution::Selected {
                    option_id: "deny".into(),
                },
            )
            .await
            .unwrap();
        queue_replies.push(turn(&mut events).await);
        if !queue_replies[0]
            .text
            .iter()
            .any(|text| text.trim() == "QUEUE")
        {
            queue_replies.push(turn(&mut events).await);
        }
    }
    session.set_option("mode", "manual").await.unwrap();
    session.set_option("model", "haiku").await.unwrap();
    let id = session.native_id().to_owned();
    session.detach().await;
    print_trimmed(&session);

    let (sink, _resume_events) = unbounded_channel();
    let resumed = agent.attach(&id, None, sink).await.unwrap();
    resumed.set_option("model", "haiku").await.unwrap();
    let resume_log = resumed.wire_log().snapshot();
    let resume_spawn: Value = serde_json::from_str(&resume_log[0].text).unwrap();
    let resume_args = resume_spawn["args"].as_array().unwrap();
    assert_eq!(
        resume_args[resume_args.len() - 2..],
        [json!("--resume"), json!(&id)]
    );
    assert!(resume_log
        .iter()
        .any(|entry| entry.direction == RawDirection::Received
            && entry.text.contains("SessionStart")
            && entry.text.contains("ai-memory")));
    resumed.detach().await;
    print_trimmed(&resumed);
    println!(
        "queue turns: {:?}",
        queue_replies
            .iter()
            .map(|turn| (&turn.text, &turn.stop))
            .collect::<Vec<_>>()
    );
    let page = agent.list(None).await.unwrap();
    let record = page
        .sessions
        .iter()
        .find(|record| record.native_id == id)
        .unwrap();
    println!("native entrypoint: {:?}", record.origin);
    let history = agent.read(&id).await.unwrap();
    println!(
        "first assistant message retained its ID: {}",
        first
            .item_id
            .as_ref()
            .is_some_and(|id| history.items.iter().any(|item| item.id == *id))
    );
    assert_eq!(first.stop, Some(StopReason::EndTurn));
    assert_eq!(first.text, ["OK"]);
    assert!(first.streamed, "no partial deltas");
    assert!(hook, "SessionStart output did not include ai-memory");
    assert!(
        request.is_some(),
        "Claude did not ask the host for permission"
    );
    assert!(queue_replies
        .iter()
        .any(|turn| turn.text.iter().any(|text| text.trim() == "QUEUE")));
    assert_eq!(resumed.native_id(), id);
}

#[tokio::test]
#[ignore = "Reads real Claude Code history; sends no prompts"]
async fn native_history() {
    let agent = ClaudeAgent::default();
    let started = Instant::now();
    let page = agent.list(None).await.unwrap();
    let elapsed = started.elapsed();
    let labels = page
        .sessions
        .iter()
        .fold(BTreeMap::new(), |mut counts, record| {
            *counts
                .entry(record.origin.as_deref().unwrap_or("<absent>"))
                .or_insert(0) += 1;
            counts
        });
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap(),
            )
            .join(".claude")
        });
    let largest = fs::read_dir(config.join("projects"))
        .unwrap()
        .flatten()
        .filter_map(|entry| fs::read_dir(entry.path()).ok())
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|entry| Some((entry.metadata().ok()?.len(), entry.path())))
        .max_by_key(|(bytes, _)| *bytes)
        .unwrap();
    let started = Instant::now();
    let transcript = agent
        .read(largest.1.file_stem().unwrap().to_str().unwrap())
        .await
        .unwrap();
    let read_time = started.elapsed();
    println!(
        "{} listed in {elapsed:?}; entrypoints {labels:?}",
        page.sessions.len()
    );
    println!(
        "largest: {} bytes, {} items in {read_time:?}",
        largest.0,
        transcript.items.len()
    );
    assert!(!page.sessions.is_empty() && !transcript.items.is_empty());
}

fn prompt(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text { text: text.into() }]
}

async fn next(events: &mut UnboundedReceiver<SessionEvent>) -> Option<SessionEvent> {
    tokio::time::timeout(Duration::from_secs(125), events.recv())
        .await
        .expect("Claude timed out")
}

#[derive(Default)]
struct Turn {
    text: Vec<String>,
    item_id: Option<String>,
    hook: bool,
    streamed: bool,
    stop: Option<StopReason>,
}

async fn turn(events: &mut UnboundedReceiver<SessionEvent>) -> Turn {
    let mut turn = Turn::default();
    while let Some(event) = next(events).await {
        match event {
            SessionEvent::TextDelta { .. } => turn.streamed = true,
            SessionEvent::Closed { error } => panic!("Claude closed before its result: {error:?}"),
            SessionEvent::Item(item) => match item.kind {
                ItemKind::AgentMessage {
                    text,
                    streaming: false,
                } if !text.is_empty() => {
                    turn.item_id.get_or_insert(item.id);
                    turn.text.push(text);
                }
                ItemKind::Notice { text, .. } => {
                    turn.hook |= text.contains("ai-memory");
                }
                ItemKind::TurnEnd { stop_reason, .. } => {
                    turn.stop = Some(stop_reason);
                    return turn;
                }
                _ => {}
            },
            _ => {}
        }
    }
    turn
}

fn print_trimmed(session: &Arc<dyn Session>) {
    for row in session.wire_log().snapshot() {
        let Ok(value) = serde_json::from_str::<Value>(&row.text) else {
            continue;
        };
        let trimmed = match (row.direction, value["type"].as_str()) {
            (RawDirection::Sent, Some("control_request")) => {
                json!({"type":"control_request","request":value["request"]})
            }
            (RawDirection::Sent, Some("user")) => {
                json!({"type":"user","text":value["message"]["content"][0]["text"]})
            }
            (RawDirection::Sent, Some("control_response")) => {
                json!({"type":"control_response","behavior":value["response"]["response"]["behavior"]})
            }
            (RawDirection::Received, Some("control_request")) => json!({
                "type":"control_request","subtype":value["request"]["subtype"],"tool_name":value["request"]["tool_name"],
                "suggestions":value["request"]["permission_suggestions"].as_array().map(Vec::len)
            }),
            (RawDirection::Received, Some("control_response")) => json!({
                "type":"control_response","subtype":value["response"]["subtype"],
                "models":value["response"]["response"]["models"].as_array().map(Vec::len),
                "commands":value["response"]["response"]["commands"].as_array().map(Vec::len)
            }),
            (RawDirection::Received, Some("system")) if value["subtype"] == "init" => json!({
                "type":"system","subtype":"init","model":value["model"],
                "tools":value["tools"].as_array().map(Vec::len),"commands":value["slash_commands"].as_array().map(Vec::len)
            }),
            (RawDirection::Received, Some("system")) if value["hook_event"] == "SessionStart" => {
                json!({
                    "type":"system","subtype":value["subtype"],"hook_event":"SessionStart",
                    "ai_memory_marker":value.to_string().contains("ai-memory")
                })
            }
            (RawDirection::Received, Some("result")) => json!({
                "type":"result","subtype":value["subtype"],"stop_reason":value["stop_reason"],
                "duration_ms":value["duration_ms"],
            }),
            (RawDirection::Received, Some("user")) if value["isReplay"] == true => {
                json!({"type":"user","isReplay":true})
            }
            (RawDirection::Received, Some("stream_event"))
                if value["event"]["type"] == "message_start" =>
            {
                json!({"type":"stream_event","event":{"type":"message_start"},"user_message_uuid_present":value["user_message_uuid"].is_string()})
            }
            _ => continue,
        };
        println!("{:?} {trimmed}", row.direction);
    }
}
