use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use verre_agent_acp::{AcpAgent, SpawnSpec};
use verre_agent_api::Agent;
use verre_proto::{ItemKind, ThreadStatus, ToolStatus};

use crate::{home::Home, registry::antigravity::Antigravity, storage::Filter};

use super::*;

mod capture;

type Outcome<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Expectations {
    created_in_verre: BTreeSet<String>,
    threads: BTreeMap<String, Expected>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    case: String,
    #[serde(default)]
    absent: bool,
    #[serde(default)]
    checks: BTreeMap<String, Value>,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_owned()
}

#[test]
#[ignore = "reads private local agent stores and writes a gitignored corpus"]
fn capture_native_corpus() -> Outcome<()> {
    capture::run(&repo())
}

#[tokio::test]
#[ignore = "requires a locally captured private corpus"]
async fn check_native_corpus() -> Outcome<()> {
    let root = repo().join(".orchestra/corpus");
    if fs::canonicalize(&root)? != fs::canonicalize(repo())?.join(".orchestra/corpus") {
        return Err("Corpus directory redirects outside its expected location".into());
    }
    let expectations: Expectations =
        serde_json::from_reader(fs::File::open(root.join("expectations.json"))?)?;
    assert!(
        !expectations.threads.is_empty(),
        "Corpus has no expectations"
    );
    let temp = tempfile::Builder::new()
        .prefix("corpus-verre-home-")
        .tempdir_in(&root)?;
    let home = Home::open(temp.path().to_owned(), root.clone())?;
    home.change_state(|state| {
        for id in &expectations.created_in_verre {
            state.threads.entry(id.clone()).or_default().created_here = true;
        }
    })?;

    let python_name = if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    };
    let python = std::env::var_os("VERRE_CORPUS_PYTHON")
        .map(PathBuf::from)
        .or_else(|| {
            verre_agent_api::detection::search_directories()
                .into_iter()
                .filter(|directory| !directory.to_string_lossy().contains("WindowsApps"))
                .map(|directory| directory.join(python_name))
                .find(|path| path.is_file())
        })
        .ok_or(
            "Python is required for the offline ACP replay endpoint (set VERRE_CORPUS_PYTHON)",
        )?;
    let fixture = repo().join("crates/verre-core/tests/fixtures/corpus_acp.py");
    let acp = |id: &str, data: &Path, env: Vec<_>| {
        let mut spec = SpawnSpec::new(&python);
        spec.cwd = Some(temp.path().to_owned());
        spec.args = vec![
            fixture.clone().into_os_string(),
            data.to_owned().into_os_string(),
        ];
        spec.env = env.into_iter().collect();
        AcpAgent::new(id, id, spec)
    };
    let claude = root.join("claude/projects");
    let antigravity = root.join("antigravity");
    let kiro = root.join("kiro");
    let devin = root.join("devin");
    let user_home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let agents: Vec<Arc<dyn Agent>> = vec![
        Arc::new(verre_agent_claude::ClaudeAgent::from_spec(None, &claude)),
        Arc::new(verre_agent_codex::CodexAgent::from_spec(None)),
        Arc::new(acp(
            "devin",
            &devin.join("replay.json"),
            vec![("APPDATA".into(), devin.clone().into_os_string())],
        )),
        Arc::new(Antigravity::new(
            acp("antigravity", &antigravity.join("replay.json"), vec![]),
            antigravity.clone(),
        )),
        Arc::new(acp(
            "kiro",
            &kiro.join("replay.json"),
            vec![(user_home_key.into(), kiro.into_os_string())],
        )),
    ];
    let replay_core = if root.join("acp/replay.json").is_file() {
        let endpoint: Arc<dyn Agent> = Arc::new(acp("acp", &root.join("acp/replay.json"), vec![]));
        let home = Home::open(temp.path().join("replay"), root.clone())?;
        Some(offline_core(home, Registry::new([endpoint])?))
    } else {
        None
    };
    let registry = Registry::new(agents)?
        .watch_history("claude", vec![claude])
        .watch_history("codex", vec![root.join("codex")])
        .watch_history(
            "antigravity",
            vec![
                antigravity.join(".gemini/antigravity"),
                antigravity.join(".gemini/antigravity-cli"),
            ],
        );
    let core = offline_core(home, registry);
    core.refresh_history().await;
    if let Some(replay) = &replay_core {
        replay.refresh_history().await;
    }
    let mut root_ids = HashSet::new();
    let mut indexed = HashSet::new();
    for instance in std::iter::once(&core).chain(replay_core.iter()) {
        let roots = instance.store.list(&Filter::default(), 10_000, "{}")?;
        root_ids.extend(roots.into_iter().map(|row| row.id));
        for agent in instance.registry().all() {
            indexed.extend(instance.store.marks(agent.id())?.into_iter().filter_map(
                |(id, listed, saved)| (listed.is_some() && listed == saved).then_some(id),
            ));
        }
    }
    let mut failures = 0;
    let mut rows: Vec<_> = expectations.threads.iter().collect();
    rows.sort_unstable_by(|left, right| left.1.case.cmp(&right.1.case));
    println!("| Case | Result | Checks |");
    println!("| --- | --- | --- |");
    for (id, expectation) in rows {
        let core = if id.starts_with("acp:") {
            replay_core
                .as_ref()
                .ok_or("Recorded ACP catalog is missing")?
        } else {
            &core
        };
        let summary = core.store.get(id)?;
        if expectation.absent {
            assert!(
                expectation.checks.is_empty(),
                "Absent cases cannot have indexed field checks"
            );
            let (status, detail) = if summary.is_none() {
                ("PASS", "absent from index")
            } else {
                failures += 1;
                ("FAIL", "present in index")
            };
            println!("| {} | {status} | {detail} |", expectation.case);
            continue;
        }
        let Some(summary) = summary else {
            println!("| {} | FAIL | missing from index |", expectation.case);
            failures += 1;
            continue;
        };
        let parent = summary.parent.as_ref();
        let mut actual = json!({
            "source": summary.source,
            "internal": summary.internal,
            "createdAt": summary.created_at,
            "activityTime": summary.updated_at,
            "parent": parent.map(|parent| &parent.thread_id),
            "parentHasToolCall": parent.is_some_and(|parent| parent.call_id.is_some()),
            "parentToolCallId": parent.and_then(|parent| parent.call_id.as_ref()),
            "parentConfidence": parent.map(|parent| parent.confidence),
            "defaultRoot": root_ids.contains(id),
            "indexed": indexed.contains(id),
        });
        if expectation.checks.contains_key("copyPerAnswer")
            || expectation.checks.contains_key("hasCopyableAnswer")
            || expectation.checks.contains_key("nativeTurnDurations")
        {
            let agent = core.registry().get(&summary.agent_id)?;
            if let Ok(history) = core.read_history_items(&agent, &summary).await {
                let answers =
                    copy_per_answer(&history.items, summary.status == ThreadStatus::Running);
                actual["hasCopyableAnswer"] = json!(answers.contains(&true));
                actual["copyPerAnswer"] = json!(answers);
                actual["nativeTurnDurations"] = json!(history
                    .items
                    .iter()
                    .filter_map(|item| {
                        match &item.kind {
                            ItemKind::TurnEnd { duration_ms, .. } => Some(*duration_ms),
                            _ => None,
                        }
                    })
                    .collect::<Vec<_>>());
            }
        }
        if expectation.checks.contains_key("parentToolListsChild") {
            let mut listed = false;
            if let Some(parent) = parent {
                if let Ok(open) = core.open_thread(&parent.thread_id, Some(u32::MAX)).await {
                    listed = open.items.iter().any(|item| {
                        matches!(&item.kind, ItemKind::ToolCall(tool)
                            if parent.call_id.as_deref() == Some(&tool.call_id)
                                && tool.child_thread_ids.contains(id))
                    });
                }
            }
            actual["parentToolListsChild"] = json!(listed);
        }
        let differences: Vec<_> = expectation
            .checks
            .iter()
            .filter_map(|(field, expected)| {
                (actual.get(field) != Some(expected)).then_some(field.as_str())
            })
            .collect();
        let (status, detail) = if differences.is_empty() {
            (
                "PASS",
                expectation
                    .checks
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        } else {
            failures += 1;
            ("FAIL", format!("differs on {}", differences.join(", ")))
        };
        println!("| {} | {status} | {detail} |", expectation.case);
    }
    core.shutdown().await;
    if let Some(replay) = replay_core {
        replay.shutdown().await;
    }
    assert_eq!(
        failures, 0,
        "{failures} corpus cases differ from expectations"
    );
    Ok(())
}

