//! Store tests against synthetic databases with hand-encoded protobuf.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use rusqlite::{params, Connection};
use vetro_proto::{
    ContentBlock, ItemKind, NoticeLevel, ThreadSource, ToolContent, ToolKind, ToolStatus,
};

use crate::{steps::UNKNOWN_STEP_PREFIX, wire::enc::Enc, Store};

const T0: i64 = 1_790_000_000_000;

#[test]
fn wal_without_shared_memory_is_busy_instead_of_stale() {
    let root = TempDir::new("busy-wal");
    let path = root.0.join("history.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE history(text)")
        .unwrap();
    let wal = super::sibling(&path, "-wal");
    fs::write(&wal, b"unavailable WAL").unwrap();
    let error = super::sqlite::open_readonly(&path).unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    assert!(!super::sibling(&path, "-shm").exists());
    assert_eq!(fs::read(wal).unwrap(), b"unavailable WAL");
}

type StepRow = (i64, i64, i64, Vec<u8>, Option<Vec<u8>>);
type SummaryRow<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    Vec<u8>,
);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("vetro-agstore-{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, u128)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                let t = m
                    .modified()
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                out.insert(e.path(), (m.len(), t));
            }
        }
    }
    out
}

fn meta(ts: i64) -> Enc {
    Enc::new().ts(1, ts)
}

fn step(kind: u64, status: u64, meta: Enc, field: u32, body: Enc) -> Vec<u8> {
    Enc::new()
        .uint(1, kind)
        .uint(4, status)
        .msg(5, meta)
        .msg(field, body)
        .build()
}

fn call(id: &str, name: &str, args: &str) -> Enc {
    Enc::new().str(1, id).str(2, name).str(3, args)
}

fn create_conversation(path: &Path, workspace: Option<&str>, steps: &[StepRow]) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.execute_batch(
        "CREATE TABLE trajectory_meta (trajectory_id text, cascade_id text, trajectory_type integer, source integer, PRIMARY KEY (trajectory_id));
         CREATE TABLE steps (idx integer, step_type integer NOT NULL DEFAULT 0, status integer NOT NULL DEFAULT 0,
            has_subtrajectory numeric NOT NULL DEFAULT false, metadata blob, error_details blob, permissions blob,
            task_details blob, render_info blob, step_payload blob, step_format integer NOT NULL DEFAULT 0, PRIMARY KEY (idx));
         CREATE TABLE trajectory_metadata_blob (id text DEFAULT 'main', data blob, PRIMARY KEY (id));",
    )
    .unwrap();
    let mut blob = Enc::new().ts(2, T0);
    if let Some(w) = workspace {
        blob = blob.msg(1, Enc::new().str(1, w));
    }
    conn.execute(
        "INSERT INTO trajectory_metadata_blob VALUES ('main', ?1)",
        [blob.build()],
    )
    .unwrap();
    for (idx, kind, status, payload, error) in steps {
        let metadata = crate::wire::Msg(payload).bytes(5).map(<[u8]>::to_vec);
        conn.execute(
            "INSERT INTO steps (idx, step_type, status, metadata, step_payload, error_details) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![idx, kind, status, metadata, payload, error],
        )
        .unwrap();
    }
    conn
}

fn create_summaries(root: &Path, rows: &[SummaryRow], parents: &[(&str, &str)]) {
    let conn = Connection::open(root.join("conversation_summaries.db")).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.execute_batch(
        "CREATE TABLE conversation_summaries (conversation_id text, title text NOT NULL DEFAULT '', preview text NOT NULL DEFAULT '',
            step_count integer NOT NULL DEFAULT 0, last_modified_time datetime NOT NULL, workspace_uris text NOT NULL,
            status text NOT NULL DEFAULT '', source text NOT NULL DEFAULT '', project_id text NOT NULL DEFAULT '',
            agent_name text NOT NULL DEFAULT '', parent_conversation_id text NOT NULL DEFAULT '', nesting_depth integer NOT NULL DEFAULT 0,
            app_data_dir text NOT NULL DEFAULT '', raw_summary blob, PRIMARY KEY (conversation_id));
         CREATE TABLE conversation_parents (conversation_id TEXT PRIMARY KEY, parent_id TEXT NOT NULL) WITHOUT ROWID;",
    )
    .unwrap();
    for (id, title, preview, modified, workspaces, app_dir, raw) in rows {
        conn.execute(
            "INSERT INTO conversation_summaries (conversation_id, title, preview, last_modified_time, workspace_uris, app_data_dir, raw_summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, title, preview, modified, workspaces, app_dir, raw],
        )
        .unwrap();
    }
    for (id, parent) in parents {
        conn.execute(
            "INSERT INTO conversation_parents VALUES (?1, ?2)",
            [id, parent],
        )
        .unwrap();
    }
}

