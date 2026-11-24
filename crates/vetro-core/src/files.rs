use std::{
    fs,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use vetro_agent_api::Result;
use vetro_proto::rpc::{FsEntry, FsFindParams, FsFindResult, FsListResult};

use crate::{bad_request, internal, page_limit};

pub(crate) fn normalize(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let resolved = fs::canonicalize(&absolute).unwrap_or(absolute);
    display_path(resolved)
}

pub(crate) fn display_path(resolved: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = resolved.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            if local.as_bytes().get(1) == Some(&b':') {
                return PathBuf::from(local);
            }
        }
    }
    resolved
}

pub(crate) fn directory(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(bad_request(format!(
            "Expected an existing absolute directory: {}",
            path.display()
        )));
    }
    Ok(normalize(path))
}

pub fn mkdir(parent: &Path, name: &str) -> Result<PathBuf> {
    let parent = directory(parent)?;
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err(bad_request("Folder name cannot be empty, '.' or '..'"));
    }
    if name.contains(['/', '\\', '\0']) || name.chars().any(char::is_control) {
        return Err(bad_request("Folder name cannot contain path separators"));
    }
    #[cfg(windows)]
    {
        if name.ends_with(['.', ' ']) || name.contains(['<', '>', ':', '"', '|', '?', '*']) {
            return Err(bad_request(
                "Folder name contains invalid Windows characters",
            ));
        }
        let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        ) {
            return Err(bad_request("Folder name is reserved on Windows"));
        }
    }
    let target = parent.join(name);
    if target.exists() {
        return Err(bad_request(
            "A file or folder with that name already exists",
        ));
    }
    fs::create_dir(&target).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            bad_request("A file or folder with that name already exists")
        } else {
            bad_request(format!("Could not create folder: {error}"))
        }
    })?;
    Ok(normalize(&target))
}

pub fn list(path: &Path) -> Result<FsListResult> {
    #[cfg(windows)]
    if path.as_os_str().is_empty() {
        return Ok(FsListResult {
            path: String::new(),
            parent: None,
            entries: (b'A'..=b'Z')
                .map(|letter| format!("{}:\\", char::from(letter)))
                .filter(|path| Path::new(path).is_dir())
                .map(|path| FsEntry {
                    name: path.clone(),
                    path,
                    is_dir: true,
                })
                .collect(),
        });
    }
    let path = directory(path)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&path).map_err(internal)? {
        let entry = entry.map_err(internal)?;
        entries.push(FsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path().to_string_lossy().into_owned(),
            is_dir: entry.path().is_dir(),
        });
    }
    entries
        .sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase(), entry.name.clone()));
    let parent = path
        .parent()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| cfg!(windows).then(String::new));
    Ok(FsListResult {
        path: path.to_string_lossy().into_owned(),
        parent,
        entries,
    })
}

pub fn find(params: FsFindParams) -> Result<FsFindResult> {
    let root = directory(Path::new(&params.cwd))?;
    let query = display_separators(params.query.to_lowercase());
    let mut matches = Vec::new();
    for entry in WalkBuilder::new(&root)
        .require_git(false)
        .build()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path().strip_prefix(&root).unwrap();
        if path.as_os_str().is_empty() {
            continue;
        }
        let path = display_separators(path.to_string_lossy().into_owned());
        let lower = path.to_lowercase();
        if let Some(offset) = lower.find(&query) {
            matches.push(((0, offset, path.len()), path));
        } else if let Some(span) = subsequence(&lower, &query) {
            matches.push(((1, span, path.len()), path));
        }
    }
    matches.sort_unstable();
    Ok(FsFindResult {
        paths: matches
            .into_iter()
            .take(page_limit(params.limit, 50, 500))
            .map(|(_, path)| path)
            .collect(),
    })
}

fn display_separators(path: String) -> String {
    #[cfg(windows)]
    return path.replace('\\', "/");
    #[cfg(not(windows))]
    path
}

#[cfg(all(test, unix))]
#[test]
fn file_search_keeps_backslashes_inside_posix_file_names() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join(r"a\b.txt"), "").unwrap();
    let result = find(FsFindParams {
        cwd: root.path().to_string_lossy().into_owned(),
        query: r"a\b".into(),
        limit: None,
    })
    .unwrap();
    assert_eq!(result.paths, [r"a\b.txt"]);
}

fn subsequence(candidate: &str, query: &str) -> Option<usize> {
    let mut chars = candidate.char_indices();
    let mut first = None;
    let mut last = 0;
    for target in query.chars() {
        let (index, _) = chars.find(|&(_, character)| character == target)?;
        first.get_or_insert(index);
        last = index;
    }
    Some(last - first.unwrap_or(last))
}

#[cfg(test)]
#[test]
fn mkdir_creates_single_level_folder_and_rejects_invalid_names() {
    let root = tempfile::tempdir().unwrap();
    let created = mkdir(root.path(), "my-project").unwrap();
    assert!(created.is_dir());
    assert!(mkdir(root.path(), "my-project").is_err());
    for bad in ["", " ", ".", "..", "a/b", r"a\b"] {
        assert!(
            mkdir(root.path(), bad).is_err(),
            "expected error for {bad:?}"
        );
    }
    #[cfg(windows)]
    for bad in ["CON", "nul.txt", "bad:name", "trail."] {
        assert!(
            mkdir(root.path(), bad).is_err(),
            "expected error for {bad:?}"
        );
    }
}
