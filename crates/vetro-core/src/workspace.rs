use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use tokio::io::{AsyncRead, AsyncReadExt};
use vetro_agent_api::Result;
use vetro_proto::rpc::{
    FilePreviewContent, GitDiffResult, GitFileChange, GitStatusResult, OpenExternalParams,
    OpenTarget, WorkspaceFileResult, WorktreeInfo, WorktreesResult,
};

use crate::{bad_request, files, internal};

#[cfg(target_os = "linux")]
mod system_open;
#[cfg(not(target_os = "linux"))]
use open::that_detached as open_in_system;
#[cfg(target_os = "linux")]
use system_open::open_in_system;

const TEXT_LIMIT: usize = 256 * 1024;
const IMAGE_LIMIT: usize = 4 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Repository {
    root: PathBuf,
    config: Vec<String>,
}

impl Repository {
    pub(crate) async fn discover(cwd: &Path) -> Result<Option<Self>> {
        if !cwd.is_dir() {
            return Ok(None);
        }
        let repo = Self {
            root: cwd.to_owned(),
            config: Vec::new(),
        };
        let Some(output) = repo
            .run(&["rev-parse", "--show-toplevel"], usize::MAX)
            .await?
        else {
            return Ok(None);
        };
        let root = files::directory(Path::new(output.line()))?;
        if !files::normalize(cwd).starts_with(&root) {
            return Err(bad_request("Git work tree does not contain the workspace"));
        }
        Ok(Some(Self {
            root,
            config: Vec::new(),
        }))
    }

    pub(crate) async fn discover_worktrees(cwd: &Path) -> Result<Option<Self>> {
        let repo = Self::discover(cwd).await?;
        if repo.is_some() || !cwd.is_dir() {
            return Ok(repo);
        }
        let mut bare = Self {
            root: cwd.to_owned(),
            config: Vec::new(),
        };
        let Some(output) = bare
            .run(
                &["rev-parse", "--is-bare-repository", "--absolute-git-dir"],
                usize::MAX,
            )
            .await?
        else {
            return Ok(None);
        };
        let Some(root) = output.line().strip_prefix("true\n") else {
            return Ok(None);
        };
        bare.root = files::directory(Path::new(root))?;
        if !files::normalize(cwd).starts_with(&bare.root) {
            return Err(bad_request("Git directory does not contain the workspace"));
        }
        Ok(Some(bare))
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    async fn run(&self, args: &[&str], limit: usize) -> Result<Option<Capture>> {
        let mut cmd = tokio::process::Command::new("git");
        cmd.current_dir(&self.root)
            .args([
                "-c",
                "core.fsmonitor=false",
                "--no-optional-locks",
                "--literal-pathspecs",
            ])
            .args(&self.config)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        #[cfg(target_os = "linux")]
        vetro_agent_api::process::remove_appimage_environment(cmd.as_std_mut());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(internal(error)),
        };
        let stdout = child.stdout.take().expect("piped git stdout");
        let (status, data) = tokio::time::timeout(GIT_TIMEOUT, async {
            tokio::try_join!(child.wait(), capture_async(stdout, limit))
        })
        .await
        .map_err(|_| internal("Git command timed out"))?
        .map_err(internal)?;
        Ok(status.success().then_some(data))
    }

    async fn disable_filters(&mut self) -> Result<()> {
        let Some(keys) = self
            .run(
                &[
                    "config",
                    "--null",
                    "--name-only",
                    "--get-regexp",
                    r"^filter\..*\.(clean|process|required)$",
                ],
                usize::MAX,
            )
            .await?
        else {
            return Ok(());
        };
        for key in keys
            .data
            .split(|byte| *byte == 0)
            .filter(|key| !key.is_empty())
        {
            let key = std::str::from_utf8(key).map_err(internal)?;
            let value = if key.ends_with(".required") {
                "false"
            } else {
                ""
            };
            self.config.extend(["-c".into(), format!("{key}={value}")]);
        }
        Ok(())
    }

    async fn head(&self) -> Result<Option<Capture>> {
        self.run(&["rev-parse", "--verify", "HEAD"], 128).await
    }

    async fn empty_tree(&self) -> Result<Capture> {
        self.run(&["hash-object", "-t", "tree", "--stdin"], 128)
            .await?
            .ok_or_else(|| internal("Could not resolve Git's empty tree"))
    }

