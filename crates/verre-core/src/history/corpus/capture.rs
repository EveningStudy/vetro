use std::{
    env,
    fs::{File, FileTimes},
    io::{BufRead, BufReader, Read, Write},
    time::SystemTime,
};

use rusqlite::{
    params_from_iter,
    types::{ToSqlOutput, ValueRef},
    Connection,
};
use verre_store_antigravity::sqlite::open_readonly;

use super::*;

const MAX_FILE: u64 = 20 * 1024 * 1024;
const MAX_ROWS: usize = 20_000;
const MAX_FILES_PER_SESSION: usize = 8;
const RECENT: usize = 4;

pub(super) fn run(repo: &Path) -> Outcome<()> {
    let scratch = repo.join(".orchestra");
    fs::create_dir_all(&scratch)?;
    if fs::canonicalize(&scratch)? != fs::canonicalize(repo)?.join(".orchestra") {
        return Err("Corpus directory redirects outside the repository".into());
    }
    let final_path = scratch.join("corpus");
    fs::create_dir_all(&final_path)?;
    if fs::canonicalize(&final_path)? != fs::canonicalize(&scratch)?.join("corpus") {
        return Err("Corpus directory redirects outside its expected location".into());
    }
    for name in [
        "expectations.json",
        "claude",
        "codex",
        "devin",
        "antigravity",
        "kiro",
        "acp",
    ] {
        if final_path.join(name).symlink_metadata().is_ok() {
            return Err("Move the existing corpus entries before recapture".into());
        }
    }
    let stage = tempfile::Builder::new()
        .prefix(".capture-")
        .tempdir_in(&final_path)?;
    let root = stage.path();
    let home = env::home_dir().ok_or("Home directory is unavailable")?;
    let verre = env::var_os("VERRE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".verre"));
    let mut expected = Expectations::default();
    let origins = known_origins(&verre.join("index.db"))?;
    let claude_home = env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    capture_claude(&claude_home.join("projects"), root, &origins, &mut expected)?;
    let codex_home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let delegation = if let Some(appdata) = env::var_os("APPDATA") {
        capture_devin(&PathBuf::from(appdata), root, &mut expected)?;
        read_devin_delegation(root, &expected)?
    } else {
        None
    };
    capture_codex(
        &codex_home,
        root,
        &origins,
        delegation.as_ref(),
        &mut expected,
    )?;
    capture_antigravity(&home, root, &mut expected)?;
    capture_kiro(&home, root, delegation.as_ref(), &mut expected)?;
    capture_wire(&verre.join("raw"), root, &mut expected)?;
    println!("| Source | Captured cases |");
    println!("| --- | ---: |");
    for agent in ["claude", "codex", "devin", "antigravity", "kiro", "acp"] {
        let count = expected
            .threads
            .keys()
            .filter(|id| id.split_once(':').is_some_and(|(key, _)| key == agent))
            .count();
        println!("| {agent} | {count} |");
    }
    if expected.threads.is_empty() {
        return Err("No native session samples found".into());
    }
    write_json(&root.join("expectations.json"), &expected)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name() != "expectations.json" {
            fs::rename(entry.path(), final_path.join(entry.file_name()))?;
        }
    }
    fs::rename(
        root.join("expectations.json"),
        final_path.join("expectations.json"),
    )?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Outcome<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let mut output = File::create(path)?;
    serde_json::to_writer_pretty(&mut output, value)?;
    writeln!(output)?;
    Ok(())
}

fn known_origins(path: &Path) -> Outcome<BTreeSet<String>> {
    if !path.is_file() {
        return Ok(BTreeSet::new());
    }
    let db = open_readonly(path)?;
    let mut statement = db.prepare("SELECT id FROM threads WHERE source='verre'")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn files(root: &Path, depth: usize) -> Vec<PathBuf> {
    if !root
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Vec::new();
    }
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_file() => vec![path],
                Ok(kind) if kind.is_dir() && depth > 1 => files(&path, depth - 1),
                _ => Vec::new(),
            }
        })
        .collect()
}