fn user_step(idx: i64, text: &str) -> StepRow {
    let body = Enc::new().str(2, text).msg(3, Enc::new().str(1, text));
    (
        idx,
        14,
        3,
        step(14, 3, meta(T0 + idx * 1000), 19, body),
        None,
    )
}

fn desktop_store() -> (TempDir, Store) {
    let tmp = TempDir::new("desktop");
    let root = tmp.0.join("antigravity");
    fs::create_dir_all(root.join("conversations")).unwrap();
    create_summaries(
        &root,
        &[
            (
                "aaaa-1",
                "First task",
                "Do the thing",
                "2026-01-02 03:04:05.5+00:00",
                r#"["file:///d%3A/Work/proj"]"#,
                "antigravity",
                Vec::new(),
            ),
            (
                "bbbb-2",
                "",
                "",
                "2026-01-03 00:00:00+00:00",
                "",
                "antigravity",
                Enc::new()
                    .ts(7, T0)
                    .msg(
                        17,
                        Enc::new().msg(8, Enc::new().str(1, "browser").str(2, "Page Checker")),
                    )
                    .build(),
            ),
            (
                "cccc-3",
                "Lives elsewhere",
                "",
                "2026-01-04 00:00:00+00:00",
                "",
                "antigravity-cli",
                Vec::new(),
            ),
        ],
        &[("bbbb-2", "aaaa-1")],
    );
    let conversations = root.join("conversations");
    drop(create_conversation(
        &conversations.join("aaaa-1.db"),
        None,
        &[user_step(0, "hello")],
    ));
    drop(create_conversation(
        &conversations.join("bbbb-2.db"),
        None,
        &[],
    ));
    drop(create_conversation(
        &conversations.join("dddd-4.db"),
        Some("file:///home/u/other"),
        &[user_step(0, "orphan first line\nsecond")],
    ));
    (tmp, Store::open(root))
}

#[test]
fn lists_desktop_conversations_newest_first() {
    let (tmp, store) = desktop_store();
    let before = snapshot(&tmp.0);
    let list = store.list().unwrap();
    assert_eq!(snapshot(&tmp.0), before, "listing must not touch any file");

    let ids: Vec<&str> = list.iter().map(|r| r.native_id.as_str()).collect();
    assert_eq!(
        ids,
        ["dddd-4", "bbbb-2", "aaaa-1"],
        "cccc-3 has no file in this store"
    );
    let first = list.iter().find(|r| r.native_id == "aaaa-1").unwrap();
    assert_eq!(first.title, "First task");
    assert_eq!(first.preview.as_deref(), Some("Do the thing"));
    assert_eq!(first.updated_at, 1_767_323_045_500);
    if cfg!(windows) {
        assert_eq!(first.cwd, Some(PathBuf::from(r"D:\Work\proj")));
    }
    assert_eq!(first.source, ThreadSource::Desktop);
    assert!(!first.resumable);
    assert_eq!(first.origin.as_deref(), Some("antigravity"));

    let sub = list.iter().find(|r| r.native_id == "bbbb-2").unwrap();
    assert_eq!(sub.title, "Page Checker");
    assert_eq!(sub.parent_native_id.as_deref(), Some("aaaa-1"));
    assert_eq!(sub.created_at, Some(T0));

    let orphan = list.iter().find(|r| r.native_id == "dddd-4").unwrap();
    assert_eq!(orphan.title, "orphan first line");
    assert_eq!(orphan.cwd, Some(PathBuf::from("/home/u/other")));
    assert_eq!(orphan.created_at, Some(T0));
}

