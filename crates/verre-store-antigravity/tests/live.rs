//! Reads the real Antigravity stores of the machine. Prints counts and timings only, never
//! conversation content. Run on purpose:
//! `cargo test -p verre-store-antigravity --release --test live -- --ignored --nocapture`

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Instant, UNIX_EPOCH},
};

use verre_proto::{ItemKind, ToolContent, ToolKind};
use verre_store_antigravity::{sqlite::open_readonly, steps::UNKNOWN_STEP_PREFIX, Store};

fn gemini_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .expect("home directory");
    PathBuf::from(home).join(".gemini")
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, u128)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                let t = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                out.insert(e.path(), (m.len(), t));
            }
        }
    }
    out
}

fn kind_label(kind: &ItemKind) -> String {
    match kind {
        ItemKind::UserMessage { .. } => "user_message".into(),
        ItemKind::AgentMessage { .. } => "agent_message".into(),
        ItemKind::Reasoning { .. } => "reasoning".into(),
        ItemKind::ToolCall(t)
            if t.name
                .as_deref()
                .is_some_and(|n| n.starts_with(UNKNOWN_STEP_PREFIX)) =>
        {
            format!("unknown {}", t.name.as_deref().unwrap_or_default())
        }
        ItemKind::ToolCall(t) => format!("tool {:?} {}", t.kind, t.name.as_deref().unwrap_or("?")),
        ItemKind::Plan { .. } => "plan".into(),
        ItemKind::Request(_) => "request".into(),
        ItemKind::Notice { level, .. } => format!("notice {level:?}"),
        ItemKind::TurnEnd { .. } => "turn_end".into(),
    }
}

#[test]
#[ignore = "reads the real Antigravity stores on this machine"]
fn reads_real_stores() {
    let gemini = gemini_dir();
    let before = snapshot(&gemini);
    let started = std::time::SystemTime::now();

    let mut step_types: BTreeMap<i64, usize> = BTreeMap::new();
    let mut items_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_steps = 0usize;
    let mut total_items = 0usize;
    for name in ["antigravity", "antigravity-cli"] {
        let store = Store::open(gemini.join(name));
        let t = Instant::now();
        let list = store.list().expect("list");
        let list_ms = t.elapsed().as_millis();
        let mut sources: BTreeMap<String, usize> = BTreeMap::new();
        for r in &list {
            *sources
                .entry(format!(
                    "{:?} / {}",
                    r.source,
                    r.origin.as_deref().unwrap_or("-")
                ))
                .or_default() += 1;
        }
        println!(
            "{name}: {} conversations listed in {list_ms} ms; with cwd {}, with parent {}, sources {sources:?}",
            list.len(),
            list.iter().filter(|r| r.cwd.is_some()).count(),
            list.iter().filter(|r| r.parent_native_id.is_some()).count(),
        );

        let mut sized: Vec<(u64, String)> = list
            .iter()
            .map(|r| {
                let path = store
                    .root()
                    .join("conversations")
                    .join(format!("{}.db", r.native_id));
                (
                    fs::metadata(&path).map_or(0, |m| m.len()),
                    r.native_id.clone(),
                )
            })
            .collect();
        sized.sort();
        let t_all = Instant::now();
        for (size, id) in sized.iter().rev() {
            let path = store.root().join("conversations").join(format!("{id}.db"));
            let conn = open_readonly(&path).expect("open");
            let mut stmt = conn
                .prepare("SELECT step_type, count(*) FROM steps GROUP BY step_type")
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize))
                })
                .unwrap();
            let mut steps = 0;
            for row in rows {
                let (k, n) = row.unwrap();
                *step_types.entry(k).or_default() += n;
                steps += n;
            }
            drop(stmt);
            drop(conn);

            let t = Instant::now();
            let items = store.read(id).expect("read").items;
            let ms = t.elapsed().as_millis();
            if *size == sized.last().unwrap().0 || *size > 20_000_000 {
                println!(
                    "  read {:>6.1} MB, {steps:>5} steps -> {:>5} items in {ms} ms",
                    *size as f64 / 1e6,
                    items.len()
                );
            }
            assert!(steps == 0 || !items.is_empty());
            for item in &items {
                *items_by_kind.entry(kind_label(&item.kind)).or_default() += 1;
                if let ItemKind::ToolCall(t) = &item.kind {
                    assert!(!t.title.is_empty());
                    *items_by_kind
                        .entry(format!("~status {:?}", t.status))
                        .or_default() += 1;
                    for c in &t.content {
                        let label = match c {
                            ToolContent::Text { text } if text.ends_with("not shown]") => {
                                "~content image omitted"
                            }
                            ToolContent::Text { .. } => "~content text",
                            ToolContent::Diff {
                                unified: Some(_), ..
                            } => "~content diff unified",
                            ToolContent::Diff { .. } => "~content diff old/new",
                            ToolContent::Terminal {
                                exit_code: Some(_), ..
                            } => "~content terminal with exit code",
                            ToolContent::Terminal { .. } => "~content terminal",
                            ToolContent::Image { .. } => "~content image",
                        };
                        *items_by_kind.entry(label.into()).or_default() += 1;
                    }
                    if t.content.is_empty() {
                        *items_by_kind.entry("~content none".into()).or_default() += 1;
                    }
                    if t.kind == ToolKind::Agent && t.name.as_deref() == Some("invoke_subagent") {
                        *items_by_kind
                            .entry("invoke_subagent with child id".into())
                            .or_default() += usize::from(!t.child_thread_ids.is_empty());
                    }
                }
            }
            total_steps += steps;
            total_items += items.len();
        }
        println!(
            "{name}: read {} conversations in {} ms",
            sized.len(),
            t_all.elapsed().as_millis()
        );
    }
    println!("total steps {total_steps}, items {total_items}");
    println!("steps per step_type: {step_types:?}");
    for (k, n) in &items_by_kind {
        println!("  {n:>6} {k}");
    }

    let after = snapshot(&gemini);
    let mut changed = Vec::new();
    for (path, meta) in &after {
        match before.get(path) {
            None => changed.push(format!("created {}", path.display())),
            Some(old) if old != meta => changed.push(format!("modified {}", path.display())),
            _ => {}
        }
    }
    for path in before.keys().filter(|p| !after.contains_key(*p)) {
        changed.push(format!("removed {}", path.display()));
    }
    println!(
        "files under {}: {} before, {} after, {} changed during the run (started {:?})",
        gemini.display(),
        before.len(),
        after.len(),
        changed.len(),
        started.duration_since(UNIX_EPOCH).unwrap().as_secs()
    );
    for c in &changed {
        println!("  {c}");
    }
}