fn order_recent(paths: &mut [PathBuf]) {
    paths.sort_by_cached_key(|path| {
        std::cmp::Reverse((
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok(),
            path.clone(),
        ))
    });
}

fn extension(path: &Path, value: &str) -> bool {
    path.extension().is_some_and(|extension| extension == value)
}

fn bounded(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= MAX_FILE)
}

fn copy(source: &Path, destination: &Path) -> Outcome<()> {
    fs::create_dir_all(destination.parent().unwrap())?;
    let mut output = File::create(destination)?;
    let input = File::open(source)?;
    let metadata = input.metadata()?;
    let bytes = std::io::copy(
        &mut input.take(metadata.len().min(MAX_FILE + 1)),
        &mut output,
    )?;
    if bytes > MAX_FILE {
        return Err("Sample exceeded the per-file limit".into());
    }
    output.set_times(FileTimes::new().set_modified(metadata.modified()?))?;
    Ok(())
}

fn json_rows(path: &Path) -> Outcome<impl Iterator<Item = Value>> {
    Ok(BufReader::new(File::open(path)?.take(MAX_FILE))
        .split(b'\n')
        .map_while(std::result::Result::ok)
        .filter_map(|line| serde_json::from_slice(&line).ok()))
}

fn time(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let time = humantime::parse_rfc3339(value.as_str()?).ok()?;
        i64::try_from(
            time.duration_since(SystemTime::UNIX_EPOCH)
                .ok()?
                .as_millis(),
        )
        .ok()
    })
}

fn fields(value: Value) -> BTreeMap<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn add(expected: &mut Expectations, id: String, case: String, checks: Value) {
    expected.threads.insert(
        id,
        Expected {
            case,
            absent: false,
            checks: fields(checks),
        },
    );
}

fn capture_claude(
    source: &Path,
    root: &Path,
    origins: &BTreeSet<String>,
    expected: &mut Expectations,
) -> Outcome<()> {
    let mut paths = files(source, 2);
    paths.retain(|path| extension(path, "jsonl") && bounded(path));
    order_recent(&mut paths);
    let mut counts = BTreeMap::new();
    for path in paths.into_iter().take(512) {
        let Some(id) = path
            .file_stem()
            .and_then(|id| id.to_str())
            .filter(|id| id.len() == 36)
        else {
            continue;
        };
        let key = format!("claude:{id}");
        let mut origin = None;
        let mut latest = None;
        let mut tools_after = false;
        let mut derived_after = false;
        for row in json_rows(&path)? {
            if let Some(timestamp) = time(&row["timestamp"]) {
                latest = latest.max(Some(timestamp));
                tools_after = false;
                derived_after = false;
            }
            match row["type"].as_str() {
                Some("tools-used") => tools_after = true,
                Some("derived-from") => derived_after = true,
                Some("user" | "assistant") if row["isSidechain"] != true && origin.is_none() => {
                    origin = row["entrypoint"].as_str().map(str::to_owned);
                }
                _ => {}
            }
        }
        let (label, thread_source, internal) = match origin.as_deref() {
            Some("sdk-cli") if origins.contains(&key) => ("Verre sdk-cli", "verre", false),
            Some("sdk-cli") => ("native sdk-cli", "exec", true),
            Some("claude-desktop") if tools_after && derived_after => {
                ("desktop trailing tool and lineage rows", "desktop", false)
            }
            Some("claude-desktop") if tools_after => {
                ("desktop trailing tool rows", "desktop", false)
            }
            _ => continue,
        };
        let Some(latest) = latest else { continue };
        let count = counts.entry(label).or_insert(0);
        if *count >= 2 {
            continue;
        }
        *count += 1;
        let destination = root
            .join("claude/projects")
            .join(path.strip_prefix(source)?);
        copy(&path, &destination)?;
        let session = path.with_extension("");
        for sidechain in files(&session.join("subagents"), 1)
            .into_iter()
            .filter(|path| extension(path, "jsonl") && bounded(path))
            .take(MAX_FILES_PER_SESSION)
        {
            copy(
                &sidechain,
                &destination
                    .with_extension("")
                    .join(sidechain.strip_prefix(&session)?),
            )?;
        }
        let mut checks = json!({"source":thread_source,"internal":internal,"activityTime":latest,"indexed":true});
        if thread_source == "desktop" || thread_source == "verre" {
            checks["parent"] = Value::Null;
            checks["defaultRoot"] = json!(true);
        }
        if thread_source == "verre" {
            expected.created_in_verre.insert(key.clone());
        }
        add(expected, key, format!("Claude {label} {count}"), checks);
    }
    Ok(())
}