#[test]
fn cli_store_source_comes_from_the_cli_logs() {
    let tmp = TempDir::new("cli");
    let root = tmp.0.join("antigravity-cli");
    fs::create_dir_all(root.join("conversations")).unwrap();
    fs::create_dir_all(root.join("log")).unwrap();
    fs::write(root.join("log/cli-1.log"), "I printmode.go:1] Print mode: start\nI server.go:1] Created conversation 1111-a (backend=X)\n").unwrap();
    fs::write(
        root.join("log/acp-server-2.log"),
        "I server.go:1] Created conversation 2222-b (backend=X)\n",
    )
    .unwrap();
    for id in ["1111-a", "2222-b", "3333-c"] {
        drop(create_conversation(
            &root.join(format!("conversations/{id}.db")),
            None,
            &[user_step(0, id)],
        ));
    }
    let list = Store::open(&root).list().unwrap();
    let by_id = |id: &str| list.iter().find(|r| r.native_id == id).unwrap();
    assert_eq!(by_id("1111-a").source, ThreadSource::Exec);
    assert_eq!(
        by_id("1111-a").origin.as_deref(),
        Some("antigravity-cli print")
    );
    assert_eq!(by_id("2222-b").source, ThreadSource::Cli);
    assert_eq!(
        by_id("2222-b").origin.as_deref(),
        Some("antigravity-cli acp")
    );
    assert_eq!(by_id("3333-c").source, ThreadSource::Cli);
}

