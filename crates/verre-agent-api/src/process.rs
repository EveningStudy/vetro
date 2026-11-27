use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    future::Future,
    io,
    path::PathBuf,
    process::ExitStatus,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot, watch},
};
use verre_proto::RawDirection;

use crate::WireLog;

#[cfg(target_os = "linux")]
mod appimage;
#[cfg(target_os = "linux")]
pub use appimage::remove_appimage_environment;
#[cfg(unix)]
#[path = "process/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "process/windows.rs"]
mod platform;

const GRACE_PERIOD: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// The agent inherits the environment with these overrides. Values are masked in the log.
    pub env: BTreeMap<OsString, OsString>,
}

impl SpawnSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        }
    }
}

pub type Stdout = mpsc::Receiver<io::Result<String>>;

/// A live process identity: its id and a platform start marker that guards against id reuse.
pub type ProcessIdentity = (u32, u64);

struct WriteLine {
    text: String,
    done: oneshot::Sender<io::Result<()>>,
}

pub struct Process {
    pid: u32,
    tree: Arc<platform::Tree>,
    stdin: Mutex<Option<mpsc::Sender<WriteLine>>>,
    exited: watch::Receiver<Option<std::result::Result<ExitStatus, Arc<io::Error>>>>,
    stderr: Arc<WireLog>,
    stderr_drained: watch::Receiver<bool>,
    log: Arc<WireLog>,
    before_first_prompt: OnceLock<HashSet<ProcessIdentity>>,
}

impl Process {
    /// Launches a native executable. The caller must be on a Tokio runtime.
    /// Consume stdout concurrently, or drop its receiver to drain it to the log alone.
    pub fn spawn(spec: SpawnSpec, log: Arc<WireLog>) -> io::Result<(Self, Stdout)> {
        log.record(
            RawDirection::Spawn,
            json!({
                "program": spec.program.as_os_str().to_string_lossy(),
                "args": spec.args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
                "cwd": spec.cwd.as_ref().map(|cwd| cwd.as_os_str().to_string_lossy()),
                "env": spec.env.keys().map(|key| (key.to_string_lossy(), "<redacted>"))
                    .collect::<BTreeMap<_, _>>(),
            })
            .to_string(),
        );
        let child = platform::spawn(spec)?;
        let tree = Arc::new(child.tree);
        let (stdin, writes) = mpsc::channel(32);
        let (stdout, lines) = mpsc::channel(128);
        let (exit, exited) = watch::channel(None);

        tokio::spawn(write_lines(child.stdin, writes, Arc::clone(&log)));
        tokio::spawn(read_lines(
            child.stdout,
            Some(stdout),
            Arc::clone(&log),
            RawDirection::Received,
        ));
        let stderr = Arc::new(WireLog::new(16 * 1024));
        let tail = stderr.clone();
        let stderr_log = log.clone();
        let (drained, stderr_drained) = watch::channel(false);
        tokio::spawn(async move {
            read_stderr(child.stderr, stderr_log, tail).await;
            drained.send_replace(true);
        });

        let live_tree = Arc::clone(&tree);
        tokio::spawn(async move {
            let status = child.wait.await;
            let _ = live_tree.terminate();
            exit.send_replace(Some(status.map_err(Arc::new)));
        });

        Ok((
            Self {
                pid: child.pid,
                tree,
                stdin: Mutex::new(Some(stdin)),
                exited,
                stderr,
                stderr_drained,
                log,
                before_first_prompt: OnceLock::new(),
            },
            lines,
        ))
    }

    pub fn id(&self) -> u32 {
        self.pid
    }

    pub fn has_exited(&self) -> bool {
        self.exited.borrow().is_some()
    }

    /// Processes the agent started that are still alive, excluding the agent itself.
    pub fn descendants(&self) -> Vec<ProcessIdentity> {
        if self.has_exited() {
            return Vec::new();
        }
        let mut members = self.tree.members(self.pid);
        members.retain(|(pid, _)| *pid != self.pid);
        members
    }

    pub fn begin_user_work(&self) {
        self.before_first_prompt
            .get_or_init(|| self.descendants().into_iter().collect());
    }

    pub fn background_processes(&self) -> usize {
        self.before_first_prompt.get().map_or(0, |initial| {
            self.descendants()
                .into_iter()
                .filter(|member| !initial.contains(member))
                .count()
        })
    }