fn newest_db(root: &Path, prefix: &str) -> Option<PathBuf> {
    files(root, 1)
        .into_iter()
        .filter_map(|path| {
            let version = path
                .file_name()?
                .to_str()?
                .strip_prefix(prefix)?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            Some((version, path))
        })
        .max()
        .map(|(_, path)| path)
}

fn capture_codex(
    source: &Path,
    root: &Path,
    origins: &BTreeSet<String>,
    delegation: Option<&Delegation>,
    expected: &mut Expectations,
) -> Outcome<()> {
    let Some(path) = newest_db(source, "state_") else {
        return Ok(());
    };
    let source_db = open_readonly(&path)?;
    let input = source_db.unchecked_transaction()?;
    let output_root = root.join("codex");
    fs::create_dir_all(&output_root)?;
    let output = Connection::open(output_root.join(path.file_name().unwrap()))?;
    let mut rollouts: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for folder in ["sessions", "archived_sessions"] {
        for rollout in files(&source.join(folder), 5) {
            let Some(id) = rollout
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("rollout-")?.get(20..56))
            else {
                continue;
            };
            rollouts.entry(id.to_owned()).or_default().push(rollout);
        }
    }
    let mut query = input.prepare("SELECT id, source, originator, updated_at, archived, created_at FROM threads ORDER BY updated_at DESC LIMIT 512")?;
    let mut rows = query.query([])?;
    let mut counts = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let raw: String = row.get(1)?;
        let origin: String = row.get(2)?;
        let timestamp: i64 = row.get(3)?;
        let archived: bool = row.get(4)?;
        let created: i64 = row.get(5)?;
        let source_label = raw.trim_matches('"');
        let key = format!("codex:{id}");
        let launch = delegation.and_then(|delegation| delegation.without_printed_id(&id, created));
        let (label, internal) = match (source_label, origin.to_ascii_lowercase().as_str()) {
            ("exec", _) if launch.is_some() && !origins.contains(&key) => {
                ("background exec without printed ID", true)
            }
            ("exec", _) if !origins.contains(&key) => ("exec rollout", true),
            ("cli", _) => ("interactive rollout", false),
            ("chatgpt" | "cloud", _) => ("cloud stub", false),
            _ => continue,
        };
        let count = counts.entry(label).or_insert(0);
        if *count >= RECENT {
            continue;
        }
        let parts = rollouts.get(&id);
        if label != "cloud stub"
            && parts.is_none_or(|paths| {
                paths.len() > MAX_FILES_PER_SESSION || paths.iter().any(|path| !bounded(path))
            })
        {
            continue;
        }
        *count += 1;
        copy_table(
            &input,
            &output,
            "threads",
            "SELECT * FROM threads WHERE id=?1",
            &[&id],
        )?;
        output.execute("UPDATE threads SET rollout_path='' WHERE id=?1", [&id])?;
        if label == "cloud stub" {
            expected.threads.insert(
                key,
                Expected {
                    case: format!("Codex {label} {count}"),
                    absent: true,
                    checks: BTreeMap::new(),
                },
            );
            continue;
        }
        for part in parts.into_iter().flatten() {
            copy(part, &output_root.join(part.strip_prefix(source)?))?;
        }
        let updated = input
            .query_row(
                "SELECT updated_at_ms FROM threads WHERE id=?1",
                [&id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .unwrap_or(timestamp * 1000);
        let mut checks = json!({"source":source_label,"internal":internal,"activityTime":updated,"indexed":true});
        if let (Some(delegation), Some(launch)) = (delegation, launch) {
            if label == "background exec without printed ID" {
                expect_parent(&mut checks, delegation, launch, "heuristic");
            }
        }
        let mut durations = Vec::new();
        for part in parts
            .into_iter()
            .flatten()
            .filter(|path| extension(path, "jsonl"))
        {
            for row in json_rows(part)? {
                let payload = &row["payload"];
                if row["type"] == "event_msg" && payload["type"] == "task_complete" {
                    durations.push(payload["duration_ms"].as_u64());
                    if payload["last_agent_message"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
                    {
                        checks["hasCopyableAnswer"] = json!(true);
                    }
                }
            }
        }
        if !durations.is_empty() {
            checks["nativeTurnDurations"] = json!(durations);
        }
        if origins.contains(&key) {
            expected.created_in_verre.insert(key.clone());
            checks["source"] = json!("verre");
        }
        if internal {
            checks["defaultRoot"] = json!(false);
        } else if !archived {
            checks["defaultRoot"] = json!(true);
        }
        add(expected, key, format!("Codex {label} {count}"), checks);
    }
    Ok(())
}

fn capture_devin(appdata: &Path, root: &Path, expected: &mut Expectations) -> Outcome<()> {
    let path = appdata.join("Devin/cli/sessions.db");
    if !path.is_file() {
        return Ok(());
    }
    let input = open_readonly(&path)?;
    let snapshot = input.unchecked_transaction()?;
    let destination = root.join("devin/Devin/cli/sessions.db");
    fs::create_dir_all(destination.parent().unwrap())?;
    let output = Connection::open(destination)?;
    let pinned = env::var("VERRE_CORPUS_DEVIN_SESSION").ok();
    if let Some(id) = &pinned {
        let found = snapshot.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND hidden=0)",
            [id],
            |row| row.get::<_, bool>(0),
        )?;
        if !found {
            return Err("Requested Devin session is missing from the visible inventory".into());
        }
    }
    let mut query = snapshot.prepare(
        "SELECT id, last_activity_at FROM sessions WHERE hidden=0
         ORDER BY CASE WHEN id=?1 THEN 0 ELSE 1 END, last_activity_at DESC LIMIT ?2",
    )?;
    let mut rows = query.query(rusqlite::params![pinned, RECENT as i64])?;
    let mut count = 0;
    let mut used_receipt_case = false;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let time: i64 = row.get(1)?;
        let receipt_case = match &pinned {
            Some(pinned) => pinned == &id,
            None if !used_receipt_case => snapshot.query_row(
                "SELECT EXISTS(SELECT 1 FROM message_nodes WHERE session_id=?1
                     AND chat_message LIKE '%Command running in background with ID:%')
                     AND EXISTS(SELECT 1 FROM message_nodes WHERE session_id=?1
                     AND chat_message LIKE '%\"get_output\"%')",
                [&id],
                |row| row.get::<_, bool>(0),
            )?,
            None => false,
        };
        used_receipt_case |= receipt_case;
        count += 1;
        for (table, select) in [
            ("sessions", "SELECT id,title,working_directory,created_at,last_activity_at,model,hidden,main_chain_id FROM sessions WHERE id=?1"),
            ("message_nodes", "SELECT node_id,parent_node_id,session_id,created_at,chat_message FROM message_nodes WHERE session_id=?1"),
            ("tool_call_state", "SELECT session_id,tool_call_id,tool_call_json,tool_call_update_json FROM tool_call_state WHERE session_id=?1"),
        ] {
            copy_table(&snapshot, &output, table, select, &[&id])?;
        }
        add(
            expected,
            format!("devin:{id}"),
            if receipt_case {
                "Devin background exec and receipt".into()
            } else {
                format!("Devin native chain {count}")
            },
            json!({
                "indexed":true,"activityTime":time*1000
            }),
        );
    }
    Ok(())
}

