use std::{
    env, fs,
    io::{self, BufRead, Write},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::Duration,
};

use serde_json::{json, Value};
use vetro_agent_api::{
    jsonrpc::{Error, Id, Incoming, Peer, RemoteError, Version},
    process::{Process, SpawnSpec, Stdout},
    WireLog,
};
use vetro_proto::{RawDirection, RawEntry};

const LINE: &str = r#"{"text":"a \"quote\" and a backslash \\", "unicode":"例"}"#;

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("fixture-echo") => echo(args.collect()),
        Some("fixture-rpc") => rpc(args.next().unwrap() == "v2"),
        Some("fixture-tree" | "fixture-detached-tree") => {
            let detached = env::args().nth(1).as_deref() == Some("fixture-detached-tree");
            let mut leaf = Command::new(env::current_exe().unwrap())
                .arg(if detached {
                    "fixture-detached-leaf"
                } else {
                    "fixture-leaf"
                })
                .spawn()
                .unwrap();
            if !detached {
                emit(leaf.id());
            }
            leaf.wait().unwrap();
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Some("fixture-detached-leaf") => {
            assert_ne!(unsafe { libc::setsid() }, -1);
            let mut child = Command::new(env::current_exe().unwrap())
                .arg("fixture-leaf")
                .spawn()
                .unwrap();
            emit(format!("{} {}", std::process::id(), child.id()));
            child.wait().unwrap();
        }
        Some("fixture-leaf") => thread::sleep(Duration::from_secs(60)),
        Some("fixture-host") => runtime().block_on(async {
            let mode = args.next().unwrap_or_else(|| "fixture-tree".into());
            let (process, mut stdout) = spawn(&mode, Arc::default());
            emit(format!("{} {}", process.id(), line(&mut stdout).await));
            let _ = process.wait().await;
        }),
        _ => runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(25), async {
                round_trip().await;
                println!("line round-trip and wire log ... ok");
                json_rpc(Version::V2).await;
                json_rpc(Version::Bare).await;
                println!("JSON-RPC with and without version field ... ok");
                tree_shutdown(TreeShutdown::Drop, "fixture-tree").await;
                tree_shutdown(TreeShutdown::Kill, "fixture-tree").await;
                tree_shutdown(TreeShutdown::Stop, "fixture-tree").await;
                println!("process tree drop, kill and stop ... ok");
                host_death("fixture-tree", 2).await;
                println!("process tree after hard host exit ... ok");
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                {
                    tree_shutdown(TreeShutdown::Drop, "fixture-detached-tree").await;
                    tree_shutdown(TreeShutdown::Stop, "fixture-detached-tree").await;
                    host_death("fixture-detached-tree", 3).await;
                    println!("setsid descendants after drop, stop and hard host exit ... ok");
                }
            })
            .await
            .expect("transport tests timed out");
        }),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn fixture(mode: &str) -> SpawnSpec {
    let mut spec = SpawnSpec::new(env::current_exe().unwrap());
    spec.args.push(mode.into());
    spec
}

fn spawn(mode: &str, log: Arc<WireLog>) -> (Process, Stdout) {
    Process::spawn(fixture(mode), log).unwrap()
}

async fn line(stdout: &mut Stdout) -> String {
    stdout.recv().await.unwrap().unwrap()
}

fn emit(text: impl std::fmt::Display) {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{text}").unwrap();
    stdout.flush().unwrap();
}

fn echo(args: Vec<String>) {
    emit(json!({
        "args": args,
        "env": env::var("VETRO_TEST_OVERRIDE").unwrap(),
        "cwd": env::current_dir().unwrap(),
    }));
    eprintln!("fixture stderr");
    for line in io::stdin().lock().lines() {
        emit(line.unwrap());
    }
    emit("stdin closed");
}

