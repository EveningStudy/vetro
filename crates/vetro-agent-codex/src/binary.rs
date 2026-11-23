use std::{
    env,
    path::{Path, PathBuf},
};

const EXE: &str = if cfg!(windows) { "codex.exe" } else { "codex" };

pub fn resolve_binary(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return resolve(path);
    }
    env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .chain(env::var_os("APPDATA").map(|home| PathBuf::from(home).join("npm")))
        .find_map(|directory| resolve(&directory.join(EXE)))
}

fn resolve(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if !cfg!(windows)
        || path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        if let Ok(actual) = path.canonicalize() {
            if actual.is_file() && actual.file_name().is_some_and(|name| name != "codex.js") {
                return Some(actual);
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
    let npm = directory.join("node_modules");
    let wrapper = npm.join("@openai/codex");
    [
        wrapper.join("node_modules").join(&package),
        npm.join(package),
        wrapper,
    ]
    .into_iter()
    .find_map(|root| {
        root.join(&relative)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file())
    })
}