    pub fn wire_log(&self) -> Arc<WireLog> {
        Arc::clone(&self.log)
    }

    pub async fn exit_detail(&self) -> String {
        let Ok(Ok(status)) = tokio::time::timeout(GRACE_PERIOD, self.wait()).await else {
            return String::new();
        };
        let mut drained = self.stderr_drained.clone();
        let _ = tokio::time::timeout(GRACE_PERIOD, drained.wait_for(|done| *done)).await;
        let mut entries = self.stderr.snapshot();
        if entries.len() > 8 {
            entries.drain(..entries.len() - 8);
        }
        let mut result = format!(" ({status})");
        for entry in entries {
            result.push('\n');
            result.push_str(&entry.text);
        }
        result
    }

    /// Enqueues a line without its terminator and waits for the write to complete.
    /// Once enqueued, a write still runs if the waiting future is cancelled.
    pub async fn send_line(&self, line: impl Into<String>) -> io::Result<()> {
        let text = line.into();
        if text.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "send_line accepts exactly one line without a terminator",
            ));
        }
        let stdin = self
            .stdin
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(stdin_closed)?;
        let (done, written) = oneshot::channel();
        stdin
            .send(WriteLine { text, done })
            .await
            .map_err(|_| stdin_closed())?;
        drop(stdin);
        written.await.map_err(|_| stdin_closed())?
    }

    pub fn close_stdin(&self) {
        self.stdin.lock().unwrap().take();
    }

    /// Waits for the process status. Pipe readers may still be draining buffered output.
    pub fn wait(&self) -> impl Future<Output = io::Result<ExitStatus>> + Send + 'static {
        let mut exited = self.exited.clone();
        async move {
            let result = exited
                .wait_for(Option::is_some)
                .await
                .map_err(io::Error::other)?;
            match result.as_ref().unwrap() {
                Ok(status) => Ok(*status),
                Err(error) => Err(match error.raw_os_error() {
                    Some(code) => io::Error::from_raw_os_error(code),
                    None => io::Error::new(error.kind(), Arc::clone(error)),
                }),
            }
        }
    }

    pub async fn stop(&self) -> io::Result<ExitStatus> {
        self.close_stdin();
        match tokio::time::timeout(GRACE_PERIOD, self.wait()).await {
            Ok(result) => result,
            Err(_) => self.kill().await,
        }
    }

    pub async fn kill(&self) -> io::Result<ExitStatus> {
        self.close_stdin();
        self.tree.terminate()?;
        self.wait().await
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.tree.terminate();
    }
}

fn stdin_closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "agent stdin is closed")
}

async fn write_lines(
    mut stdin: impl AsyncWrite + Unpin,
    mut writes: mpsc::Receiver<WriteLine>,
    log: Arc<WireLog>,
) {
    while let Some(WriteLine { text, done }) = writes.recv().await {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\n');
        log.record(RawDirection::Sent, text);
        let result = async {
            stdin.write_all(&bytes).await?;
            stdin.flush().await
        }
        .await;
        let failed = result.is_err();
        let _ = done.send(result);
        if failed {
            break;
        }
    }
}

async fn read_lines(
    reader: impl AsyncRead + Unpin,
    mut stdout: Option<mpsc::Sender<io::Result<String>>>,
    log: Arc<WireLog>,
    direction: RawDirection,
) {
    let mut reader = BufReader::new(reader);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes).await {
            Ok(0) => break,
            Err(error) => {
                if let Some(stdout) = stdout {
                    let _ = stdout.send(Err(error)).await;
                }
                break;
            }
            Ok(_) => {}
        }
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if let Some(sender) = &stdout {
            log.record(direction, text.clone());
            if sender.send(Ok(text)).await.is_err() {
                stdout = None;
            }
        } else {
            log.record(direction, text);
        }
    }
}

async fn read_stderr(reader: impl AsyncRead + Unpin, log: Arc<WireLog>, tail: Arc<WireLog>) {
    let (sender, mut lines) = mpsc::channel(32);
    let read = read_lines(reader, Some(sender), log, RawDirection::Stderr);
    let collect = async {
        while let Some(Ok(mut line)) = lines.recv().await {
            let mut start = line.len().saturating_sub(2048);
            while !line.is_char_boundary(start) {
                start += 1;
            }
            line.drain(..start);
            tail.record(RawDirection::Stderr, line);
        }
    };
    tokio::join!(read, collect);
}
