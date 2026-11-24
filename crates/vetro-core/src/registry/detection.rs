use std::{env, path::PathBuf};
use vetro_agent_api::detection::{search_directories, usable};

fn binary_name(command: &str) -> String {
    if cfg!(windows) {
        let lower = command.to_ascii_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".bat") {
            command.to_owned()
        } else {
            format!("{command}.exe")
        }
    } else {
        command.to_owned()
    }
}

fn on_path(command: &str) -> Option<PathBuf> {
    let name = binary_name(command);
    let mut dirs = search_directories();
    #[cfg(all(feature = "acp", windows))]
    dirs.extend(windows_registry_path_dirs());
    dirs.into_iter()
        .map(|dir| dir.join(&name))
        .find(|file| usable(file))
}

#[cfg(all(feature = "acp", windows))]
fn windows_registry_path_dirs() -> Vec<PathBuf> {
    use winreg::{enums::*, RegKey};

    let mut dirs = Vec::new();
    for (hive, subkey) in [
        (HKEY_CURRENT_USER, r"Environment"),
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
    ] {
        let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(subkey, KEY_READ) else {
            continue;
        };
        let Ok(raw) = key.get_value::<String, _>("Path") else {
            continue;
        };
        for entry in raw.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let expanded = expand_windows_env(entry);
            let path = PathBuf::from(expanded);
            if path.is_absolute() {
                dirs.push(path);
            }
        }
    }
    dirs
}

#[cfg(all(feature = "acp", windows))]
fn expand_windows_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            if !name.is_empty() {
                if let Ok(value) = env::var(name) {
                    out.push_str(&value);
                } else {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push('%');
            rest = after;
            break;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(feature = "claude")]
pub(super) fn claude() -> Option<PathBuf> {
    on_path("claude").or_else(claude_desktop)
}

#[cfg(feature = "claude")]
fn claude_desktop() -> Option<PathBuf> {
    #[cfg(windows)]
    let root = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)?
        .join("Claude-Data");
    #[cfg(target_os = "macos")]
    let root = env::home_dir()?.join("Library/Application Support/Claude");

    #[cfg(any(windows, target_os = "macos"))]
    {
        let dirs = std::fs::read_dir(&root).ok()?;
        let stores = std::iter::once(root.join("claude-code")).chain(
            dirs.filter_map(std::result::Result::ok)
                .map(|entry| entry.path().join("claude-code")),
        );
        latest_claude(stores)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    None
}

#[cfg(all(feature = "claude", any(windows, target_os = "macos")))]
fn latest_claude(stores: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for store in stores {
        for version in std::fs::read_dir(store).into_iter().flatten().flatten() {
            let name = version.file_name().to_string_lossy().into_owned();
            let Some(numbers) = name
                .trim_start_matches('v')
                .split('.')
                .map(|part| part.parse::<u64>().ok())
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            for hash in std::fs::read_dir(version.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = hash.path().join(binary_name("claude"));
                if path.is_file() {
                    candidates.push((numbers.clone(), path));
                }
            }
        }
    }
    candidates.into_iter().max().map(|(_, path)| path)
}

#[cfg(feature = "acp")]
pub(super) fn acp(command: &str) -> Option<PathBuf> {
    match command {
        "devin" => on_path(command).or_else(devin_desktop),
        "kiro-cli" => on_path(command).or_else(kiro_install),
        "agy" => {
            let installed = if cfg!(windows) {
                env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .map(|root| root.join("agy/bin/agy.exe"))
                    .filter(|path| path.is_file())
            } else {
                None
            };
            on_path(command).or(installed)
        }
        _ => on_path(command),
    }
}

#[cfg(feature = "acp")]
fn kiro_install() -> Option<PathBuf> {
    #[cfg(windows)]
    let path = PathBuf::from(env::var_os("LOCALAPPDATA")?).join("Kiro-Cli/kiro-cli.exe");
    #[cfg(not(windows))]
    let path = env::home_dir()?.join(".local/bin/kiro-cli");
    usable(&path).then_some(path)
}

#[cfg(feature = "acp")]
fn devin_desktop() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        const NATIVE: &str = "resources/app/extensions/windsurf/devin/bin/devin.exe";
        devin_install_locations()
            .into_iter()
            .chain(
                env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .into_iter()
                    .flat_map(|local| [local.join("Programs/Devin"), local.join("Devin")]),
            )
            .chain(
                env::var_os("ProgramFiles")
                    .map(PathBuf::from)
                    .map(|root| root.join("Devin")),
            )
            .map(|root| root.join(NATIVE))
            .find(|file| file.is_file())
    }
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from(
            "/Applications/Devin.app/Contents/Resources/app/extensions/windsurf/devin/bin/devin",
        );
        path.is_file().then_some(path)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    None
}

#[cfg(all(feature = "acp", windows))]
fn devin_install_locations() -> Vec<PathBuf> {
    use winreg::{enums::*, RegKey};

    let path = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let mut roots = Vec::new();
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let hive = RegKey::predef(hive);
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let Ok(uninstall) = hive.open_subkey_with_flags(path, KEY_READ | view) else {
                continue;
            };
            for name in uninstall.enum_keys().flatten() {
                let Ok(entry) = uninstall.open_subkey_with_flags(name, KEY_READ | view) else {
                    continue;
                };
                let Ok(display) = entry.get_value::<String, _>("DisplayName") else {
                    continue;
                };
                if display.to_ascii_lowercase().starts_with("devin") {
                    if let Ok(location) = entry.get_value::<String, _>("InstallLocation") {
                        let root = PathBuf::from(location.trim().trim_matches('"'));
                        if root.is_absolute() {
                            roots.push(root);
                        }
                    }
                }
            }
        }
    }
    roots
}
