use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use tokio::io::{AsyncRead, AsyncReadExt};
use vetro_agent_api::Result;
use vetro_proto::rpc::{
    GitDiffResult, GitFileChange, GitStatusResult, OpenExternalParams, OpenTarget,
};

use crate::{bad_request, files, internal};

const TEXT_LIMIT: usize = 256 * 1024;
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
        let new = tokio::task::spawn_blocking(move || read_file(&full_path))
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

fn read_file(path: &Path) -> Result<Option<Capture>> {
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
            open::that_detached(target).map_err(internal)
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
    let child = Command::new(program)
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