fn rich_steps() -> Vec<StepRow> {
    let image = Enc::new()
        .str(1, "image/png")
        .str(5, "C:/shot.png")
        .bytes(6, b"PNG");
    let user = Enc::new().str(2, "fix it").msg(9, image);
    let planner = Enc::new()
        .str(3, "thinking hard")
        .str(1, "On it.")
        .str(8, "On it.")
        .msg(
            7,
            call(
                "call_1",
                "run_command",
                r#"{"CommandLine":"cargo test","toolSummary":"run tests"}"#,
            ),
        );
    let terminal = Enc::new()
        .str(1, "cargo test")
        .str(2, "D:\\w")
        .uint(3, 1)
        .str(4, "1 failed");
    let render = Enc::new()
        .str(5, "Ran")
        .str(7, "terminal")
        .msg(9, Enc::new().str(1, "run tests"))
        .msg(10, Enc::new().msg(3, terminal));
    let run = Enc::new().msg(
        2,
        Enc::new()
            .str(1, "Created At: x\nCompleted At: y\n1 failed")
            .msg(3, render),
    );
    let lines = [(3, "a"), (2, "b"), (1, "B"), (3, "c")]
        .into_iter()
        .fold(Enc::new(), |e, (op, t)| {
            e.msg(3, Enc::new().str(1, t).uint(2, op))
        });
    let file_edit = Enc::new()
        .uint(1, 3)
        .str(2, "C:/w/f.txt")
        .msg(3, Enc::new().msg(3, lines))
        .msg(4, Enc::new().uint(1, 1).uint(2, 1));
    let edit = Enc::new()
        .msg(1, Enc::new().str(1, "Path").str(2, "C:/w/f.txt"))
        .msg(
            2,
            Enc::new().msg(
                6,
                Enc::new()
                    .str(1, "type.googleapis.com/x.FileEdit")
                    .msg(2, file_edit),
            ),
        );
    let read = Enc::new().msg(
        2,
        Enc::new()
            .str(1, "1: hello")
            .msg(
                6,
                Enc::new()
                    .str(1, "t/ReadFile")
                    .msg(2, Enc::new().str(3, "C:/w/r.txt").uint(4, 7)),
            )
            .msg(4, Enc::new().str(1, "image/png").bytes(2, b"img")),
    );
    let sub = Enc::new().msg(
        2,
        Enc::new().msg(
            6,
            Enc::new()
                .str(1, "t/InvokeSubagent")
                .msg(2, Enc::new().msg(2, Enc::new().str(1, "child-1"))),
        ),
    );
    let error = Enc::new().str(2, "file not found").str(3, "stack").build();
    let failed = Enc::new().msg(1, Enc::new().str(1, "Path").str(2, "C:/missing"));
    vec![
        (0, 14, 3, step(14, 3, meta(T0), 19, user), None),
        (1, 15, 3, step(15, 3, meta(T0 + 1000), 20, planner), None),
        (
            2,
            132,
            3,
            step(
                132,
                3,
                meta(T0 + 2000).msg(4, call("call_1", "run_command", "{}")),
                140,
                run,
            ),
            None,
        ),
        (
            3,
            132,
            3,
            step(
                132,
                3,
                meta(T0 + 3000).msg(4, call("call_2", "edit_file", "")),
                140,
                edit,
            ),
            None,
        ),
        (
            4,
            132,
            3,
            step(
                132,
                3,
                meta(T0 + 4000).msg(4, call("call_3", "read_file", "")),
                140,
                read,
            ),
            None,
        ),
        (
            5,
            132,
            3,
            step(
                132,
                3,
                meta(T0 + 5000).msg(4, call("call_4", "invoke_subagent", r#"{"Role":"Helper"}"#)),
                140,
                sub,
            ),
            None,
        ),
        (
            6,
            132,
            7,
            step(
                132,
                7,
                meta(T0 + 6000).msg(4, call("call_5", "read_file", "")),
                140,
                failed,
            ),
            Some(error),
        ),
        (
            7,
            17,
            3,
            step(
                17,
                3,
                meta(T0 + 7000),
                24,
                Enc::new().msg(3, Enc::new().str(2, "please continue")),
            ),
            None,
        ),
        (
            8,
            23,
            3,
            step(
                23,
                3,
                meta(T0 + 8000),
                30,
                Enc::new().str(5, "summary text"),
            ),
            None,
        ),
        (
            9,
            101,
            3,
            step(
                101,
                3,
                meta(T0 + 9000),
                114,
                Enc::new().str(3, "task_notification").msg(
                    2,
                    Enc::new()
                        .str(1, "Task finished")
                        .msg(10, Enc::new().str(1, "output")),
                ),
            ),
            None,
        ),
        (
            10,
            999,
            3,
            step(
                999,
                3,
                meta(T0 + 10_000),
                77,
                Enc::new().uint(1, 5).str(2, "Mystery step"),
            ),
            None,
        ),
    ]
}

#[test]
fn desktop_native_step_envelope_keeps_user_and_planner_content() {
    let tmp = TempDir::new("native-step-envelope");
    let path = tmp.0.join("conversations/adef-10.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let prompt = Enc::new()
        .str(2, "Name a fruit.")
        .msg(3, Enc::new().str(1, "Name a fruit."));
    let answer = Enc::new()
        .str(1, "Peach.")
        .str(3, "Choose a fruit.")
        .str(8, "Peach.");
    let completed = Enc::new().msg(1, Enc::new().uint(1, 3).ts(2, T0 + 2400));
    let planner_meta = meta(T0 + 1000).msg(26, completed);
    drop(create_conversation(
        &path,
        None,
        &[
            (1, 14, 3, step(14, 3, meta(T0), 19, prompt), None),
            (2, 15, 3, step(15, 3, planner_meta, 20, answer), None),
        ],
    ));
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + std::time::Duration::from_millis((T0 + 50_000) as u64))
        .unwrap();
    let store = Store::open(&tmp.0);
    let list = store.list_with_native_activity().unwrap();
    assert_eq!(list[0].0.updated_at, T0 + 2400);
    assert_eq!(list[0].1, Some(T0 + 2400));
    assert!(super::mtime_ms(&path).unwrap() > list[0].0.updated_at);
    let transcript = store.read("adef-10").unwrap();
    let [user, thought, answer, end] = transcript.items.as_slice() else {
        panic!("expected user input, planner thought, answer, and completion");
    };
    assert!(
        matches!(&user.kind, ItemKind::UserMessage { content } if content == &[ContentBlock::Text { text: "Name a fruit.".into() }])
    );
    assert!(
        matches!(&thought.kind, ItemKind::Reasoning { text, streaming: false } if text == "Choose a fruit.")
    );
    assert!(
        matches!(&answer.kind, ItemKind::AgentMessage { text, streaming: false } if text == "Peach.")
    );
    assert!(matches!(
        &end.kind,
        ItemKind::TurnEnd {
            duration_ms: Some(2400),
            usage: None,
            ..
        }
    ));
}

#[test]
fn reads_every_step_kind() {
    let tmp = TempDir::new("read");
    let root = tmp.0.join("antigravity");
    fs::create_dir_all(root.join("conversations")).unwrap();
    drop(create_conversation(
        &root.join("conversations/eeee-5.db"),
        None,
        &rich_steps(),
    ));
    let before = snapshot(&tmp.0);
    let items = Store::open(&root).read("eeee-5").unwrap().items;
    assert_eq!(snapshot(&tmp.0), before, "reading must not touch any file");

    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "step-0",
            "step-1-thinking",
            "step-1-text",
            "call_1",
            "call_2",
            "call_3",
            "call_4",
            "call_5",
            "step-7",
            "step-8",
            "step-9",
            "step-10"
        ]
    );
    assert!(items.iter().all(|i| i.turn_id.as_deref() == Some("turn-0")));
    assert_eq!(items[0].ts, Some(T0));

    let ItemKind::UserMessage { content } = &items[0].kind else {
        panic!()
    };
    assert_eq!(
        content[0],
        ContentBlock::Text {
            text: "fix it".into()
        }
    );
    assert_eq!(
        content[1],
        ContentBlock::Image {
            mime_type: "image/png".into(),
            data: Some("UE5H".into()),
            uri: None
        }
    );
    assert!(matches!(&items[1].kind, ItemKind::Reasoning { text, .. } if text == "thinking hard"));
    assert!(matches!(&items[2].kind, ItemKind::AgentMessage { text, .. } if text == "On it."));

    let tool = |i: usize| match &items[i].kind {
        ItemKind::ToolCall(t) => t,
        other => panic!("{other:?}"),
    };
    let run = tool(3);
    assert_eq!(
        items[3].ts,
        Some(T0 + 1000),
        "the call keeps the planner's position and time"
    );
    assert_eq!(
        (run.kind, run.status, run.title.as_str()),
        (ToolKind::Execute, ToolStatus::Completed, "Ran run tests")
    );
    assert_eq!(
        run.content,
        [ToolContent::Terminal {
            command: Some("cargo test".into()),
            cwd: Some("D:\\w".into()),
            output: "1 failed".into(),
            exit_code: Some(1)
        }]
    );

    let edit = tool(4);
    assert_eq!(edit.kind, ToolKind::Edit);
    let ToolContent::Diff {
        unified: Some(unified),
        ..
    } = &edit.content[0]
    else {
        panic!("{:?}", edit.content)
    };
    assert!(
        unified.contains("@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"),
        "{unified}"
    );

    let read = tool(5);
    assert_eq!(read.kind, ToolKind::Read);
    assert_eq!(read.locations[0].line, Some(7));
    assert!(read.content.contains(&ToolContent::Text {
        text: "1: hello".into()
    }));
    assert!(read.content.contains(&ToolContent::Image {
        mime_type: "image/png".into(),
        data: "aW1n".into()
    }));

    let sub = tool(6);
    assert_eq!((sub.kind, sub.title.as_str()), (ToolKind::Agent, "Helper"));
    assert_eq!(sub.child_thread_ids, ["antigravity:child-1"]);

    let failed = tool(7);
    assert_eq!(failed.status, ToolStatus::Failed);
    assert!(failed.content.contains(&ToolContent::Text {
        text: "file not found\n\nstack".into()
    }));
    assert_eq!(failed.title, "read_file missing");

    assert!(
        matches!(&items[8].kind, ItemKind::Notice { level: NoticeLevel::Info, text, .. } if text == "please continue")
    );
    assert!(
        matches!(&items[9].kind, ItemKind::Notice { text, detail: Some(_), .. } if text == "Context compacted")
    );
    assert!(matches!(&items[10].kind, ItemKind::Notice { text, .. } if text == "Task finished"));
    let unknown = tool(11);
    assert_eq!(
        unknown.name.as_deref(),
        Some(&*format!("{UNKNOWN_STEP_PREFIX}999"))
    );
    assert_eq!(
        (unknown.kind, unknown.title.as_str()),
        (ToolKind::Other, "Mystery step")
    );
}