    pub(crate) async fn status(mut self) -> Result<GitStatusResult> {
        self.disable_filters().await?;
        let branch = match self.run(&["symbolic-ref", "--short", "HEAD"], 1024).await? {
            Some(branch) => Some(branch),
            None => self.run(&["rev-parse", "--short", "HEAD"], 128).await?,
        }
        .map(|branch| branch.line().to_owned());
        let status = self
            .run(
                &[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "-uall",
                    "--ignore-submodules=dirty",
                ],
                usize::MAX,
            )
            .await?
            .ok_or_else(|| internal("Could not read Git status"))?;
        let mut files = status_files(&status.data);
        if !files.is_empty() {
            let base = match self.head().await? {
                Some(head) => head,
                None => self.empty_tree().await?,
            };
            let stats = self
                .run(
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--ignore-submodules=dirty",
                        "--numstat",
                        "-z",
                        "--find-renames",
                        base.line(),
                        "--",
                    ],
                    usize::MAX,
                )
                .await?
                .ok_or_else(|| internal("Could not read Git change counts"))?;
            apply_stats(&mut files, &stats.data);
        }
        Ok(GitStatusResult {
            is_repo: true,
            root: Some(self.root.to_string_lossy().into_owned()),
            branch,
            dirty: !files.is_empty(),
            files,
        })
    }

    pub(crate) async fn diff(mut self, rel_path: &str) -> Result<GitDiffResult> {
        let path = relative_path(rel_path)?;
        let full_path = contained_path(&self.root, &path)?;
        if full_path
            .strip_prefix(&self.root)
            .is_ok_and(has_git_component)
        {
            return Err(bad_request("Git metadata is outside the work tree files"));
        }
        let path = path
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        self.disable_filters().await?;
        let head = self.head().await?;
        let old = match &head {
            Some(head) => {
                self.run(
                    &["show", "--no-textconv", &format!("{}:{path}", head.line())],
                    TEXT_LIMIT,
                )
                .await?
            }
            None => None,
        };
        let new = tokio::task::spawn_blocking(move || read_diff_file(&full_path))
            .await
            .map_err(internal)??;
        let mut result = GitDiffResult {
            path: path.clone(),
            ..Default::default()
        };
        result.binary = old.iter().chain(&new).any(|data| data.binary);
        result.truncated = old.iter().chain(&new).any(|data| data.truncated);
        if result.binary {
            return Ok(result);
        }
        let base = match head {
            Some(head) => head,
            None => self.empty_tree().await?,
        };
        let diff = self
            .run(
                &[
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--ignore-submodules=dirty",
                    base.line(),
                    "--",
                    &path,
                ],
                TEXT_LIMIT,
            )
            .await?
            .ok_or_else(|| internal("Could not read Git diff"))?;
        result.old_text = old.map(|data| data.text(&mut result.truncated));
        result.new_text = new.map(|data| data.text(&mut result.truncated));
        result.diff = (!diff.data.is_empty()).then(|| diff.text(&mut result.truncated));
        Ok(result)
    }

    pub(crate) async fn worktrees(&self) -> Result<WorktreesResult> {
        let output = self
            .run(&["worktree", "list", "--porcelain", "-z"], usize::MAX)
            .await?
            .ok_or_else(|| internal("Could not list Git worktrees"))?;
        Ok(parse_worktrees(&output.data, &self.root))
    }
}

pub async fn git_status(cwd: &Path) -> Result<GitStatusResult> {
    match Repository::discover(cwd).await? {
        Some(repo) => repo.status().await,
        None => Ok(GitStatusResult::default()),
    }
}

pub async fn git_diff(cwd: &Path, rel_path: &str) -> Result<GitDiffResult> {
    relative_path(rel_path)?;
    Repository::discover(cwd)
        .await?
        .ok_or_else(|| bad_request("Workspace is not inside a Git work tree"))?
        .diff(rel_path)
        .await
}

fn parse_worktrees(data: &[u8], root: &Path) -> WorktreesResult {
    let mut result = WorktreesResult {
        is_repo: true,
        ..Default::default()
    };
    let mut fields = data.split(|byte| *byte == 0);
    while let Some(path) = fields.find_map(|field| field.strip_prefix(b"worktree ")) {
        let path = files::normalize(Path::new(String::from_utf8_lossy(path).as_ref()));
        let is_main = result.main_root.is_none();
        let mut worktree = WorktreeInfo {
            path: path.to_string_lossy().into_owned(),
            is_main,
            is_current: path == root,
            branch: None,
            head: None,
            detached: false,
        };
        if is_main {
            result.main_root = Some(worktree.path.clone());
        }
        let mut bare = false;
        for field in fields.by_ref().take_while(|field| !field.is_empty()) {
            if let Some(head) = field.strip_prefix(b"HEAD ") {
                worktree.head = Some(String::from_utf8_lossy(head).into_owned());
            } else if let Some(branch) = field.strip_prefix(b"branch refs/heads/") {
                worktree.branch = Some(String::from_utf8_lossy(branch).into_owned());
            } else if field == b"detached" {
                worktree.detached = true;
            } else if field == b"bare" {
                bare = true;
            }
        }
        if !bare {
            result.worktrees.push(worktree);
        }
    }
    result
}

