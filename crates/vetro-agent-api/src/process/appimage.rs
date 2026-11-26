use std::{
    env,
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::Command,
};

const RUNTIME_KEYS: &[&str] = &["APPDIR", "APPIMAGE", "ARGV0", "OWD"];
const PATH_LIST_KEYS: &[&str] = &[
    "PATH",
    "LD_LIBRARY_PATH",
    "XDG_DATA_DIRS",
    "GSETTINGS_SCHEMA_DIR",
    "GI_TYPELIB_PATH",
    "GIO_EXTRA_MODULES",
    "GTK_PATH",
    "PERLLIB",
    "PYTHONHOME",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH_0_10",
];
const SINGLE_PATH_KEYS: &[&str] = &[
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_PIXBUF_MODULEDIR",
    "GIO_MODULE_DIR",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GST_PLUGIN_SCANNER_1_0",
    "GST_PLUGIN_SCANNER_0_10",
    "GST_PTP_HELPER_1_0",
    "GST_PTP_HELPER_0_10",
];

fn running_bundle() -> Option<PathBuf> {
    env::var_os("APPIMAGE")?;
    let bundle = PathBuf::from(env::var_os("APPDIR")?);
    let executable = env::current_exe().ok()?;
    (bundle.is_absolute() && executable.starts_with(&bundle)).then_some(bundle)
}

pub fn remove_appimage_environment(command: &mut Command) {
    let Some(bundle) = running_bundle() else {
        return;
    };
    for key in RUNTIME_KEYS {
        if !command.get_envs().any(|(name, _)| name == *key) {
            command.env_remove(key);
        }
    }
    for &key in PATH_LIST_KEYS.iter().chain(SINGLE_PATH_KEYS) {
        let value = match command.get_envs().find(|(name, _)| *name == key) {
            Some((_, value)) => value.map(OsStr::to_owned),
            None => env::var_os(key),
        };
        let Some(value) = value else {
            continue;
        };
        let filtered = without_bundle_paths(&bundle, key, &value);
        if filtered.as_deref() != Some(value.as_os_str()) {
            if let Some(filtered) = filtered {
                command.env(key, filtered);
            } else {
                command.env_remove(key);
            }
        }
    }
}

fn without_bundle_paths(bundle: &Path, key: &str, value: &OsStr) -> Option<OsString> {
    if SINGLE_PATH_KEYS.contains(&key) {
        return (!Path::new(value).starts_with(bundle)).then(|| value.to_owned());
    }
    let segments: Vec<_> = value
        .as_bytes()
        .split(|byte| *byte == b':' || (key == "LD_LIBRARY_PATH" && *byte == b';'))
        .collect();
    let kept: Vec<_> = segments
        .iter()
        .copied()
        .filter(|segment| !Path::new(OsStr::from_bytes(segment)).starts_with(bundle))
        .collect();
    if kept.len() == segments.len() {
        return Some(value.to_owned());
    }
    if kept.is_empty() || (kept.len() == 1 && kept[0].is_empty()) {
        return None;
    }
    Some(OsString::from_vec(kept.join(&b':')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_mount_paths_and_preserves_native_paths() {
        let bundle = Path::new("/tmp/.mount_Vetro_abc");
        assert_eq!(
            without_bundle_paths(
                bundle,
                "LD_LIBRARY_PATH",
                OsStr::new("/opt/voice/lib;/tmp/.mount_Vetro_abc//usr/lib:/custom/lib")
            ),
            Some("/opt/voice/lib:/custom/lib".into())
        );
        assert_eq!(
            without_bundle_paths(
                bundle,
                "PATH",
                OsStr::new("/tmp/.mount_Vetro_abc/bin:/tmp/.mount_Vetro_abcdef/bin::/usr/bin")
            ),
            Some("/tmp/.mount_Vetro_abcdef/bin::/usr/bin".into())
        );
        assert_eq!(
            without_bundle_paths(
                bundle,
                "PYTHONHOME",
                OsStr::new("/tmp/.mount_Vetro_abc/usr")
            ),
            None
        );
        assert_eq!(
            without_bundle_paths(
                bundle,
                "LD_LIBRARY_PATH",
                OsStr::new("/tmp/.mount_Vetro_abc/usr/lib:")
            ),
            None
        );
        assert_eq!(
            without_bundle_paths(bundle, "GDK_PIXBUF_MODULE_FILE", bundle.as_os_str()),
            None
        );
        assert_eq!(
            without_bundle_paths(bundle, "PYTHONHOME", OsStr::new("/opt/native/python")),
            Some("/opt/native/python".into())
        );
    }
}