struct Delegation {
    parent: String,
    launches: BTreeMap<String, Launch>,
}

struct Launch {
    call_id: String,
    seconds: i64,
    output: String,
    receipts: Vec<String>,
}

impl Delegation {
    fn without_printed_id(&self, child: &str, created: i64) -> Option<&Launch> {
        let mut matches = self.launches.values().filter(|launch| {
            (launch.seconds..=launch.seconds + 3).contains(&created)
                && !launch.output.contains(child)
                && launch
                    .receipts
                    .iter()
                    .all(|receipt| !receipt.contains(child))
        });
        let launch = matches.next()?;
        matches.next().is_none().then_some(launch)
    }

    fn with_receipt(&self, child: &str) -> Option<&Launch> {
        let marker = format!("session_id={child}");
        let mut matches = self.launches.values().filter(|launch| {
            launch
                .receipts
                .iter()
                .any(|text| text.lines().any(|line| line.trim() == marker))
        });
        let launch = matches.next()?;
        matches.next().is_none().then_some(launch)
    }
}

fn read_devin_delegation(root: &Path, expected: &Expectations) -> Outcome<Option<Delegation>> {
    let Some((parent, _)) = expected
        .threads
        .iter()
        .find(|(_, row)| row.case == "Devin background exec and receipt")
    else {
        return Ok(None);
    };
    let (_, id) = parent.split_once(':').unwrap();
    let db = open_readonly(&root.join("devin/Devin/cli/sessions.db"))?;
    let mut query = db.prepare(
        "WITH RECURSIVE chain(node_id, parent_node_id, created_at, chat_message) AS (
            SELECT node_id, parent_node_id, created_at, chat_message FROM message_nodes
            WHERE session_id=?1 AND node_id=(SELECT main_chain_id FROM sessions WHERE id=?1)
            UNION
            SELECT n.node_id,n.parent_node_id,n.created_at,n.chat_message FROM message_nodes n
              JOIN chain c ON n.node_id=c.parent_node_id WHERE n.session_id=?1
         ) SELECT created_at,chat_message FROM chain ORDER BY node_id",
    )?;
    let mut rows = query.query([id])?;
    let mut calls = BTreeMap::new();
    let mut launches: BTreeMap<String, Launch> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let seconds: i64 = row.get(0)?;
        let raw: String = row.get(1)?;
        let Ok(message) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            if let Some(id) = call["id"].as_str() {
                calls.insert(id.to_owned(), (seconds, call.clone()));
            }
        }
        let Some((id, output)) = message["tool_call_id"]
            .as_str()
            .zip(message["content"].as_str())
        else {
            continue;
        };
        let Some((started, call)) = calls.get(id) else {
            continue;
        };
        let args = &call["arguments"];
        match call["name"].as_str() {
            Some("exec")
                if args["command"]
                    .as_str()
                    .is_some_and(|command| command.to_ascii_lowercase().contains("peer-run")) =>
            {
                if let Some(handle) = output
                    .trim()
                    .strip_prefix("Command running in background with ID: ")
                {
                    launches.insert(
                        handle.to_owned(),
                        Launch {
                            call_id: id.to_owned(),
                            seconds: *started,
                            output: output.into(),
                            receipts: Vec::new(),
                        },
                    );
                }
            }
            Some("get_output") => {
                if let Some(launch) = args["shell_id"]
                    .as_str()
                    .and_then(|handle| launches.get_mut(handle))
                {
                    launch.receipts.push(output.into());
                }
            }
            _ => {}
        }
    }
    Ok(Some(Delegation {
        parent: parent.clone(),
        launches,
    }))
}

