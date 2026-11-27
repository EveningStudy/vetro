use std::{
    env,
    path::{Path, PathBuf},
};
use verre_agent_api::detection::{search_directories, usable};

const EXE: &str = if cfg!(windows) { "codex.exe" } else { "codex" };

pub fn resolve_binary(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return resolve(path);
    }
    search_directories()
        .into_iter()
        .chain(env::var_os("APPDATA").map(|home| PathBuf::from(home).join("npm")))
        .find_map(|directory| resolve(&directory.join(EXE)))
        .or_else(desktop)
}

fn resolve(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if !cfg!(windows)
        || path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        if let Ok(actual) = path.canonicalize() {
            if usable(&actual)
                && actual.file_name().is_some_and(|name| name != "codex.js")
                && !windows_binary_or_shim(&actual)
            {
                return Some(if path.is_absolute() {
                    path.to_owned()
                } else {
                    actual
                });
            }
        }
    }
    if !["codex", "codex.exe", "codex.cmd", "codex.ps1"]
        .iter()
        .any(|shim| shim.eq_ignore_ascii_case(name))
    {
        return None;
    }
    let directory = path.parent()?;
    let arch = match env::consts::ARCH {
        "x86_64" => ("x64", "x86_64"),
        "aarch64" => ("arm64", "aarch64"),
        _ => return None,
    };
    let (platform, target) = match env::consts::OS {
        "windows" => ("win32", "pc-windows-msvc"),
        "linux" => ("linux", "unknown-linux-musl"),
        "macos" => ("darwin", "apple-darwin"),
        _ => return None,
    };
    let package = format!("@openai/codex-{platform}-{}", arch.0);
    let relative = PathBuf::from("vendor")
        .join(format!("{}-{target}", arch.1))
        .join("bin")
        .join(EXE);
    [
        directory.join("node_modules"),
        directory.join("../lib/node_modules"),
    ]
    .into_iter()
    .find_map(|npm| {
        let wrapper = npm.join("@openai/codex");
        [
            wrapper.join("node_modules").join(&package),
            npm.join(&package),
            wrapper,
        ]
        .into_iter()
        .find_map(|root| {
            root.join(&relative)
                .canonicalize()
                .ok()
                .filter(|path| usable(path))
        })
    })
}

fn desktop() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        ["ChatGPT", "Codex"].into_iter().find_map(|app| {
            let path = PathBuf::from(format!(
                "/Applications/{app}.app/Contents/Resources/codex-cli/bin/codex"
            ));
            usable(&path).then_some(path)
        })
    }
    #[cfg(windows)]
    {
        desktop_packages()
            .into_iter()
            .filter_map(|root| {
                let version = root
                    .file_name()?
                    .to_str()?
                    .strip_prefix("OpenAI.Codex_")?
                    .split('_')
                    .next()?;
                let version = version
                    .split('.')
                    .map(str::parse::<u64>)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                let path = root.join("app/resources/codex.exe");
                usable(&path).then_some((version, path))
            })
            .max()
            .map(|(_, path)| path)
            .or_else(|| {
                let path = PathBuf::from(env::var_os("LOCALAPPDATA")?)
                    .join("Programs/Codex/resources/codex.exe");
                usable(&path).then_some(path)
            })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    None
}

#[cfg(windows)]
fn desktop_packages() -> Vec<PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, ptr};
    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
        Storage::Packaging::Appx::{GetPackagePathByFullName, GetPackagesByPackageFamily},
    };

    let family: Vec<_> = "OpenAI.Codex_2p2nqsd0c76g0\0".encode_utf16().collect();
    let mut count = 0;
    let mut length = 0;
    let result = unsafe {
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            ptr::null_mut(),
            &mut length,
            ptr::null_mut(),
        )
    };
    if result != ERROR_INSUFFICIENT_BUFFER {
        return Vec::new();
    }
    let mut names = vec![ptr::null_mut(); count as usize];
    let mut buffer = vec![0; length as usize];
    let result = unsafe {
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            names.as_mut_ptr(),
            &mut length,
            buffer.as_mut_ptr(),
        )
    };
    if result != ERROR_SUCCESS {
        return Vec::new();
    }
    names.truncate(count as usize);
    names
        .into_iter()
        .filter_map(|name| {
            let mut length = 0;
            if unsafe { GetPackagePathByFullName(name, &mut length, ptr::null_mut()) }
                != ERROR_INSUFFICIENT_BUFFER
            {
                return None;
            }
            let mut path = vec![0; length as usize];
            (unsafe { GetPackagePathByFullName(name, &mut length, path.as_mut_ptr()) }
                == ERROR_SUCCESS)
                .then(|| {
                    PathBuf::from(OsString::from_wide(
                        &path[..length.saturating_sub(1) as usize],
                    ))
                })
        })
        .collect()
}

fn windows_binary_or_shim(path: &Path) -> bool {
    if env::consts::OS != "linux" {
        return false;
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(4096).read_to_end(&mut bytes);
    }
    let text = String::from_utf8_lossy(&bytes);
    bytes.starts_with(b"MZ")
        || (text.starts_with("#!")
            && (["node.exe", "codex.exe", "cmd.exe"]
                .iter()
                .any(|value| text.contains(value))
                || (text.contains("CYGWIN")
                    && text.contains("cygpath")
                    && text.contains("/codex.js"))))
}

pub(crate) fn missing_binary_message(configured: Option<&Path>) -> &'static str {
    if cfg!(target_os = "linux")
        && configured.map_or_else(
            || {
                search_directories().iter().any(|dir| {
                    windows_binary_or_shim(&dir.join("codex"))
                        || ["codex.cmd", "codex.exe"]
                            .iter()
                            .any(|name| dir.join(name).is_file())
                })
            },
            |path| {
                windows_binary_or_shim(path)
                    || path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
            },
        )
    {
        "The Windows Codex shim is unusable on Linux or WSL. Install a native Linux Codex binary."
    } else {
        "Codex executable not found"
    }
}