fn offline_core(home: Home, registry: Registry) -> Arc<Core> {
    let mut core = Core::new(home, registry);
    Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().store)
        .unwrap()
        .suppress_workspace_reads();
    core
}

fn copy_per_answer(items: &[verre_agent_api::ItemDraft], running: bool) -> Vec<bool> {
    let mut answers = Vec::new();
    let mut has_answer = false;
    let mut unsettled = false;
    for item in items {
        match &item.kind {
            ItemKind::UserMessage { .. } | ItemKind::TurnEnd { .. } => {
                if has_answer {
                    answers.push(matches!(&item.kind, ItemKind::TurnEnd { .. }) || !unsettled);
                }
                has_answer = false;
                unsettled = false;
            }
            ItemKind::AgentMessage { text, streaming } => {
                unsettled |= *streaming;
                has_answer |= !streaming && !text.trim().is_empty();
            }
            ItemKind::Reasoning { streaming, .. } => unsettled |= *streaming,
            ItemKind::ToolCall(tool) => {
                unsettled |= matches!(tool.status, ToolStatus::Pending | ToolStatus::InProgress);
            }
            ItemKind::Request(request) => unsettled |= request.resolution.is_none(),
            _ => {}
        }
    }
    if has_answer {
        answers.push(!unsettled && !running);
    }
    answers
}