fn expect_parent(checks: &mut Value, delegation: &Delegation, launch: &Launch, confidence: &str) {
    checks["parent"] = json!(delegation.parent);
    checks["parentConfidence"] = json!(confidence);
    checks["parentToolCallId"] = json!(launch.call_id);
    checks["parentToolListsChild"] = json!(true);
    checks["defaultRoot"] = json!(false);
}

fn capture_antigravity(home: &Path, root: &Path, expected: &mut Expectations) -> Outcome<()> {
    for (folder, label) in [("antigravity", "desktop"), ("antigravity-cli", "CLI store")] {
        let source = home.join(".gemini").join(folder);
        let destination = root.join("antigravity/.gemini").join(folder);
        let mut paths = files(&source.join("conversations"), 1);
        paths.retain(|path| extension(path, "db") && bounded(path));
        order_recent(&mut paths);
        let summaries = source.join("conversation_summaries.db");
        let summaries = if summaries.is_file() {
            fs::create_dir_all(destination.join("conversations"))?;
            Some((
                open_readonly(&summaries)?,
                Connection::open(destination.join("conversation_summaries.db"))?,
            ))
        } else {
            None
        };
        let mut count = 0;
        for path in paths.into_iter().take(32) {
            let Some(id) = path.file_stem().and_then(|id| id.to_str()) else {
                continue;
            };
            let input = open_readonly(&path)?;
            if !input.query_row("SELECT EXISTS(SELECT 1 FROM steps)", [], |row| {
                row.get::<_, bool>(0)
            })? {
                continue;
            }
            count += 1;
            let output_path = destination
                .join("conversations")
                .join(path.file_name().unwrap());
            fs::create_dir_all(output_path.parent().unwrap())?;
            let output = Connection::open(&output_path)?;
            copy_table(
                &input,
                &output,
                "steps",
                "SELECT idx,step_type,status,step_payload,error_details FROM steps",
                &[],
            )?;
            if table_exists(&input, "trajectory_metadata_blob")? {
                copy_table(
                    &input,
                    &output,
                    "trajectory_metadata_blob",
                    "SELECT id,data FROM trajectory_metadata_blob WHERE id='main'",
                    &[],
                )?;
            }
            if let Some((input, output)) = &summaries {
                for (table, select) in [
                    ("conversation_summaries", "SELECT conversation_id,title,preview,last_modified_time,workspace_uris,source,app_data_dir,parent_conversation_id,raw_summary FROM conversation_summaries WHERE conversation_id=?1"),
                    ("conversation_parents", "SELECT conversation_id,parent_id FROM conversation_parents WHERE conversation_id=?1"),
                ] {
                    if table_exists(input, table)? {
                        copy_table(input, output, table, select, &[id])?;
                    }
                }
            }
            drop(output);
            File::options()
                .write(true)
                .open(&output_path)?
                .set_times(FileTimes::new().set_modified(path.metadata()?.modified()?))?;
            add(
                expected,
                format!("antigravity:{id}"),
                format!("Antigravity {label} steps {count}"),
                json!({"indexed":true}),
            );
            if count == 2 {
                break;
            }
        }
    }
    Ok(())
}