pub(crate) fn read_file(
    cwd: Option<&Path>,
    raw: &str,
    remote: bool,
) -> Result<WorkspaceFileResult> {
    let relative = if remote {
        Some(relative_path(raw)?)
    } else {
        None
    };
    let path = match &relative {
        Some(path) => path.clone(),
        None => expand_tilde(raw)?,
    };
    if !valid_preview_path(&path) {
        return Err(bad_request("Invalid file preview path"));
    }
    let target = match cwd {
        Some(cwd) => cwd.join(path),
        None if !remote && path.is_absolute() => path,
        None => return Err(bad_request("File preview requires a workspace directory")),
    };
    let canonical = fs::canonicalize(&target)
        .map(files::display_path)
        .map_err(|_| bad_request("File does not exist or cannot be opened"))?;
    check_preview_target(&canonical, cwd)?;
    if !canonical.is_file() {
        return Err(bad_request("Expected a regular file"));
    }

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(&canonical).map_err(internal)?;
    let metadata = file.metadata().map_err(internal)?;
    if !metadata.is_file() {
        return Err(bad_request("Expected a regular file"));
    }
    let resolved = files::display_path(opened_file_path(&file).map_err(internal)?);
    check_preview_target(&resolved, cwd)?;

    let extension = target
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let image = match extension.as_str() {
        "png" => Some(("image/png", b"\x89PNG\r\n\x1a\n".as_slice())),
        "jpg" | "jpeg" => Some(("image/jpeg", b"\xff\xd8\xff".as_slice())),
        "webp" => Some(("image/webp", b"RIFF".as_slice())),
        "gif" => Some(("image/gif", b"GIF".as_slice())),
        _ => None,
    };
    let limit = if image.is_some() {
        IMAGE_LIMIT
    } else {
        TEXT_LIMIT
    };
    let mut data = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(internal)?;
    let size_bytes = metadata.len().max(data.len() as u64);
    let content = if let Some((mime_type, magic)) = image {
        if size_bytes > IMAGE_LIMIT as u64 {
            unsupported("Image exceeds 4 MB preview limit")
        } else if data.starts_with(magic)
            && match extension.as_str() {
                "webp" => data.get(8..12) == Some(b"WEBP"),
                "gif" => data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
                _ => true,
            }
        {
            FilePreviewContent::Image {
                mime_type: mime_type.into(),
                base64: STANDARD.encode(data),
            }
        } else {
            unsupported("Binary file cannot be previewed")
        }
    } else {
        text_preview(&data, &extension)
    };
    let (path, resolved_path) = match (cwd, relative) {
        (Some(cwd), Some(relative)) => (
            preview_relative(&relative),
            preview_relative(resolved.strip_prefix(cwd).map_err(internal)?),
        ),
        _ => (raw.into(), resolved.to_string_lossy().into_owned()),
    };
    Ok(WorkspaceFileResult {
        path,
        resolved_path,
        size_bytes,
        content,
    })
}

fn preview_relative(path: &Path) -> String {
    path.iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn valid_preview_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !has_git_component(path)
        && path.components().all(|part| match part {
            Component::ParentDir => false,
            Component::Normal(name) => valid_preview_name(&name.to_string_lossy()),
            #[cfg(windows)]
            Component::Prefix(prefix) => {
                matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::UNC(_, _)
                ) && !prefix.as_os_str().to_string_lossy().contains('\0')
            }
            _ => true,
        })
        && (!matches!(
            path.components().next(),
            Some(Component::Prefix(_)) | Some(Component::RootDir)
        ) || path.is_absolute())
}

#[cfg(not(windows))]
fn valid_preview_name(name: &str) -> bool {
    !name.contains('\0')
}