async fn round_trip() {
    let path = env::temp_dir().join(format!("vetro-wirelog-{}.jsonl", std::process::id()));
    let cwd = env::current_exe().unwrap().parent().unwrap().to_owned();
    let args = ["", r#"a "quote" \"#, "例"];
    let log = Arc::new(WireLog::with_file(250, &path).unwrap());
    let mut spec = fixture("fixture-echo");
    spec.cwd = Some(cwd.clone());
    spec.args.extend(args.into_iter().map(Into::into));
    spec.env
        .insert("VETRO_TEST_OVERRIDE".into(), "override value".into());
    let (process, mut stdout) = Process::spawn(spec, Arc::clone(&log)).unwrap();
    let startup: Value = serde_json::from_str(&line(&mut stdout).await).unwrap();
    assert_eq!(startup["args"], json!(args));
    assert_eq!(startup["env"], "override value");
    assert_eq!(startup["cwd"], json!(cwd));
    assert!(process.send_line("two\nlines").await.is_err());
    process.send_line(LINE).await.unwrap();
    assert_eq!(line(&mut stdout).await, LINE);
    assert!(process.stop().await.unwrap().success());
    assert_eq!(line(&mut stdout).await, "stdin closed");
    assert!(stdout.recv().await.is_none());

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let text = fs::read_to_string(&path).unwrap();
            if text.contains("fixture stderr") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let snapshot = log.snapshot();
    assert!(snapshot.len() < 6);
    assert!(snapshot.iter().any(|entry| entry.text == "stdin closed"));
    assert!(log.file_error().is_none());
    let entries: Vec<RawEntry> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|entry| serde_json::from_str(entry).unwrap())
        .collect();
    let spawn = &entries[0];
    assert_eq!(spawn.direction, RawDirection::Spawn);
    let spawn: Value = serde_json::from_str(&spawn.text).unwrap();
    assert_eq!(spawn["env"], json!({"VETRO_TEST_OVERRIDE":"<redacted>"}));
    for (direction, text) in [
        (RawDirection::Sent, LINE),
        (RawDirection::Received, LINE),
        (RawDirection::Stderr, "fixture stderr"),
    ] {
        assert!(entries
            .iter()
            .any(|entry| entry.direction == direction && entry.text == text));
    }
    drop((process, log));
    fs::remove_file(path).unwrap();
}

fn rpc(versioned: bool) {
    fn send(mut message: Value, versioned: bool) {
        if versioned {
            message["jsonrpc"] = json!("2.0");
        }
        emit(message);
    }

    emit("non-JSON stdout");
    send(
        json!({"method":"unknown/notification","params":{"value":1}}),
        versioned,
    );
    send(
        json!({"id":"server-1","method":"unknown/request"}),
        !versioned,
    );
    send(json!({"id":null,"method":"another/request"}), versioned);
    for line in io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        assert_eq!(
            message.get("jsonrpc"),
            versioned.then(|| json!("2.0")).as_ref()
        );
        match message["method"].as_str() {
            Some("echo") => send(
                json!({"id":message["id"],"result":message["params"].clone()}),
                !versioned,
            ),
            Some("error") => send(
                json!({
                    "id":message["id"],
                    "error":{"code":-32000,"message":"remote failure","data":{"reason":"fixture"}}
                }),
                versioned,
            ),
            Some("client/notification") => send(json!({"method":"notified"}), versioned),
            Some("disconnect") => return,
            None if message["id"] == "server-1" && message.get("result") == Some(&Value::Null) => {
                send(json!({"method":"answered"}), !versioned);
            }
            None if message["id"].is_null() && message["error"]["code"] == -32601 => {
                send(json!({"method":"rejected"}), versioned);
            }
            _ => panic!("unexpected RPC message: {message}"),
        }
    }
}