#[test]
fn subagent_prompt_from_parent_is_the_first_user_message() {
    let tmp = TempDir::new("subagent");
    let root = tmp.0.join("antigravity");
    fs::create_dir_all(root.join("conversations")).unwrap();
    let msg = |text: &str| {
        Enc::new().str(3, "agent_message").msg(
            2,
            Enc::new()
                .str(1, "Message from Parent Agent")
                .msg(10, Enc::new().str(1, text)),
        )
    };
    let steps = vec![
        (
            0,
            101,
            3,
            step(101, 3, meta(T0), 114, msg("inspect the page")),
            None,
        ),
        (
            1,
            15,
            3,
            step(15, 3, meta(T0), 20, Enc::new().str(1, "done")),
            None,
        ),
        (
            2,
            101,
            3,
            step(101, 3, meta(T0), 114, msg("follow-up")),
            None,
        ),
    ];
    drop(create_conversation(
        &root.join("conversations/ffff-6.db"),
        None,
        &steps,
    ));
    let items = Store::open(&root).read("ffff-6").unwrap().items;
    assert!(
        matches!(&items[0].kind, ItemKind::UserMessage { content } if content == &[ContentBlock::Text { text: "inspect the page".into() }])
    );
    assert!(
        matches!(&items[2].kind, ItemKind::Notice { text, .. } if text == "Message from Parent Agent")
    );
}