#[cfg(windows)]
fn valid_preview_name(name: &str) -> bool {
    if name.ends_with(['.', ' ']) || name.contains(['\0', ':', '<', '>', '"', '|', '?', '*']) {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn check_preview_target(path: &Path, cwd: Option<&Path>) -> Result<()> {
    if !path.is_absolute() || !valid_preview_path(path) {
        return Err(bad_request("Invalid file preview path"));
    }
    if cwd.is_some_and(|cwd| !path.starts_with(cwd)) {
        return Err(bad_request("Path is outside the workspace"));
    }
    Ok(())
}

#[cfg(windows)]
fn opened_file_path(file: &fs::File) -> io::Result<PathBuf> {
    use std::{
        ffi::OsString,
        os::windows::{ffi::OsStringExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    let handle = file.as_raw_handle();
    let len = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut path = vec![0; len as usize];
    let count = unsafe { GetFinalPathNameByHandleW(handle, path.as_mut_ptr(), len, 0) };
    if count == 0 {
        return Err(io::Error::last_os_error());
    }
    if count >= len {
        return Err(io::Error::other("File path changed while opening"));
    }
    Ok(PathBuf::from(OsString::from_wide(&path[..count as usize])))
}

#[cfg(target_os = "linux")]
fn opened_file_path(file: &fs::File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;

    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "macos")]
fn opened_file_path(file: &fs::File) -> io::Result<PathBuf> {
    use std::{
        ffi::CStr,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };

    let mut path = [0 as libc::c_char; libc::MAXPATHLEN as usize];
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let bytes = unsafe { CStr::from_ptr(path.as_ptr()) }.to_bytes();
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn opened_file_path(_file: &fs::File) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "File preview is unavailable on this platform",
    ))
}

fn text_preview(data: &[u8], extension: &str) -> FilePreviewContent {
    let binary = || unsupported("Binary file cannot be previewed");
    let truncated = data.len() > TEXT_LIMIT;
    if data
        .iter()
        .any(|byte| *byte < b' ' && !matches!(byte, b'\n' | b'\r' | b'\t' | 0x0c))
    {
        return binary();
    }
    let text = match std::str::from_utf8(data) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&data[..error.valid_up_to()]).expect("validated UTF-8 prefix")
        }
        Err(_) => return binary(),
    };
    let mut end = text.len().min(TEXT_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let language = match extension {
        "rs" => Some("rust"),
        "ts" | "tsx" | "mts" | "cts" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "json" | "jsonl" => Some("json"),
        "md" | "markdown" => Some("markdown"),
        "toml" => Some("toml"),
        "yml" | "yaml" => Some("yaml"),
        "py" => Some("python"),
        "css" => Some("css"),
        "html" | "htm" | "xml" | "svg" => Some("xml"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "ps1" | "psm1" => Some("powershell"),
        "sql" => Some("sql"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "hpp" => Some("cpp"),
        _ => None,
    };
    FilePreviewContent::Text {
        text: text[..end].into(),
        language: language.map(str::to_owned),
        truncated,
    }
}

fn unsupported(reason: &str) -> FilePreviewContent {
    FilePreviewContent::Unsupported {
        reason: reason.into(),
    }
}

#[derive(Default)]
struct Capture {
    data: Vec<u8>,
    binary: bool,
    truncated: bool,
}

impl Capture {
    fn append(&mut self, bytes: &[u8], limit: usize) {
        let available = limit.saturating_sub(self.data.len()).min(bytes.len());
        self.data.extend_from_slice(&bytes[..available]);
        self.binary |= bytes.contains(&0);
        self.truncated |= available < bytes.len();
    }

    fn line(&self) -> &str {
        std::str::from_utf8(&self.data)
            .unwrap_or_default()
            .trim_end_matches(['\n', '\r'])
    }

    fn text(self, truncated: &mut bool) -> String {
        let mut text = String::from_utf8_lossy(&self.data).into_owned();
        *truncated |= self.truncated || text.len() > TEXT_LIMIT;
        if text.len() > TEXT_LIMIT {
            let mut end = TEXT_LIMIT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        text
    }
}

async fn capture_async(mut reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Capture> {
    let mut result = Capture::default();
    let mut chunk = [0; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            return Ok(result);
        }
        result.append(&chunk[..count], limit);
    }
}

fn read_diff_file(path: &Path) -> Result<Option<Capture>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(internal(error)),
    };
    if !metadata.is_file() {
        return Err(bad_request("Expected a regular file"));
    }
    let mut file = fs::File::open(path).map_err(internal)?;
    let mut data = Capture::default();
    let mut chunk = [0; 8192];
    loop {
        let count = file.read(&mut chunk).map_err(internal)?;
        if count == 0 {
            return Ok(Some(data));
        }
        data.append(&chunk[..count], TEXT_LIMIT);
    }
}

fn status_files(data: &[u8]) -> Vec<GitFileChange> {
    let mut records = data.split(|byte| *byte == 0);
    let mut files = Vec::new();
    while let Some(record) = records.next().filter(|record| record.len() > 3) {
        let code = &record[..2];
        let status = if code == b"??" {
            "untracked"
        } else if code.contains(&b'R') || code.contains(&b'C') {
            records.next();
            "renamed"
        } else if code.contains(&b'D') {
            "deleted"
        } else if code.contains(&b'A') {
            "added"
        } else if code.contains(&b'M') {
            "modified"
        } else {
            "other"
        };
        files.push(GitFileChange {
            path: String::from_utf8_lossy(&record[3..]).into_owned(),
            status: status.into(),
            additions: None,
            deletions: None,
        });
        if files.len() == 500 {
            break;
        }
    }
    files
}

fn apply_stats(files: &mut [GitFileChange], data: &[u8]) {
    let mut by_path = files
        .iter_mut()
        .map(|file| (file.path.clone(), file))
        .collect::<std::collections::HashMap<_, _>>();
    let mut records = data.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let (Some(added), Some(deleted), Some(mut path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if path.is_empty() {
            records.next();
            let Some(destination) = records.next() else {
                break;
            };
            path = destination;
        }
        if let Some(file) = by_path.get_mut(String::from_utf8_lossy(path).as_ref()) {
            file.additions = std::str::from_utf8(added)
                .ok()
                .and_then(|count| count.parse().ok());
            file.deletions = std::str::from_utf8(deleted)
                .ok()
                .and_then(|count| count.parse().ok());
        }
    }
}

fn has_git_component(path: &Path) -> bool {
    path.iter().any(|part| part.eq_ignore_ascii_case(".git"))
}

fn relative_path(path: &str) -> Result<PathBuf> {
    let path_buf = Path::new(path);
    if !path_buf
        .components()
        .any(|part| matches!(part, Component::Normal(_)))
        || path_buf
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        || path.contains('\0')
        || (cfg!(windows) && path.contains(':'))
        || has_git_component(path_buf)
    {
        return Err(bad_request(
            "Expected a file path relative to the repository root",
        ));
    }
    Ok(path_buf
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .collect())
}

fn contained_path(root: &Path, path: &Path) -> Result<PathBuf> {
    let target = root.join(path);
    let mut existing = target.as_path();
    loop {
        match fs::canonicalize(existing) {
            Ok(canonical) => {
                let canonical = files::display_path(canonical);
                if !canonical.starts_with(root) {
                    return Err(bad_request("Path is outside the workspace"));
                }
                let suffix = target.strip_prefix(existing).map_err(internal)?;
                return if suffix.as_os_str().is_empty() {
                    Ok(canonical)
                } else {
                    Ok(canonical.join(suffix))
                };
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && fs::symlink_metadata(existing).is_err() =>
            {
                existing = existing
                    .parent()
                    .ok_or_else(|| bad_request("Invalid workspace path"))?;
            }
            Err(_) => return Err(bad_request("Invalid workspace path")),
        }
    }
}

pub(crate) fn open_external(params: OpenExternalParams, editor: Option<&str>) -> Result<()> {
    let raw_cwd = params.cwd.trim();
    let (cwd, path) = if raw_cwd.is_empty() {
        let raw_path = params
            .path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| bad_request("Expected a workspace directory or absolute path"))?;
        let expanded = expand_tilde(raw_path)?;
        if !expanded.is_absolute() {
            return Err(bad_request("Relative path requires a workspace directory"));
        }
        let resolved = fs::canonicalize(&expanded)
            .map(files::display_path)
            .map_err(|_| bad_request("Path does not exist"))?;
        let base = if resolved.is_dir() {
            resolved.clone()
        } else {
            resolved
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| resolved.clone())
        };
        (base, Some(resolved))
    } else {
        let cwd = files::directory(Path::new(raw_cwd))?;
        let path = params
            .path
            .as_deref()
            .map(|raw| {
                let path = expand_tilde(raw)?;
                if !path.is_absolute()
                    && (path.has_root()
                        || matches!(path.components().next(), Some(Component::Prefix(_))))
                {
                    return Err(bad_request("Invalid workspace path"));
                }
                let resolved = fs::canonicalize(cwd.join(&path))
                    .map(files::display_path)
                    .map_err(|_| bad_request("Workspace path does not exist"))?;
                if !resolved.starts_with(&cwd) {
                    return Err(bad_request("Path is outside the workspace"));
                }
                Ok(resolved)
            })
            .transpose()?;
        (cwd, path)
    };
    match params.target {
        OpenTarget::Folder => match path.as_deref() {
            Some(target) if target.is_file() => reveal_file(target, &cwd).map_err(internal),
            Some(target) if target.is_dir() => open_folder(target).map_err(internal),
            _ => open_folder(&cwd).map_err(internal),
        },
        OpenTarget::Terminal => open_terminal(&cwd).map_err(internal),
        OpenTarget::Editor => {
            let target = path.as_deref().unwrap_or(&cwd);
            let editors: &[&str] = match editor.unwrap_or("auto") {
                "auto" => &["cursor", "code", "zed"],
                "cursor" => &["cursor"],
                "code" => &["code"],
                "zed" => &["zed"],
                "system" => &[],
                _ => return Err(bad_request("Unknown external editor")),
            };
            if let Some((name, program)) = editors
                .iter()
                .find_map(|name| editor_program(name).map(|program| (*name, program)))
            {
                let mut args = Vec::new();
                let mut location = target.as_os_str().to_owned();
                if path.is_some() {
                    if name != "zed" {
                        args.push("-g".into());
                    }
                    if let Some(line) = params.line {
                        location.push(format!(":{line}"));
                    }
                }
                args.push(location);
                return launch(&program, &args, &cwd).map_err(internal);
            }
            if !editors.is_empty() && editor.is_some_and(|name| name != "auto") {
                return Err(bad_request("Configured external editor was not found"));
            }
            if path.is_some() && is_executable_file(target) {
                return Err(bad_request(
                    "Refusing to launch an executable with the system opener; choose an editor in Settings",
                ));
            }
            open_in_system(target).map_err(internal)
        }
    }
}

fn expand_tilde(raw: &str) -> Result<PathBuf> {
    if raw == "~" {
        return std::env::home_dir().ok_or_else(|| bad_request("Home directory unavailable"));
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = std::env::home_dir().ok_or_else(|| bad_request("Home directory unavailable"))?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(raw))
}

fn is_executable_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "exe"
            | "bat"
            | "cmd"
            | "com"
            | "msi"
            | "ps1"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "wsf"
            | "wsh"
            | "lnk"
            | "scr"
            | "pif"
            | "app"
            | "command"
            | "sh"
            | "desktop"
            | "run"
    )
}

