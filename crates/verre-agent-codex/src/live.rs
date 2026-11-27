use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use tokio::sync::mpsc;
use verre_agent_api::{Agent, SessionEvent, StartRequest, WireLog};
use verre_proto::{
    ConfigOption, ContentBlock, ItemKind, RawDirection, RequestBody, RequestResolution, StopReason,
    ToolKind, ToolStatus,
};

use crate::{catalog::Catalog, CodexAgent};

#[tokio::test]
#[ignore = "uses one short provider prompt on the installed Codex"]
async fn installed_per_thread_raw_log() {
    let agent = CodexAgent::default();
    agent.list(None).await.unwrap();
    let cwd = scratch();
    let catalog = agent.options(Some(&cwd), &BTreeMap::new()).await.unwrap();
    let mut options = BTreeMap::from([("model".into(), cheap_model(&catalog.options))]);
    let dependent = agent.options(Some(&cwd), &options).await.unwrap();
    if let Some(effort) = lowest_effort(&dependent.options) {
        options.insert("effort".into(), effort);
    }
    let (sink, mut events) = mpsc::unbounded_channel();
    let session = agent
        .start(
            StartRequest {
                cwd,
                content: text("Reply OK."),
                options,
            },
            sink,
        )
        .await
        .unwrap();
    let methods: Vec<_> = session
        .wire_log()
        .snapshot()
        .into_iter()
        .filter_map(|entry| {
            if entry.direction == RawDirection::Spawn {
                return Some("spawn".to_owned());
            }
            if entry.direction != RawDirection::Sent {
                return None;
            }
            let value: Value = serde_json::from_str(&entry.text).ok()?;
            if let Some(id) = value["params"]["threadId"].as_str() {
                assert_eq!(id, session.native_id());
            }
            value["method"].as_str().map(str::to_owned)
        })
        .collect();
    assert_eq!(
        methods,
        [
            "spawn",
            "initialize",
            "initialized",
            "thread/start",
            "turn/start"
        ]
    );
    println!("Scoped Codex raw log contains {}", methods.join(", "));
    tokio::time::timeout(Duration::from_secs(45), async {
        while let Some(event) = events.recv().await {
            if let SessionEvent::Item(item) = event {
                if let ItemKind::TurnEnd { stop_reason, .. } = item.kind {
                    assert_eq!(stop_reason, StopReason::EndTurn);
                    return;
                }
            }
        }
        panic!("Codex closed before answering");
    })
    .await
    .unwrap();
    session.detach().await;
    agent.shutdown().await;
}

#[tokio::test]
#[ignore = "uses the installed Codex and two short provider prompts"]
async fn installed_app_server() {
    let cwd = scratch();
    let agent = CodexAgent::default();
    let first_page = agent.list(None).await.unwrap();
    let existing = first_page
        .sessions
        .iter()
        .find(|thread| {
            thread.resumable
                && thread
                    .preview
                    .as_ref()
                    .is_some_and(|preview| !preview.is_empty())
        })
        .expect("existing Codex history");
    let history = agent
        .read(&existing.native_id, existing.cwd.as_deref())
        .await
        .unwrap();
    assert!(history
        .items
        .iter()
        .any(|item| matches!(item.kind, ItemKind::UserMessage { .. })));
    let next_page = if let Some(cursor) = first_page.next_cursor.clone() {
        agent.list(Some(cursor)).await.unwrap().sessions.len()
    } else {
        0
    };
    println!(
        "History: page={}, next={}, read origin={:?}, items={}",
        first_page.sessions.len(),
        next_page,
        existing.source,
        history.items.len()
    );

    let app = agent.app().await.unwrap();
    let catalog = Catalog::load(&app, Some(&cwd)).await.unwrap();
    let options = catalog.options(&BTreeMap::new());
    let chosen_model = cheap_model(&options);
    let mut model_options = BTreeMap::from([("model".into(), chosen_model)]);
    if let Some(effort) = lowest_effort(&catalog.options(&model_options)) {
        model_options.insert("effort".into(), effort);
    }
    let (sink, mut events) = mpsc::unbounded_channel();
    let session = agent
        .start(
            StartRequest {
                cwd: cwd.clone(),
                options: model_options.clone(),
                content: text("Reply with exactly: OK"),
            },
            sink,
        )
        .await
        .unwrap();
    let mut answer = String::new();
    let end = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match events.recv().await.expect("live session event") {
                SessionEvent::Item(item) => match item.kind {
                    ItemKind::AgentMessage {
                        text,
                        streaming: false,
                    } => answer = text,
                    ItemKind::TurnEnd { stop_reason, .. } => break stop_reason,
                    ItemKind::Notice { text, .. } => println!("Live turn notice: {text}"),
                    _ => {}
                },
                SessionEvent::Closed { error } => panic!("Codex closed: {error:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("short reply timed out");
    assert_eq!(end, StopReason::EndTurn);
    assert_eq!(answer.trim(), "OK");
    let original = session.native_id().to_owned();
    session.detach().await;
    let (sink, _events) = mpsc::unbounded_channel();
    let resumed = agent.attach(&original, Some(&cwd), sink).await.unwrap();
    assert_eq!(resumed.native_id(), original);
    let recorded = agent.read(&original, None).await.unwrap();
    assert!(recorded.items.iter().any(
        |item| matches!(&item.kind, ItemKind::AgentMessage { text, .. } if text.trim() == "OK")
    ));
    resumed.detach().await;

    let (sink, mut events) = mpsc::unbounded_channel();
    model_options.extend([
        ("approvalPolicy".into(), "untrusted".into()),
        ("sandbox".into(), "read-only".into()),
    ]);
    let session = agent.start(StartRequest {
        cwd,
        content: text("Run `Set-Content -LiteralPath ./approval-marker.txt -Value VERRE_APPROVAL` once, then say OK."),
        options: model_options,
    }, sink).await.unwrap();
    let log = session.wire_log();
    let mut approved = false;
    let mut command_completed = false;
    let end = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match events.recv().await.expect("live session event") {
                SessionEvent::Item(item) => match item.kind {
                    ItemKind::Request(request) if request.resolution.is_none() => {
                        let RequestBody::Permission { options } = request.body else {
                            panic!("Expected command approval");
                        };
                        let matches_marker = request
                            .detail
                            .as_deref()
                            .is_some_and(|detail| detail.contains("approval-marker.txt"))
                            || request.call_id.as_deref().is_some_and(|id| saw_marker_command(&log, id));
                        if request.title != "Approve command execution" || !matches_marker {
                            print_wire_summary(&log);
                            panic!(
                                "Unexpected approval request {:?} (detail present: {}, marker: {matches_marker})",
                                request.title,
                                request.detail.is_some()
                            );
                        }
                        let once = options
                            .iter()
                            .find(|option| option.id == "accept")
                            .expect("server allow-once option");
                        assert!(!approved, "one command approval expected");
                        session
                            .respond(
                                &request.request_id,
                                RequestResolution::Selected {
                                    option_id: once.id.clone(),
                                },
                            )
                            .await
                            .unwrap();
                        approved = true;
                    }
                    ItemKind::ToolCall(tool)
                        if tool.kind == ToolKind::Execute
                            && tool.status == ToolStatus::Completed =>
                    {
                        command_completed = true;
                    }
                    ItemKind::TurnEnd { stop_reason, .. } => break stop_reason,
                    ItemKind::Notice { text, .. } => println!("Live approval notice: {text}"),
                    _ => {}
                },
                SessionEvent::Closed { error } => panic!("Codex closed: {error:?}"),
                _ => {}
            }
        }
    })
    .await;
    print_wire_summary(&log);
    assert_eq!(end.expect("approval turn timed out"), StopReason::EndTurn);
    assert!(
        approved && command_completed,
        "approval={approved}, command completed={command_completed}"
    );
    session.detach().await;
    agent.shutdown().await;
}