fn capture_kiro(
    home: &Path,
    root: &Path,
    delegation: Option<&Delegation>,
    expected: &mut Expectations,
) -> Outcome<()> {
    let source = home.join(".kiro/sessions");
    let destination = root.join("kiro/.kiro/sessions");
    let mut paths = files(&source, 3);
    paths.retain(|path| {
        path.file_name().is_some_and(|name| name == "session.json") && bounded(path)
    });
    order_recent(&mut paths);
    let mut list = Vec::new();
    for path in paths.into_iter().take(24) {
        let value: Value = serde_json::from_reader(File::open(&path)?.take(MAX_FILE))?;
        let Some(id) = value["id"].as_str() else {
            continue;
        };
        let messages = path.with_file_name("messages.jsonl");
        if !bounded(&messages) {
            continue;
        }
        for file in [&path, &messages] {
            copy(file, &destination.join(file.strip_prefix(&source)?))?;
        }
        let cwd = value["workspacePaths"]
            .get(0)
            .or_else(|| value["rootPaths"].get(0));
        list.push(json!({
            "sessionId":id,"cwd":cwd,"title":value["title"],"updatedAt":value["lastModifiedAt"],
        }));
        let mut checks = json!({
            "createdAt": time(&value["createdAt"]),
            "activityTime": time(&value["lastModifiedAt"]).unwrap_or_default(),
            "internal": value["_meta"]["kiro"]["nonInteractive"] == true,
        });
        let launch = delegation.and_then(|delegation| delegation.with_receipt(id));
        let label = if let (Some(delegation), Some(launch)) = (delegation, launch) {
            expect_parent(&mut checks, delegation, launch, "exact");
            "wrapper receipt"
        } else {
            "native session"
        };
        add(
            expected,
            format!("kiro:{id}"),
            format!("Kiro {label} {}", list.len()),
            checks,
        );
    }
    write_json(&root.join("kiro/replay.json"), &json!({"sessions":list}))?;
    Ok(())
}