#[test]
fn rejects_bad_and_missing_ids() {
    let (_tmp, store) = desktop_store();
    assert_eq!(
        store.read("../x").unwrap_err().code,
        vetro_proto::rpc::ErrorCode::BadRequest
    );
    assert_eq!(
        store.read("nope").unwrap_err().code,
        vetro_proto::rpc::ErrorCode::NotFound
    );
    assert!(
        Store::open(std::env::temp_dir().join("vetro-agstore-missing-root"))
            .list()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn reads_uncheckpointed_frames_of_a_live_writer_without_disturbing_it() {
    let tmp = TempDir::new("live");
    let root = tmp.0.join("antigravity");
    fs::create_dir_all(root.join("conversations")).unwrap();
    let path = root.join("conversations/1234-7.db");
    let writer = create_conversation(&path, None, &[user_step(0, "first")]);
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let late = user_step(1, "second");
    writer
        .execute(
            "INSERT INTO steps (idx, step_type, status, step_payload) VALUES (?1, ?2, ?3, ?4)",
            params![late.0, late.1, late.2, late.3],
        )
        .unwrap();
    assert!(
        fs::metadata(path.with_file_name("1234-7.db-wal"))
            .unwrap()
            .len()
            > 0
    );

    let files_before: Vec<PathBuf> = snapshot(&tmp.0).into_keys().collect();
    let items = Store::open(&root).read("1234-7").unwrap().items;
    assert_eq!(items.len(), 2, "frames still in the WAL are visible");
    let files_after: Vec<PathBuf> = snapshot(&tmp.0).into_keys().collect();
    assert_eq!(files_before, files_after);

    let more = user_step(2, "third");
    writer
        .execute(
            "INSERT INTO steps (idx, step_type, status, step_payload) VALUES (?1, ?2, ?3, ?4)",
            params![more.0, more.1, more.2, more.3],
        )
        .unwrap();
    writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|busy| assert_eq!(busy, 0))
        .unwrap();
    drop(writer);
    assert!(
        !path.with_file_name("1234-7.db-wal").exists(),
        "the writer could clean up after itself"
    );
    assert_eq!(Store::open(&root).read("1234-7").unwrap().items.len(), 3);
}

#[test]
fn wal_without_shm_defers_without_creating_files() {
    let tmp = TempDir::new("orphan-wal");
    let root = tmp.0.join("antigravity");
    fs::create_dir_all(root.join("conversations")).unwrap();
    let src = tmp.0.join("src.db");
    let writer = create_conversation(&src, None, &[user_step(0, "checkpointed")]);
    writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let late = user_step(1, "only in wal");
    writer
        .execute(
            "INSERT INTO steps (idx, step_type, status, step_payload) VALUES (?1, ?2, ?3, ?4)",
            params![late.0, late.1, late.2, late.3],
        )
        .unwrap();
    let dst = root.join("conversations/5678-8.db");
    fs::copy(&src, &dst).unwrap();
    fs::copy(
        tmp.0.join("src.db-wal"),
        root.join("conversations/5678-8.db-wal"),
    )
    .unwrap();
    drop(writer);

    let before = snapshot(&root);
    let error = Store::open(&root).read("5678-8").unwrap_err();
    assert_eq!(error.code, vetro_proto::rpc::ErrorCode::Busy);
    assert_eq!(snapshot(&root), before);
    assert!(!root.join("conversations/5678-8.db-shm").exists());
}
