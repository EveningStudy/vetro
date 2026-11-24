use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use vetro_agent_api::Result;
use vetro_proto::{
    rpc::{AgentOptionsResult, RemoteDevice, Settings},
    RawEntry,
};

use crate::{internal, now};

pub struct Home {
    root: PathBuf,
    user_home: PathBuf,
    settings: Mutex<Settings>,
    state: Mutex<State>,
    log_lock: Mutex<()>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct State {
    pub local_token: String,
    pub local_session_hashes: Vec<String>,
    pub devices: BTreeMap<String, Device>,
    pub threads: BTreeMap<String, ThreadOverlay>,
    pub projects: BTreeMap<String, ProjectOverlay>,
    pub options: BTreeMap<String, AgentOptionsResult>,
    pub prefs: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Device {
    #[serde(rename = "tokenHash", alias = "secret")]
    pub token_hash: String,
    pub details: RemoteDevice,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct ThreadOverlay {
    pub title: Option<String>,
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct ProjectOverlay {
    pub name: Option<String>,
    pub pinned: bool,
    pub hidden: bool,
    #[serde(default)]
    pub added: bool,
}

impl Home {
    pub fn discover() -> Result<Self> {
        let user_home =
            std::env::home_dir().ok_or_else(|| internal("Home directory unavailable"))?;
        let root = std::env::var_os("VETRO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home.join(".vetro"));
        Self::open(root, user_home)
    }

    pub fn open(root: PathBuf, user_home: PathBuf) -> Result<Self> {
        let root = std::path::absolute(root).map_err(internal)?;
        private_dir(&root).map_err(internal)?;
        #[cfg(unix)]
        tighten(&fs::canonicalize(&root).map_err(internal)?).map_err(internal)?;
        for name in ["raw", "logs"] {
            let path = root.join(name);
            private_dir(&path).map_err(internal)?;
            let actual = fs::canonicalize(&path).map_err(internal)?;
            if actual != fs::canonicalize(&root).map_err(internal)?.join(name) {
                return Err(internal(
                    "Vetro data subdirectory redirects outside its location",
                ));
            }
        }

        let config = root.join("config.json");
        let settings = match read(&config)? {
            Some(settings) => settings,
            None => {
                let settings = Settings::default();
                write(&config, &settings)?;
                settings
            }
        };
        let state_path = root.join("state.json");
        let mut state: State = read(&state_path)?.unwrap_or_default();
        let mut changed = state.local_token.is_empty();
        if changed {
            state.local_token = secret(32)?;
        }
        for device in state.devices.values_mut() {
            if !device.token_hash.starts_with("sha256:") {
                device.token_hash = device_token_hash(&device.token_hash);
                changed = true;
            }
        }
        if changed {
            write(&state_path, &state)?;
        }
        Ok(Self {
            root,
            user_home,
            settings: Mutex::new(settings),
            state: Mutex::new(state),
            log_lock: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn user_home(&self) -> &Path {
        &self.user_home
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn local_token(&self) -> String {
        self.state.lock().unwrap().local_token.clone()
    }

    pub(crate) fn check_token(&self, token: &str) -> bool {
        equal(&self.state.lock().unwrap().local_token, token)
    }

    pub fn scratch_root(&self) -> Result<PathBuf> {
        let path = self.root.join("scratch");
        private_dir(&path).map_err(internal)?;
        let actual = fs::canonicalize(&path).map_err(internal)?;
        let expected = fs::canonicalize(&self.root)
            .map_err(internal)?
            .join("scratch");
        if actual != expected {
            return Err(internal(
                "Vetro scratch directory redirects outside its location",
            ));
        }
        Ok(crate::files::normalize(&actual))
    }

    pub fn voice_root(&self) -> Result<PathBuf> {
        let path = self.root.join("voice");
        if path.exists() {
            let actual = fs::canonicalize(&path).map_err(internal)?;
            let expected = fs::canonicalize(&self.root)
                .map_err(internal)?
                .join("voice");
            if actual != expected {
                return Err(internal(
                    "Vetro voice directory redirects outside its location",
                ));
            }
            return Ok(crate::files::normalize(&actual));
        }
        Ok(crate::files::normalize(&path))
    }

    pub fn create_scratch_dir(&self, title: &str) -> Result<PathBuf> {
        let root = self.scratch_root()?;
        let date = utc_date(now());
        let slug = scratch_slug(title);
        for _ in 0..8 {
            let suffix = secret(3)?;
            let name = if slug.is_empty() {
                format!("{date}-{suffix}")
            } else {
                format!("{date}-{slug}-{suffix}")
            };
            let dir = root.join(name);
            match fs::create_dir(&dir) {
                Ok(()) => {
                    #[cfg(unix)]
                    tighten(&dir).map_err(internal)?;
                    return Ok(crate::files::normalize(&dir));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(internal(error)),
            }
        }
        Err(internal("Could not allocate a unique scratch directory"))
    }

    pub(crate) fn new_local_session(&self) -> Result<String> {
        let token = secret(32)?;
        self.change_state(|state| {
            if state.local_session_hashes.len() >= 64 {
                state.local_session_hashes.remove(0);
            }
            state.local_session_hashes.push(device_token_hash(&token));
        })?;
        Ok(token)
    }

    pub(crate) fn check_local_session_hash(&self, hash: &str) -> bool {
        self.read_state(|state| {
            state
                .local_session_hashes
                .iter()
                .any(|stored| equal(stored, hash))
        })
    }

    pub(crate) fn read_state<T>(&self, action: impl FnOnce(&State) -> T) -> T {
        action(&self.state.lock().unwrap())
    }

    pub(crate) fn prefs(&self, device_id: &str) -> serde_json::Value {
        self.read_state(|state| state.prefs.get(device_id).cloned().unwrap_or_default())
    }

    pub(crate) fn change_state<T>(&self, action: impl FnOnce(&mut State) -> T) -> Result<T> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.clone();
        let result = action(&mut next);
        write(&self.root.join("state.json"), &next)?;
        *state = next;
        Ok(result)
    }

    pub(crate) fn update_settings<T>(
        &self,
        patch: serde_json::Value,
        validate: impl FnOnce(&Settings, &Settings) -> Result<T>,
    ) -> Result<(Settings, T)> {
        let mut settings = self.settings.lock().unwrap();
        let mut value = serde_json::to_value(&*settings).map_err(internal)?;
        merge(&mut value, patch);
        let next: Settings = serde_json::from_value(value)
            .map_err(|error| crate::bad_request(format!("Invalid settings: {error}")))?;
        let validated = validate(&settings, &next)?;
        write(&self.root.join("config.json"), &next)?;
        *settings = next.clone();
        Ok((next, validated))
    }

    pub(crate) fn read_raw(&self, thread_id: &str) -> Result<Vec<RawEntry>> {
        let path = self.raw_path(thread_id);
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(internal(error)),
        };
        BufReader::new(file)
            .lines()
            .map(|line| serde_json::from_str(&line.map_err(internal)?).map_err(internal))
            .collect()
    }

    pub fn write_raw(&self, thread_id: &str, entries: &[RawEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        atomic(&self.raw_path(thread_id), |file| {
            let mut writer = BufWriter::new(file);
            for entry in entries {
                serde_json::to_writer(&mut writer, entry)?;
                writer.write_all(b"\n")?;
            }
            writer.flush()
        })
    }

    fn raw_path(&self, thread_id: &str) -> PathBuf {
        self.root
            .join("raw")
            .join(format!("{:x}.jsonl", Sha256::digest(thread_id.as_bytes())))
    }

    pub(crate) fn log(&self, message: &str) {
        let _guard = self.log_lock.lock().unwrap();
        let path = self.root.join("logs/core.log");
        let mut previous = Vec::new();
        if let Ok(file) = File::open(&path) {
            let _ = file.take(256 * 1024).read_to_end(&mut previous);
        }
        let line = format!("{} {message}\n", now());
        let start = previous
            .len()
            .saturating_sub((256 * 1024usize).saturating_sub(line.len()));
        let start = if start > 0 {
            previous[start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(previous.len(), |offset| start + offset + 1)
        } else {
            0
        };
        let _ = atomic(&path, |file| {
            file.write_all(&previous[start..])?;
            file.write_all(line.as_bytes())
        });
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(unix)]
fn tighten(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            tighten(&entry?.path())?;
        }
    } else if metadata.is_file() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub(crate) fn secret(size: usize) -> Result<String> {
    let mut bytes = vec![0; size];
    getrandom::fill(&mut bytes).map_err(internal)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(crate) fn equal(left: &str, right: &str) -> bool {
    bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}

pub(crate) fn device_token_hash(token: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(token.as_bytes()))
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match File::open(path) {
        Ok(file) => serde_json::from_reader(BufReader::new(file))
            .map(Some)
            .map_err(|error| internal(format!("{}: {error}", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(internal(error)),
    }
}

fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic(path, |file| {
        serde_json::to_writer_pretty(&mut *file, value)?;
        file.write_all(b"\n")
    })
}

fn atomic(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(internal)?;
    write(tmp.as_file_mut()).map_err(internal)?;
    tmp.as_file().sync_all().map_err(internal)?;
    tmp.persist(path).map_err(internal)?;
    Ok(())
}

fn merge(target: &mut serde_json::Value, patch: serde_json::Value) {
    use serde_json::Value;
    if let Value::Object(fields) = patch {
        if !target.is_object() {
            *target = Value::Object(Default::default());
        }
        let target = target.as_object_mut().unwrap();
        for (key, value) in fields {
            if value.is_null() {
                target.remove(&key);
            } else {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch;
    }
}

fn utc_date(millis: i64) -> String {
    // Civil date from Unix days (Howard Hinnant's algorithm).
    let z = millis.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn scratch_slug(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() && out.len() < 24 {
                out.push('-');
            }
            dash = false;
            if out.len() >= 24 {
                break;
            }
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() {
            dash = true;
        }
    }
    out.trim_end_matches('-').to_owned()
}

#[cfg(test)]
#[test]
fn scratch_dir_creates_isolated_folders_inside_vetro_home() {
    assert_eq!(utc_date(0), "1970-01-01");
    assert_eq!(utc_date(1_795_520_000_000), "2026-11-24");
    assert_eq!(
        scratch_slug("Fix Google Drive sync!"),
        "fix-google-drive-sync"
    );
    assert_eq!(scratch_slug("我的云端硬盘空间不够"), "");

    let parent = tempfile::tempdir().unwrap();
    let home = Home::open(parent.path().join(".vetro"), parent.path().into()).unwrap();
    let first = home.create_scratch_dir("Check disk usage").unwrap();
    let second = home.create_scratch_dir("中文快速对话").unwrap();
    let root = home.scratch_root().unwrap();
    assert!(first.is_dir());
    assert!(second.is_dir());
    assert_ne!(first, second);
    assert_eq!(first.parent(), Some(root.as_path()));
    assert_eq!(second.parent(), Some(root.as_path()));
    assert!(first
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .contains("-check-disk-usage-"));
}

#[cfg(all(test, unix))]
#[test]
fn tightens_existing_directory_and_creates_private_files() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join(".vetro");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("config.json"), "{}").unwrap();
    fs::set_permissions(root.join("config.json"), fs::Permissions::from_mode(0o644)).unwrap();
    let home = Home::open(root.clone(), parent.path().into()).unwrap();
    let _index = crate::storage::Index::open(home.root()).unwrap();
    home.log("test");
    for name in ["", "raw", "logs"] {
        assert_eq!(
            fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    for name in ["config.json", "state.json", "index.db", "logs/core.log"] {
        assert_eq!(
            fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