fn capture_wire(raw: &Path, root: &Path, expected: &mut Expectations) -> Outcome<()> {
    let mut paths = files(raw, 1);
    paths.retain(|path| extension(path, "jsonl") && bounded(path));
    order_recent(&mut paths);
    let mut rates = Vec::new();
    let mut replay = BTreeMap::new();
    let mut catalog = Vec::new();
    for path in paths.into_iter().take(64) {
        let mut load: Option<(Value, String, Value, Vec<Value>)> = None;
        for entry in json_rows(&path)? {
            let Some(text) = entry["text"].as_str() else {
                continue;
            };
            let Ok(row) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            let direction = entry["direction"].as_str();
            let params = &row["params"];
            match (direction, row["method"].as_str()) {
                (Some("sent"), Some("session/load")) => {
                    if let Some(id) = params["sessionId"].as_str() {
                        load = Some((
                            row["id"].clone(),
                            id.to_owned(),
                            params["cwd"].clone(),
                            Vec::new(),
                        ));
                    }
                }
                (Some("received"), Some("session/update")) => {
                    if let Some((_, id, _, updates)) = &mut load {
                        if params["sessionId"].as_str() == Some(id) && updates.len() < MAX_ROWS {
                            updates.push(params.clone());
                        }
                    }
                }
                (Some("received"), _) => {
                    if row["type"] == "rate_limit_event" && rates.len() < 12 {
                        rates.push(row.clone());
                    }
                    if let Some((request, _, _, _)) = &load {
                        if &row["id"] != request || request.is_null() {
                            continue;
                        }
                        let (_, id, cwd, updates) = load.take().unwrap();
                        if row.get("result").is_some()
                            && !updates.is_empty()
                            && updates.len() < MAX_ROWS
                            && replay.len() < RECENT
                        {
                            catalog.push(json!({"sessionId":id,"cwd":cwd}));
                            add(
                                expected,
                                format!("acp:{id}"),
                                format!("Recorded ACP load {}", catalog.len()),
                                json!({"indexed":true,"hasCopyableAnswer":true}),
                            );
                            replay.insert(id, updates);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if !rates.is_empty() {
        let path = root.join("claude/rate_limits.jsonl");
        fs::create_dir_all(path.parent().unwrap())?;
        let mut file = File::create(path)?;
        for row in rates {
            serde_json::to_writer(&mut file, &row)?;
            writeln!(file)?;
        }
    }
    if !replay.is_empty() {
        write_json(
            &root.join("acp/replay.json"),
            &json!({"sessions":catalog,"updates":replay}),
        )?;
    }
    Ok(())
}

fn table_exists(db: &Connection, table: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
}

fn copy_table(
    input: &Connection,
    output: &Connection,
    table: &str,
    select: &str,
    params: &[&str],
) -> Outcome<()> {
    let mut query = input.prepare(select)?;
    let names: Vec<_> = query
        .column_names()
        .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
        .collect();
    let count = names.len();
    let transaction = output.unchecked_transaction()?;
    transaction.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS \"{table}\" ({})",
        names.join(",")
    ))?;
    let markers = vec!["?"; count].join(",");
    let mut insert = transaction.prepare(&format!("INSERT INTO \"{table}\" VALUES ({markers})"))?;
    let mut rows = query.query(params_from_iter(params))?;
    let mut size = 0;
    let mut row_count = 0;
    while let Some(row) = rows.next()? {
        let values = (0..count)
            .map(|index| row.get_ref(index))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        size += values
            .iter()
            .map(|value| match value {
                ValueRef::Text(value) | ValueRef::Blob(value) => value.len(),
                _ => 8,
            })
            .sum::<usize>();
        row_count += 1;
        if size > MAX_FILE as usize || row_count > MAX_ROWS {
            return Err(format!("Native table {table} sample exceeded the capture limit").into());
        }
        insert.execute(params_from_iter(
            values.into_iter().map(ToSqlOutput::Borrowed),
        ))?;
    }
    drop(insert);
    transaction.commit()?;
    Ok(())
}