fn editor_program(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .filter(|dir| dir.is_absolute())
        .find_map(|dir| {
            #[cfg(windows)]
            {
                let executable = dir.join(format!("{name}.exe"));
                if executable.is_file() {
                    return Some(executable);
                }
                let shim = dir.join(format!("{name}.cmd"));
                let bundled = dir.parent()?.join(match name {
                    "code" => "Code.exe",
                    "cursor" => "Cursor.exe",
                    _ => "zed.exe",
                });
                (shim.is_file() && bundled.is_file()).then_some(bundled)
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let executable = dir.join(name);
                let metadata = executable.metadata().ok()?;
                (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                    .then_some(executable)
            }
        })
}

fn open_folder(cwd: &Path) -> io::Result<()> {
    #[cfg(windows)]
    let program = "explorer.exe";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    launch(Path::new(program), &[cwd.as_os_str().to_owned()], cwd)
}

fn reveal_file(file: &Path, fallback_dir: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut arg = OsString::from("/select,\"");
        arg.push(file.as_os_str());
        arg.push("\"");
        let child = Command::new("explorer.exe")
            .raw_arg(arg)
            .current_dir(fallback_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        drop(child);
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        launch(
            Path::new("open"),
            &[OsString::from("-R"), file.as_os_str().to_owned()],
            fallback_dir,
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        open_folder(file.parent().unwrap_or(fallback_dir))
    }
}

#[cfg(windows)]
fn open_terminal(cwd: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;

    launch(
        Path::new("wt.exe"),
        &["-d".into(), cwd.as_os_str().into()],
        cwd,
    )
    .or_else(|_| {
        for program in ["powershell.exe", "pwsh.exe"] {
            match Command::new(program)
                .current_dir(cwd)
                .creation_flags(0x00000010)
                .spawn()
            {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No terminal application found",
        ))
    })
}

#[cfg(target_os = "macos")]
fn open_terminal(cwd: &Path) -> io::Result<()> {
    launch(
        Path::new("open"),
        &["-a".into(), "Terminal".into(), cwd.as_os_str().into()],
        cwd,
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_terminal(cwd: &Path) -> io::Result<()> {
    for (program, directory_flag) in [
        ("x-terminal-emulator", None),
        ("gnome-terminal", Some("--working-directory")),
        ("konsole", Some("--workdir")),
        ("alacritty", Some("--working-directory")),
        ("kitty", Some("--directory")),
        ("xterm", None),
    ] {
        let args = directory_flag.map(|flag| [OsString::from(flag), cwd.as_os_str().into()]);
        match launch(
            Path::new(program),
            args.as_ref().map_or(&[][..], |args| &args[..]),
            cwd,
        ) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No terminal application found",
    ))
}

fn launch(program: &Path, args: &[OsString], cwd: &Path) -> io::Result<()> {
    let mut command = Command::new(program);
    #[cfg(target_os = "linux")]
    vetro_agent_api::process::remove_appimage_environment(&mut command);
    let child = command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    #[cfg(unix)]
    std::thread::spawn(move || {
        let _ = { child }.wait();
    });
    #[cfg(windows)]
    drop(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vetro_proto::rpc::ErrorCode;

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn previews_utf8_source_images_and_unsupported_files() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = files::normalize(temp.path());
        let mut source = "x".repeat(TEXT_LIMIT - 1);
        source.push_str("🙂\n");
        fs::write(cwd.join("source.rs"), &source).unwrap();
        let result = read_file(Some(&cwd), "./source.rs", true).unwrap();
        assert_eq!(result.path, "source.rs");
        assert_eq!(result.resolved_path, "source.rs");
        assert_eq!(result.size_bytes, source.len() as u64);
        assert_eq!(
            result.content,
            FilePreviewContent::Text {
                text: "x".repeat(TEXT_LIMIT - 1),
                language: Some("rust".into()),
                truncated: true,
            }
        );
        let svg = "<svg><script>example()</script></svg>";
        fs::write(cwd.join("sample.svg"), svg).unwrap();
        assert_eq!(
            read_file(Some(&cwd), "sample.svg", false).unwrap().content,
            FilePreviewContent::Text {
                text: svg.into(),
                language: Some("xml".into()),
                truncated: false,
            }
        );
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a1X8AAAAASUVORK5CYII=";
        fs::write(cwd.join("image.png"), STANDARD.decode(png).unwrap()).unwrap();
        assert_eq!(
            read_file(Some(&cwd), "image.png", true).unwrap().content,
            FilePreviewContent::Image {
                mime_type: "image/png".into(),
                base64: png.into()
            }
        );
        for (name, data) in [
            ("invalid.png", b"text in a PNG file".as_slice()),
            ("binary.bin", b"text\0with NUL".as_slice()),
            ("invalid.txt", b"invalid \xff".as_slice()),
        ] {
            fs::write(cwd.join(name), data).unwrap();
            assert_eq!(
                read_file(Some(&cwd), name, true).unwrap().content,
                unsupported("Binary file cannot be previewed"),
            );
        }
        let image = fs::OpenOptions::new()
            .write(true)
            .open(cwd.join("image.png"))
            .unwrap();
        image.set_len(IMAGE_LIMIT as u64 + 1).unwrap();
        drop(image);
        assert_eq!(
            read_file(Some(&cwd), "image.png", true).unwrap().content,
            unsupported("Image exceeds 4 MB preview limit"),
        );
    }

    #[test]
    fn preview_requires_an_allowed_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = files::normalize(temp.path());
        let cwd = root.join("project");
        fs::create_dir_all(cwd.join(".git")).unwrap();
        fs::write(cwd.join(".git/config"), "metadata").unwrap();
        fs::write(cwd.join("sample.txt"), "workspace").unwrap();
        let outside = root.join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        for path in [
            ".git/config",
            ".GIT/config",
            "../outside.txt",
            "any/../sample.txt",
            ".",
            "missing",
        ] {
            for remote in [false, true] {
                assert_eq!(
                    read_file(Some(&cwd), path, remote).unwrap_err().code,
                    ErrorCode::BadRequest
                );
            }
        }
        let absolute = cwd.join("sample.txt");
        let absolute = absolute.to_str().unwrap();
        assert_eq!(
            read_file(Some(&cwd), absolute, true).unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            read_file(Some(&cwd), absolute, false)
                .unwrap()
                .resolved_path,
            absolute
        );
        assert_eq!(
            read_file(None, outside.to_str().unwrap(), false)
                .unwrap()
                .size_bytes,
            7
        );
        assert_eq!(
            read_file(None, cwd.join(".git/config").to_str().unwrap(), false)
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            read_file(None, "sample.txt", true).unwrap_err().code,
            ErrorCode::BadRequest
        );

        #[cfg(windows)]
        for path in [
            "sample.txt:stream",
            "NUL.txt",
            r"\\.\NUL",
            r"\\?\C:\file.txt",
            "C:file.txt",
            ".git./config",
        ] {
            assert_eq!(
                read_file(Some(&cwd), path, false).unwrap_err().code,
                ErrorCode::BadRequest
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            symlink(&outside, cwd.join("escape")).unwrap();
            symlink(cwd.join(".git/config"), cwd.join("metadata")).unwrap();
            symlink(cwd.join("sample.txt"), cwd.join("alias")).unwrap();
            for path in ["escape", "metadata"] {
                assert_eq!(
                    read_file(Some(&cwd), path, true).unwrap_err().code,
                    ErrorCode::BadRequest
                );
            }
            assert_eq!(
                read_file(Some(&cwd), "alias", true).unwrap().resolved_path,
                "sample.txt"
            );
        }
    }

    #[tokio::test]
    async fn worktree_list_identifies_main_linked_and_detached_checkouts() {
        let temp = tempfile::tempdir().unwrap();
        assert!(Repository::discover_worktrees(temp.path())
            .await
            .unwrap()
            .is_none());
        let main = temp.path().join("main repo");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "--initial-branch=vetro-main"]);
        git(
            &main,
            &[
                "-c",
                "user.name=Vetro Test",
                "-c",
                "user.email=vetro-test@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "Initial",
            ],
        );
        git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "vetro-linked",
                "../linked checkout",
            ],
        );
        git(&main, &["worktree", "add", "--detach", "../detached"]);
        let linked = files::normalize(&temp.path().join("linked checkout"));
        fs::create_dir(linked.join("nested")).unwrap();
        let index = fs::read(main.join(".git/index")).unwrap();
        let worktrees = Repository::discover(&linked.join("nested"))
            .await
            .unwrap()
            .unwrap()
            .worktrees()
            .await
            .unwrap();
        assert!(worktrees.is_repo);
        assert_eq!(worktrees.worktrees.len(), 3);
        let first = &worktrees.worktrees[0];
        assert_eq!(worktrees.main_root.as_deref(), Some(first.path.as_str()));
        assert_eq!(
            files::normalize(Path::new(&first.path)),
            files::normalize(&main)
        );
        assert!(first.is_main && !first.is_current && !first.detached);
        assert_eq!(first.branch.as_deref(), Some("vetro-main"));
        assert!(first.head.is_some());
        let current = worktrees
            .worktrees
            .iter()
            .find(|entry| entry.is_current)
            .unwrap();
        assert_eq!(files::normalize(Path::new(&current.path)), linked);
        assert_eq!(current.branch.as_deref(), Some("vetro-linked"));
        assert_eq!(current.head, first.head);
        assert!(!current.is_main && !current.detached);
        let detached = worktrees
            .worktrees
            .iter()
            .find(|entry| entry.detached)
            .unwrap();
        assert_eq!(detached.branch, None);
        assert_eq!(detached.head, first.head);
        assert_eq!(fs::read(main.join(".git/index")).unwrap(), index);
        assert!(!main.join(".git/index.lock").exists());

        let bare = temp.path().join("bare.git");
        fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare"]);
        let worktrees = Repository::discover_worktrees(&bare)
            .await
            .unwrap()
            .unwrap()
            .worktrees()
            .await
            .unwrap();
        assert_eq!(
            worktrees,
            WorktreesResult {
                is_repo: true,
                main_root: Some(files::normalize(&bare).to_string_lossy().into_owned()),
                worktrees: Vec::new(),
            }
        );
    }

    #[tokio::test]
    async fn status_and_diff_cover_unborn_staged_and_unstaged_changes() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path();
        assert_eq!(git_status(cwd).await.unwrap(), GitStatusResult::default());
        git(cwd, &["init", "--initial-branch=vetro-test"]);
        git(cwd, &["config", "core.autocrlf", "false"]);
        git(cwd, &["config", "user.name", "Vetro Test"]);
        git(cwd, &["config", "user.email", "vetro-test@example.invalid"]);
        fs::write(cwd.join("tracked.txt"), "first\n").unwrap();
        git(cwd, &["add", "--", "tracked.txt"]);
        let unborn = git_status(cwd).await.unwrap();
        assert_eq!(unborn.branch.as_deref(), Some("vetro-test"));
        assert_eq!(unborn.files[0].status, "added");
        assert_eq!(unborn.files[0].additions, Some(1));
        assert_eq!(git_diff(cwd, "tracked.txt").await.unwrap().old_text, None);

        git(cwd, &["commit", "-m", "Initial"]);
        fs::write(cwd.join("tracked.txt"), "first\nstaged\n").unwrap();
        git(cwd, &["add", "--", "tracked.txt"]);
        let index_before = fs::read(cwd.join(".git/index")).unwrap();
        fs::write(cwd.join("tracked.txt"), "first\nstaged\nunstaged\n").unwrap();
        fs::write(cwd.join("untracked.txt"), "untracked\n").unwrap();
        fs::create_dir(cwd.join("nested")).unwrap();
        let status = git_status(&cwd.join("nested")).await.unwrap();
        assert!(status.is_repo && status.dirty);
        assert_eq!(
            Path::new(status.root.as_deref().unwrap()),
            files::normalize(cwd)
        );
        assert_eq!(status.files[0].path, "tracked.txt");
        assert_eq!(status.files[0].additions, Some(2));
        assert_eq!(status.files[0].deletions, Some(0));
        assert_eq!(status.files[1].status, "untracked");
        let changed = git_diff(cwd, "tracked.txt").await.unwrap();
        assert_eq!(changed.old_text.as_deref(), Some("first\n"));
        assert_eq!(
            changed.new_text.as_deref(),
            Some("first\nstaged\nunstaged\n")
        );
        assert!(changed.diff.unwrap().contains("+unstaged"));
        assert_eq!(
            git_diff(cwd, "untracked.txt")
                .await
                .unwrap()
                .new_text
                .as_deref(),
            Some("untracked\n")
        );
        assert_eq!(fs::read(cwd.join(".git/index")).unwrap(), index_before);
        assert!(!cwd.join(".git/index.lock").exists());
        for path in ["../escape", "nested/../tracked.txt", ".git/config"] {
            assert_eq!(
                git_diff(cwd, path).await.unwrap_err().code,
                ErrorCode::BadRequest
            );
        }
    }

    #[tokio::test]
    async fn diff_caps_text_and_omits_binary_content() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path();
        git(cwd, &["init"]);
        git(cwd, &["config", "core.autocrlf", "false"]);
        fs::write(cwd.join("large.txt"), "line\n".repeat(TEXT_LIMIT / 2)).unwrap();
        git(cwd, &["add", "large.txt"]);
        git(
            cwd,
            &[
                "-c",
                "user.name=Vetro Test",
                "-c",
                "user.email=vetro-test@example.invalid",
                "commit",
                "-m",
                "Initial",
            ],
        );
        fs::write(cwd.join("large.txt"), "changed\n".repeat(TEXT_LIMIT / 2)).unwrap();
        let large = git_diff(cwd, "large.txt").await.unwrap();
        assert!(large.truncated && !large.binary);
        for text in [large.old_text, large.new_text, large.diff] {
            assert_eq!(text.unwrap().len(), TEXT_LIMIT);
        }
        let mut binary = vec![b'x'; TEXT_LIMIT + 1];
        binary.push(0);
        fs::write(cwd.join("binary.bin"), binary).unwrap();
        let binary = git_diff(cwd, "binary.bin").await.unwrap();
        assert!(binary.binary && binary.truncated);
        assert!(binary.old_text.is_none() && binary.new_text.is_none() && binary.diff.is_none());
    }

    #[test]
    fn external_opener_rejects_traversal_and_unknown_editor() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir(&cwd).unwrap();
        fs::write(&outside, "").unwrap();
        let mut params = OpenExternalParams {
            cwd: cwd.to_string_lossy().into_owned(),
            target: OpenTarget::Editor,
            path: Some("../outside".into()),
            line: None,
        };
        assert_eq!(
            open_external(params.clone(), None).unwrap_err().code,
            ErrorCode::BadRequest
        );
        params.path = Some(outside.to_string_lossy().into_owned());
        assert_eq!(
            open_external(params.clone(), None).unwrap_err().code,
            ErrorCode::BadRequest
        );
        params.path = None;
        assert_eq!(
            open_external(params, Some("code --flag")).unwrap_err().code,
            ErrorCode::BadRequest
        );
        let missing = OpenExternalParams {
            cwd: String::new(),
            target: OpenTarget::Folder,
            path: Some("relative/file.txt".into()),
            line: None,
        };
        assert_eq!(
            open_external(missing, None).unwrap_err().code,
            ErrorCode::BadRequest
        );
        let exe = root.path().join("demo.exe");
        fs::write(&exe, "").unwrap();
        let blocked_exe = OpenExternalParams {
            cwd: String::new(),
            target: OpenTarget::Editor,
            path: Some(exe.to_string_lossy().into_owned()),
            line: None,
        };
        assert_eq!(
            open_external(blocked_exe, Some("system")).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }
}
