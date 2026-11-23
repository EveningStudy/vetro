use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

pub fn search_directories() -> Vec<PathBuf> {
    let mut dirs: Vec<_> = env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .filter(|path| path.is_absolute())
        .collect();
    if let Some(home) = env::home_dir() {
        dirs.extend([
            home.join(".local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            home.join(".npm-global/bin"),
            home.join(".bun/bin"),
            home.join(".cargo/bin"),
        ]);
        for (variable, default, suffix) in [
            ("NVM_BIN", "", ""),
            ("FNM_MULTISHELL_PATH", "", "bin"),
            ("BUN_INSTALL", ".bun", "bin"),
            ("CARGO_HOME", ".cargo", "bin"),
            ("NPM_CONFIG_PREFIX", ".npm-global", "bin"),
        ] {
            if let Some(root) = env::var_os(variable)
                .map(PathBuf::from)
                .or_else(|| (!default.is_empty()).then(|| home.join(default)))
            {
                dirs.push(root.join(suffix));
            }
        }
        let nvm = env_path("NVM_DIR")
            .or_else(|| env_path("XDG_CONFIG_HOME").map(|root| root.join("nvm")))
            .unwrap_or_else(|| home.join(".nvm"));
        dirs.push(nvm.join("current/bin"));
        dirs.extend(nvm_default_bin(&nvm));
        let fnm = env_path("FNM_DIR")
            .or_else(|| env_path("XDG_DATA_HOME").map(|root| root.join("fnm")))
            .unwrap_or_else(|| {
                home.join(if cfg!(target_os = "macos") {
                    "Library/Application Support/fnm"
                } else {
                    ".local/share/fnm"
                })
            });
        for root in [fnm, home.join(".fnm")] {
            dirs.push(root.join("aliases/default/bin"));
            dirs.push(root.join("aliases/default/installation/bin"));
        }
        let volta = env_path("VOLTA_HOME").unwrap_or_else(|| home.join(".volta"));
        dirs.push(volta.join("bin"));
        dirs.extend(volta_default_bin(&volta));
    }
    dirs.into_iter().filter(|path| path.is_absolute()).collect()
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn nvm_default_bin(root: &Path) -> Option<PathBuf> {
    let aliases = root.join("alias");
    let mut value = fs::read_to_string(aliases.join("default")).ok()?;
    for _ in 0..8 {
        let alias = value.trim();
        let prefix = alias.trim_start_matches('v');
        let latest = matches!(alias, "node" | "stable");
        if latest || prefix.starts_with(|prefix: char| prefix.is_ascii_digit()) {
            return fs::read_dir(root.join("versions/node"))
                .ok()?
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name();
                    let version = name.to_str()?.strip_prefix('v')?;
                    let numbers = version
                        .split('.')
                        .map(str::parse::<u64>)
                        .collect::<Result<Vec<_>, _>>()
                        .ok()?;
                    (latest
                        || version == prefix
                        || version
                            .strip_prefix(prefix)
                            .is_some_and(|rest| rest.starts_with('.')))
                    .then(|| (numbers, entry.path()))
                })
                .max()
                .map(|(_, path)| path.join("bin"));
        }
        if alias.is_empty()
            || !Path::new(alias)
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return None;
        }
        value = fs::read_to_string(aliases.join(alias)).ok()?;
    }
    None
}

fn volta_default_bin(root: &Path) -> Option<PathBuf> {
    let platform: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("tools/user/platform.json")).ok()?).ok()?;
    let version = platform["node"]["runtime"].as_str()?;
    if !version.split('.').all(|part| part.parse::<u64>().is_ok()) {
        return None;
    }
    Some(root.join("tools/image/node").join(version).join("bin"))
}

pub fn usable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    meta.is_file()
}