async fn json_rpc(version: Version) {
    let mut spec = fixture("fixture-rpc");
    spec.args.push(match version {
        Version::V2 => "v2".into(),
        Version::Bare => "bare".into(),
    });
    let (process, stdout) = Process::spawn(spec, Arc::default()).unwrap();
    let (peer, mut incoming) = Peer::new(process, stdout, version);
    assert_eq!(
        incoming.recv().await,
        Some(Incoming::Notification {
            method: "unknown/notification".into(),
            params: Some(json!({"value":1})),
        })
    );
    assert_eq!(
        incoming.recv().await,
        Some(Incoming::Request {
            id: Id::String("server-1".into()),
            method: "unknown/request".into(),
            params: None,
        })
    );
    assert_eq!(
        incoming.recv().await,
        Some(Incoming::Request {
            id: Id::Null,
            method: "another/request".into(),
            params: None,
        })
    );
    peer.respond(Id::String("server-1".into()), Ok(Value::Null))
        .await
        .unwrap();
    peer.respond(
        Id::Null,
        Err(RemoteError {
            code: -32601,
            message: "Method not found".into(),
            data: None,
        }),
    )
    .await
    .unwrap();
    peer.notify("client/notification", None).await.unwrap();
    for method in ["answered", "rejected", "notified"] {
        assert_eq!(
            incoming.recv().await,
            Some(Incoming::Notification {
                method: method.into(),
                params: None
            })
        );
    }
    assert_eq!(
        peer.request("echo", Some(json!({"test":42})))
            .await
            .unwrap(),
        json!({"test":42})
    );
    assert!(matches!(
        peer.request("error", None).await,
        Err(Error::Remote(RemoteError { code: -32000, data: Some(data), .. }))
        if data == json!({"reason":"fixture"})
    ));
    assert!(matches!(
        peer.request("disconnect", None).await,
        Err(Error::Closed)
    ));
    assert!(peer.wait().await.unwrap().success());
    assert!(incoming.recv().await.is_none());
}

enum TreeShutdown {
    Drop,
    Kill,
    Stop,
}

async fn tree_shutdown(shutdown: TreeShutdown, mode: &str) {
    let (process, mut stdout) = spawn(mode, Arc::default());
    let mut tree = vec![TrackedProcess::new(process.id())];
    tree.extend(
        line(&mut stdout)
            .await
            .split_whitespace()
            .map(|pid| TrackedProcess::new(pid.parse().unwrap())),
    );
    let (other, _) = spawn("fixture-leaf", Arc::default());
    let other_handle = TrackedProcess::new(other.id());
    match shutdown {
        TreeShutdown::Drop => drop(process),
        TreeShutdown::Kill => assert!(!process.kill().await.unwrap().success()),
        TreeShutdown::Stop => assert!(!process.stop().await.unwrap().success()),
    }
    assert_dead(&tree).await;
    assert!(!other_handle.exited());
    other.kill().await.unwrap();
}

async fn host_death(mode: &str, expected: usize) {
    let mut host = tokio::process::Command::new(env::current_exe().unwrap())
        .arg("fixture-host")
        .arg(mode)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    use tokio::io::AsyncBufReadExt;
    let mut output = tokio::io::BufReader::new(host.stdout.take().unwrap());
    let mut ids = String::new();
    output.read_line(&mut ids).await.unwrap();
    let tree: Vec<_> = ids
        .split_whitespace()
        .map(|pid| TrackedProcess::new(pid.parse().unwrap()))
        .collect();
    assert_eq!(tree.len(), expected);
    host.kill().await.unwrap();
    assert_dead(&tree).await;
}

async fn assert_dead(tree: &[TrackedProcess]) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !tree.iter().all(TrackedProcess::exited) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a member of the process tree survived");
}

#[cfg(windows)]
struct TrackedProcess(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl TrackedProcess {
    fn new(pid: u32) -> Self {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!process.is_null(), "{}", io::Error::last_os_error());
        Self(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process) })
    }

    fn exited(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
        };
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }
}

#[cfg(unix)]
struct TrackedProcess(libc::pid_t);

#[cfg(unix)]
impl TrackedProcess {
    fn new(pid: u32) -> Self {
        let pid = pid as libc::pid_t;
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        Self(pid)
    }

    fn exited(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", self.0)) {
            if stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
            {
                return true;
            }
        }
        (unsafe { libc::kill(self.0, 0) } == -1)
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}
