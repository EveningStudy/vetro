use std::{
    collections::HashMap,
    env, fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use serde_json::Value;
use verre_agent_api::{ItemDraft, SessionRecord, Transcript};
use verre_proto::{ContentBlock, ItemKind, StopReason};

use crate::{mapping, SpawnSpec};

fn sessions_root(spec: &SpawnSpec) -> Option<PathBuf> {
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    spec.env
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(home_key))
        .map(|(_, value)| PathBuf::from(value))
        .or_else(env::home_dir)
        .filter(|path| path.is_absolute())
        .map(|home| home.join(".kiro/sessions"))
}

fn session_id(id: &str) -> bool {
    id.starts_with("sess_")
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

pub(crate) async fn enrich_inventory(spec: &SpawnSpec, sessions: &mut [SessionRecord]) {
    let Some(root) = sessions_root(spec) else {
        return;
    };
    let ids: Vec<_> = sessions
        .iter()
        .filter(|record| session_id(&record.native_id))
        .map(|record| record.native_id.clone())
        .collect();
    if ids.is_empty() {
        return;
    }
    let metadata = tokio::task::spawn_blocking(move || session_metadata(root, ids))
        .await
        .unwrap_or_default();
    for record in sessions {
        if let Some(native) = metadata.get(&record.native_id) {
            record.created_at = record.created_at.or(native.created_at);
            if let Some(updated_at) = native.updated_at.or(native.created_at) {
                record.updated_at = record.updated_at.max(updated_at);
            }
            record.internal |= native.non_interactive;
        }
    }
}

fn workspace_folders(root: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
}

fn session_file(folder: &Path, id: &str, name: &str) -> Option<PathBuf> {
    let path = folder.join(id);
    if !path.symlink_metadata().ok()?.is_dir() {
        return None;
    }
    let path = path.join(name);
    path.symlink_metadata().ok()?.is_file().then_some(path)
}

#[derive(PartialEq, Eq)]
struct SessionMetadata {
    created_at: Option<i64>,
    updated_at: Option<i64>,
    non_interactive: bool,
}

fn session_metadata(root: PathBuf, ids: Vec<String>) -> HashMap<String, SessionMetadata> {
    let folders: Vec<_> = workspace_folders(&root).collect();
    ids.into_iter()
        .filter_map(|id| {
            let mut matches = folders.iter().filter_map(|folder| {
                let path = session_file(folder, &id, "session.json")?;
                let file = fs::File::open(path).ok()?;
                let value: Value = serde_json::from_reader(file.take(64 * 1024)).ok()?;
                if value["id"].as_str() != Some(&id) {
                    return None;
                }
                Some(SessionMetadata {
                    created_at: mapping::timestamp(&value["createdAt"]),
                    updated_at: mapping::timestamp(&value["lastModifiedAt"]),
                    non_interactive: value["_meta"]["kiro"]["nonInteractive"] == true,
                })
            });
            let metadata = matches.next()?;
            matches
                .all(|other| other == metadata)
                .then_some((id, metadata))
        })
        .collect()
}

pub(crate) async fn enrich_replay(spec: &SpawnSpec, id: &str, transcript: &mut Transcript) {
    let Some(root) = sessions_root(spec).filter(|_| session_id(id)) else {
        return;
    };
    let id = id.to_owned();
    let turns = tokio::task::spawn_blocking(move || {
        let mut logs = workspace_folders(&root)
            .filter_map(|folder| session_file(&folder, &id, "messages.jsonl"));
        let log = logs.next()?;
        if logs.next().is_some() {
            return None;
        }
        read_turns(BufReader::new(fs::File::open(log).ok()?))
    })
    .await
    .ok()
    .flatten();
    if let Some(turns) = turns {
        reconcile_turns(transcript, &turns);
    }
}

struct NativeTurn {
    prompt: String,
    started_at: Option<i64>,
    ended: Option<(StopReason, Option<i64>)>,
    duration_ms: Option<u64>,
}

fn read_turns(reader: impl BufRead) -> Option<Vec<NativeTurn>> {
    let mut prompts = HashMap::new();
    let mut indices = HashMap::new();
    let mut turns: Vec<NativeTurn> = Vec::new();
    for line in reader.lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            verre_agent_api::drift::report("kiro", "messages.jsonl", Some("invalid-json"));
            return None;
        };
        let payload = &event["payload"];
        match payload["type"].as_str() {
            Some("user") => {
                if let (Some(tag), Some(text)) = (
                    payload["_meta"]["kiro"]["userMessageTag"].as_str(),
                    payload["content"].as_str(),
                ) {
                    prompts.insert(tag.to_owned(), text.to_owned());
                }
            }
            Some("turn_start") => {
                let id = payload["turnId"].as_str()?;
                let tag = payload["userMessageTag"].as_str()?;
                let prompt = prompts.remove(tag)?;
                if indices.insert(id.to_owned(), turns.len()).is_some() {
                    return None;
                }
                turns.push(NativeTurn {
                    prompt,
                    started_at: mapping::timestamp(&event["timestamp"]),
                    ended: None,
                    duration_ms: None,
                });
            }
            Some("turn_end" | "usage_summary") => {
                let Some(&index) = payload["turnId"].as_str().and_then(|id| indices.get(id)) else {
                    continue;
                };
                let turn = &mut turns[index];
                if payload["type"] == "usage_summary" {
                    turn.duration_ms = payload["elapsedTime"].as_u64();
                } else {
                    let reason = match payload["stopReason"].as_str() {
                        Some("end_turn") => StopReason::EndTurn,
                        Some("cancelled") => StopReason::Cancelled,
                        Some("max_tokens") => StopReason::MaxTokens,
                        Some("refusal") => StopReason::Refusal,
                        Some("error") => StopReason::Error,
                        reason => {
                            verre_agent_api::drift::report("kiro", "turn_end stopReason", reason);
                            continue;
                        }
                    };
                    turn.ended = Some((reason, mapping::timestamp(&event["timestamp"])));
                }
            }
            _ => {}
        }
    }
    Some(turns)
}

fn reconcile_turns(transcript: &mut Transcript, turns: &[NativeTurn]) {
    let replay_turns = transcript.items.iter().filter_map(|item| match &item.kind {
        ItemKind::UserMessage { content } => Some((
            item.turn_id.as_deref(),
            content
                .iter()
                .filter_map(|part| match part {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        _ => None,
    });
    let replay_turns: Vec<_> = replay_turns.collect();
    if turns.len() != replay_turns.len()
        || !turns
            .iter()
            .zip(replay_turns.iter())
            .all(|(turn, (id, text))| {
                id.is_some() && turn.prompt.replace("\r\n", "\n") == text.replace("\r\n", "\n")
            })
    {
        return;
    }

    let mut native_ends = turns
        .iter()
        .zip(replay_turns)
        .map(|(turn, (id, _))| {
            let (reason, ts) = turn.ended?;
            let duration = turn
                .duration_ms
                .or_else(|| u64::try_from(ts?.checked_sub(turn.started_at?)?).ok());
            Some(ItemDraft::turn_end(id?, reason, ts, duration, None))
        })
        .collect::<Vec<_>>()
        .into_iter();
    let mut items = Vec::with_capacity(transcript.items.len());
    let mut end = None;
    for item in std::mem::take(&mut transcript.items) {
        if matches!(item.kind, ItemKind::UserMessage { .. }) {
            items.extend(end.take());
            end = native_ends.next().flatten();
        }
        if !matches!(item.kind, ItemKind::TurnEnd { .. }) {
            items.push(item);
        }
    }
    items.extend(end);
    transcript.items = items;
}