fn text(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text { text: text.into() }]
}

fn scratch() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .expect("Verre checkout")
        .join(".orchestra");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let directory = root.join(format!("codex-live-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn cheap_model(options: &[ConfigOption]) -> String {
    let models = options
        .iter()
        .find(|option| option.id == "model")
        .expect("model catalog");
    models
        .choices
        .iter()
        .min_by_key(|choice| {
            let description = choice
                .description
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if description.contains("affordable") || description.contains("cheapest") {
                0
            } else if description.contains("efficient") || description.contains("low-cost") {
                1
            } else {
                2
            }
        })
        .expect("model with catalog metadata")
        .value
        .clone()
}

fn lowest_effort(options: &[ConfigOption]) -> Option<String> {
    let effort = options.iter().find(|option| option.id == "effort")?;
    [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ]
    .iter()
    .find_map(|level| effort.choices.iter().find(|choice| choice.value == *level))
    .or_else(|| effort.choices.first())
    .map(|choice| choice.value.clone())
}

fn saw_marker_command(log: &WireLog, item_id: &str) -> bool {
    log.snapshot().into_iter().rev().any(|entry| {
        entry.direction == RawDirection::Received
            && serde_json::from_str::<Value>(&entry.text).is_ok_and(|value| {
                let item = &value["params"]["item"];
                item["id"] == item_id
                    && item["type"] == "commandExecution"
                    && item["command"]
                        .as_str()
                        .is_some_and(|command| command.contains("approval-marker.txt"))
            })
    })
}

fn print_wire_summary(log: &WireLog) {
    for entry in log.snapshot() {
        let Ok(value) = serde_json::from_str::<Value>(&entry.text) else {
            continue;
        };
        let method = value["method"].as_str().unwrap_or_default();
        let params = &value["params"];
        match (entry.direction, method) {
            (RawDirection::Sent, "initialize" | "initialized" | "thread/list") => {
                println!("S {value}")
            }
            (RawDirection::Sent, "thread/read" | "thread/resume") => {
                println!("S {method} (id omitted)")
            }
            (RawDirection::Sent, "thread/start" | "turn/start") => {
                let mut trimmed = params.clone();
                for key in ["cwd", "threadId", "model"] {
                    if let Some(value) = trimmed.get_mut(key) {
                        *value = Value::String(format!("<{key}>"));
                    }
                }
                println!("S {method} {trimmed}");
            }
            (
                RawDirection::Received,
                "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/permissions/requestApproval",
            ) => {
                let marker = params["command"]
                    .as_str()
                    .is_some_and(|command| command.contains("approval-marker.txt"));
                println!(
                    "R {method} decisions={} marker={marker}",
                    params["availableDecisions"]
                );
            }
            (RawDirection::Sent, "") if value["result"]["decision"].is_string() => {
                println!("S approval response {}", value["result"]);
            }
            (RawDirection::Received, "turn/completed") => {
                println!("R {method} status={}", params["turn"]["status"])
            }
            _ => {}
        }
    }
}
