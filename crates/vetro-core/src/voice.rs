use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use base64::Engine;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use vetro_agent_api::{
    process::{Process, SpawnSpec},
    AgentError, Result, WireLog,
};
use vetro_proto::{
    rpc::{
        ErrorCode, ServerEvent, VoiceSettings, VoiceStatus, VoiceTranscribeParams,
        VoiceTranscribeResult,
    },
    RawDirection,
};

use crate::{bad_request, internal};

const MODEL_NAME: &str = "SenseVoice-Small (int8)";
const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(60);

struct Asset {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
    bytes: u64,
}

const MODEL_ASSET: Asset = Asset {
    name: "model",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2",
    sha256: "7305f7905bfcf77fa0b39388a313f3da35c68d971661a65475b56fb2162c8e63",
    bytes: 165_783_878,
};

fn runtime_asset() -> Option<Asset> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some(Asset {
            name: "runtime",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.3/sherpa-onnx-v2.0.3-win-x64-shared-MT-Release-no-tts.tar.bz2",
            sha256: "e1536db1fcd1dde829f3cf26dfdbf60909aee6984983f6cd16b03b1352ac43fc",
            bytes: 30_850_705,
        })
    }
    #[cfg(target_os = "macos")]
    {
        Some(Asset {
            name: "runtime",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.3/sherpa-onnx-v2.0.3-osx-universal2-shared-no-tts.tar.bz2",
            sha256: "9fa9f41b28b961a32ff538ae640dd54f7e92c3c2942d1a41728f36153e747ce5",
            bytes: 43_399_439,
        })
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(Asset {
            name: "runtime",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.3/sherpa-onnx-v2.0.3-linux-x64-shared-no-tts.tar.bz2",
            sha256: "c16987540bc90cd10bab271023b2dcfb0864892b5af74cb1c9a9c7f1c4efe3af",
            bytes: 27_264_882,
        })
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some(Asset {
            name: "runtime",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.3/sherpa-onnx-v2.0.3-linux-aarch64-shared-cpu.tar.bz2",
            sha256: "c42748225180235bdd4ba10f8c4575b54beb8d5b26a5303bb3051026a0fe4e53",
            bytes: 29_516_176,
        })
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "macos",
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    {
        None
    }
}

#[derive(Default)]
pub(crate) struct VoiceManager {
    install_lock: AsyncMutex<()>,
    installing: AtomicBool,
    last_error: Mutex<Option<String>>,
}

struct InstalledLocal {
    binary: PathBuf,
    lib_dirs: Vec<PathBuf>,
    model: PathBuf,
    tokens: PathBuf,
}

impl VoiceManager {
    pub(crate) fn status(&self, root: &Path, settings: &VoiceSettings) -> VoiceStatus {
        let runtime = runtime_asset();
        let supported = runtime.is_some();
        let download_bytes = runtime.as_ref().map_or(0, |asset| asset.bytes) + MODEL_ASSET.bytes;
        let local = locate_installed(root);
        let installed = local.is_some();
        let installed_bytes = if root.is_dir() { dir_size(root) } else { 0 };
        let endpoint = configured_endpoint(settings);
        let mode = if endpoint.is_some() {
            "endpoint"
        } else if installed {
            "local"
        } else {
            "uninstalled"
        };
        VoiceStatus {
            supported,
            installed,
            installing: self.installing.load(Ordering::Acquire),
            ready: endpoint.is_some() || installed,
            mode: mode.into(),
            model_name: MODEL_NAME.into(),
            download_bytes,
            installed_bytes,
            error: self.last_error.lock().unwrap().clone(),
        }
    }

    pub(crate) async fn install<F>(
        &self,
        root: PathBuf,
        settings: &VoiceSettings,
        publish: F,
    ) -> Result<VoiceStatus>
    where
        F: Fn(ServerEvent) + Send + Sync + 'static,
    {
        let runtime = runtime_asset().ok_or_else(|| {
            bad_request("Automatic local voice installation is not available for this platform")
        })?;
        let Ok(_guard) = self.install_lock.try_lock() else {
            return Err(AgentError::new(
                ErrorCode::Busy,
                "Voice model installation is already running",
            ));
        };
        self.installing.store(true, Ordering::Release);
        *self.last_error.lock().unwrap() = None;
        let total_bytes = runtime.bytes + MODEL_ASSET.bytes;
        let result = self
            .install_inner(&root, runtime, total_bytes, &publish)
            .await;
        self.installing.store(false, Ordering::Release);
        match result {
            Ok(()) => {
                publish(ServerEvent::VoiceProgress {
                    stage: "ready".into(),
                    downloaded_bytes: total_bytes,
                    total_bytes,
                    done: true,
                    error: None,
                });
                Ok(self.status(&root, settings))
            }
            Err(error) => {
                *self.last_error.lock().unwrap() = Some(error.message.clone());
                publish(ServerEvent::VoiceProgress {
                    stage: "error".into(),
                    downloaded_bytes: 0,
                    total_bytes,
                    done: true,
                    error: Some(error.message.clone()),
                });
                Err(error)
            }
        }
    }

    async fn install_inner<F>(
        &self,
        root: &Path,
        runtime: Asset,
        total_bytes: u64,
        publish: &F,
    ) -> Result<()>
    where
        F: Fn(ServerEvent) + Send + Sync,
    {
        fs::create_dir_all(root).map_err(internal)?;
        let client = reqwest::Client::builder()
            .user_agent(format!("vetro/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(internal)?;

        let mut base_offset = 0u64;
        if find_runtime(root).is_none() {
            download_and_extract(&client, root, &runtime, base_offset, total_bytes, publish)
                .await?;
        }
        base_offset += runtime.bytes;

        if find_model(root).is_none() {
            download_and_extract(
                &client,
                root,
                &MODEL_ASSET,
                base_offset,
                total_bytes,
                publish,
            )
            .await?;
        }

        if locate_installed(root).is_none() {
            return Err(internal(
                "Downloaded voice archives did not contain the expected binary and model files",
            ));
        }
        Ok(())
    }

    pub(crate) async fn uninstall(
        &self,
        root: &Path,
        settings: &VoiceSettings,
    ) -> Result<VoiceStatus> {
        let Ok(_guard) = self.install_lock.try_lock() else {
            return Err(AgentError::new(
                ErrorCode::Busy,
                "Cannot remove voice files while installation is running",
            ));
        };
        *self.last_error.lock().unwrap() = None;
        if root.exists() {
            let root_owned = root.to_path_buf();
            tokio::task::spawn_blocking(move || fs::remove_dir_all(&root_owned))
                .await
                .map_err(internal)?
                .map_err(internal)?;
        }
        Ok(self.status(root, settings))
    }

    pub(crate) async fn transcribe(
        &self,
        root: &Path,
        settings: &VoiceSettings,
        params: VoiceTranscribeParams,
    ) -> Result<VoiceTranscribeResult> {
        let started = Instant::now();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(params.audio_base64.trim())
            .map_err(|error| bad_request(format!("Invalid base64 audio payload: {error}")))?;
        validate_wav(&bytes)?;

        let language = normalize_language(params.language.as_deref());
        let raw_text = if let Some(endpoint) = configured_endpoint(settings) {
            transcribe_endpoint(
                &endpoint,
                settings.endpoint_model.as_deref(),
                language,
                bytes,
            )
            .await?
        } else {
            let installed = locate_installed(root).ok_or_else(|| {
                AgentError::new(
                    ErrorCode::AgentUnavailable,
                    "Local voice model is not installed",
                )
            })?;
            transcribe_local(root, &installed, language, &bytes).await?
        };

        let cleaned = strip_sensevoice_tags(&raw_text);
        let text = apply_replacements(&cleaned, &settings.replacements);
        Ok(VoiceTranscribeResult {
            text,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

pub(crate) fn validate_settings(voice: &VoiceSettings) -> Result<()> {
    if let Some(raw) = voice
        .endpoint_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let url = reqwest::Url::parse(raw)
            .map_err(|error| bad_request(format!("Invalid voice endpoint URL: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(bad_request("Voice endpoint URL must use http or https"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad_request(
                "Voice endpoint URL must not contain embedded credentials",
            ));
        }
    }
    Ok(())
}

fn configured_endpoint(settings: &VoiceSettings) -> Option<String> {
    settings
        .endpoint_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn normalize_language(language: Option<&str>) -> Option<&'static str> {
    match language.map(str::trim) {
        Some("zh") => Some("zh"),
        Some("en") => Some("en"),
        Some("ja") => Some("ja"),
        Some("ko") => Some("ko"),
        Some("yue") => Some("yue"),
        _ => None,
    }
}

fn validate_wav(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err(bad_request("Audio clip exceeds the 16 MB limit"));
    }
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad_request("Audio must be a 16 kHz mono PCM WAV recording"));
    }
    Ok(())
}

async fn download_and_extract<F>(
    client: &reqwest::Client,
    root: &Path,
    asset: &Asset,
    base_offset: u64,
    total_bytes: u64,
    publish: &F,
) -> Result<()>
where
    F: Fn(ServerEvent) + Send + Sync,
{
    publish(ServerEvent::VoiceProgress {
        stage: format!("downloading_{}", asset.name),
        downloaded_bytes: base_offset,
        total_bytes,
        done: false,
        error: None,
    });
    let response = client.get(asset.url).send().await.map_err(|error| {
        AgentError::new(
            ErrorCode::AgentUnavailable,
            format!("Download failed: {error}"),
        )
    })?;
    if !response.status().is_success() {
        return Err(AgentError::new(
            ErrorCode::AgentUnavailable,
            format!("Download returned HTTP {}", response.status()),
        ));
    }

    let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(internal)?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    let mut last_emit = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            AgentError::new(
                ErrorCode::AgentUnavailable,
                format!("Download interrupted: {error}"),
            )
        })?;
        tmp.as_file_mut().write_all(&chunk).map_err(internal)?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        if downloaded.saturating_sub(last_emit) >= 262_144 {
            last_emit = downloaded;
            publish(ServerEvent::VoiceProgress {
                stage: format!("downloading_{}", asset.name),
                downloaded_bytes: (base_offset + downloaded).min(total_bytes),
                total_bytes,
                done: false,
                error: None,
            });
        }
    }
    tmp.as_file().sync_all().map_err(internal)?;

    let digest = format!("{:x}", hasher.finalize());
    if !digest.eq_ignore_ascii_case(asset.sha256) {
        return Err(internal(format!(
            "SHA-256 verification failed for {} (expected {}, got {digest})",
            asset.name, asset.sha256
        )));
    }

    publish(ServerEvent::VoiceProgress {
        stage: format!("extracting_{}", asset.name),
        downloaded_bytes: (base_offset + asset.bytes).min(total_bytes),
        total_bytes,
        done: false,
        error: None,
    });

    let archive_path = tmp.into_temp_path();
    let dest = root.join(asset.name);
    let staging = root.join(format!(".{}-staging", asset.name));
    let extract_archive = archive_path.to_path_buf();
    let extract_staging = staging.clone();
    let extract_dest = dest.clone();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        if extract_staging.exists() {
            fs::remove_dir_all(&extract_staging)?;
        }
        fs::create_dir_all(&extract_staging)?;
        let file = File::open(&extract_archive)?;
        let decoder = bzip2_rs::DecoderReader::new(io::BufReader::new(file));
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            if kind.is_symlink() || kind.is_hard_link() {
                continue;
            }
            entry.unpack_in(&extract_staging)?;
        }
        if extract_dest.exists() {
            fs::remove_dir_all(&extract_dest)?;
        }
        fs::rename(&extract_staging, &extract_dest)?;
        Ok(())
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    drop(archive_path);
    Ok(())
}

async fn transcribe_endpoint(
    endpoint: &str,
    model: Option<&str>,
    language: Option<&str>,
    wav_bytes: Vec<u8>,
) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(TRANSCRIBE_TIMEOUT)
        .build()
        .map_err(internal)?;
    let part = reqwest::multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(internal)?;
    let mut form = reqwest::multipart::Form::new().part("file", part).text(
        "model",
        model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("whisper-1")
            .to_owned(),
    );
    if let Some(language) = language {
        form = form.text("language", language.to_owned());
    }
    let response = client
        .post(endpoint)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            AgentError::new(
                ErrorCode::AgentUnavailable,
                format!("Voice endpoint request failed: {error}"),
            )
        })?;
    let status = response.status();
    let body = response.text().await.map_err(internal)?;
    if !status.is_success() {
        return Err(AgentError::new(
            ErrorCode::AgentUnavailable,
            format!("Voice endpoint returned HTTP {status}: {}", body.trim()),
        ));
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(text) = value["text"].as_str() {
            return Ok(text.to_owned());
        }
    }
    Ok(body)
}

async fn transcribe_local(
    root: &Path,
    installed: &InstalledLocal,
    language: Option<&str>,
    wav_bytes: &[u8],
) -> Result<String> {
    let mut wav = tempfile::Builder::new()
        .prefix("clip-")
        .suffix(".wav")
        .tempfile_in(root)
        .map_err(internal)?;
    wav.as_file_mut().write_all(wav_bytes).map_err(internal)?;
    wav.as_file().flush().map_err(internal)?;
    let wav_path = wav.path().to_path_buf();

    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 4))
        .unwrap_or(2);
    let mut spec = SpawnSpec::new(&installed.binary);
    spec.cwd = installed.binary.parent().map(Path::to_path_buf);
    spec.args.push(arg_kv("--tokens=", &installed.tokens));
    spec.args
        .push(arg_kv("--sense-voice-model=", &installed.model));
    spec.args.push("--sense-voice-use-itn=1".into());
    spec.args.push(format!("--num-threads={threads}").into());
    if let Some(language) = language {
        spec.args
            .push(format!("--sense-voice-language={language}").into());
    }
    spec.args.push(wav_path.as_os_str().to_os_string());
    inject_library_env(&mut spec, &installed.lib_dirs);

    let log = Arc::new(WireLog::new(256 * 1024));
    let (process, mut stdout) = Process::spawn(spec, Arc::clone(&log)).map_err(|error| {
        AgentError::new(
            ErrorCode::AgentUnavailable,
            format!("Could not start voice engine: {error}"),
        )
    })?;
    let drain = tokio::spawn(async move { while stdout.recv().await.is_some() {} });
    let waited = tokio::time::timeout(TRANSCRIBE_TIMEOUT, process.wait()).await;
    let status = match waited {
        Ok(result) => result.map_err(internal)?,
        Err(_) => {
            let _ = process.kill().await;
            let _ = drain.await;
            return Err(AgentError::new(
                ErrorCode::AgentUnavailable,
                "Voice transcription timed out",
            ));
        }
    };
    let _ = drain.await;
    if !status.success() {
        let tail = stderr_tail(&log);
        return Err(AgentError::new(
            ErrorCode::Internal,
            if tail.is_empty() {
                format!("Voice engine exited with {status}")
            } else {
                format!("Voice engine exited with {status}: {tail}")
            },
        ));
    }
    parse_sherpa_output(&log)
}

fn stderr_tail(log: &WireLog) -> String {
    let entries = log.snapshot();
    let mut lines: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.direction == RawDirection::Stderr)
        .map(|entry| entry.text.trim())
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() > 6 {
        lines = lines.split_off(lines.len() - 6);
    }
    lines.join(" | ")
}

fn arg_kv(prefix: &str, path: &Path) -> OsString {
    let mut value = OsString::from(prefix);
    value.push(path.as_os_str());
    value
}

fn inject_library_env(spec: &mut SpawnSpec, dirs: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }
    #[cfg(target_os = "windows")]
    let key = "PATH";
    #[cfg(target_os = "macos")]
    let key = "DYLD_LIBRARY_PATH";
    #[cfg(all(unix, not(target_os = "macos")))]
    let key = "LD_LIBRARY_PATH";

    let mut entries = dirs.to_vec();
    if let Some(existing) = std::env::var_os(key) {
        entries.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(entries) {
        spec.env.insert(OsString::from(key), joined);
    }
}

fn parse_sherpa_output(log: &WireLog) -> Result<String> {
    let entries = log.snapshot();
    let lines = entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.direction,
                RawDirection::Received | RawDirection::Stderr
            )
        })
        .map(|entry| entry.text.as_str());
    Ok(parse_sherpa_lines(lines))
}

fn parse_sherpa_lines<'a>(lines: impl DoubleEndedIterator<Item = &'a str>) -> String {
    for line in lines.rev() {
        let line = line.trim();
        if let Some(start) = line.find('{') {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line[start..]) {
                if let Some(text) = value["text"].as_str() {
                    return text.to_owned();
                }
            }
        }
    }
    String::new()
}

fn strip_sensevoice_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start + 2..].find("|>") {
            rest = &rest[start + 2 + end + 2..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

pub(crate) fn apply_replacements(text: &str, replacements: &BTreeMap<String, String>) -> String {
    if text.is_empty() || replacements.is_empty() {
        return text.to_owned();
    }
    let mut rules: Vec<(&str, &str)> = replacements
        .iter()
        .map(|(from, to)| (from.trim(), to.as_str()))
        .filter(|(from, _)| !from.is_empty())
        .collect();
    rules.sort_by_key(|a| std::cmp::Reverse(a.0.len()));

    let mut current = text.to_owned();
    for (from, to) in rules {
        if from.is_ascii() {
            current = replace_ascii_ci_bounded(&current, from, to);
        } else {
            current = current.replace(from, to);
        }
    }
    current
}

fn replace_ascii_ci_bounded(input: &str, needle: &str, replacement: &str) -> String {
    let lower_haystack = input.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let nlen = lower_needle.len();
    if nlen == 0 || input.len() < nlen {
        return input.to_owned();
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while let Some(rel) = lower_haystack[cursor..].find(&lower_needle) {
        let start = cursor + rel;
        let end = start + nlen;
        let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let right_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            out.push_str(&input[cursor..start]);
            out.push_str(replacement);
            cursor = end;
        } else {
            let next = input[start..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&input[cursor..start + next]);
            cursor = start + next;
        }
    }
    out.push_str(&input[cursor..]);
    out
}

fn locate_installed(root: &Path) -> Option<InstalledLocal> {
    let (binary, lib_dirs) = find_runtime(root)?;
    let (model, tokens) = find_model(root)?;
    Some(InstalledLocal {
        binary,
        lib_dirs,
        model,
        tokens,
    })
}

fn find_runtime(root: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
    let dir = root.join("runtime");
    let exe_name = if cfg!(windows) {
        "sherpa-onnx-offline.exe"
    } else {
        "sherpa-onnx-offline"
    };
    let binary = find_file(&dir, exe_name)?;
    let mut lib_dirs = Vec::new();
    if let Some(parent) = binary.parent() {
        lib_dirs.push(parent.to_path_buf());
        if let Some(grandparent) = parent.parent() {
            let lib = grandparent.join("lib");
            if lib.is_dir() {
                lib_dirs.push(lib);
            }
        }
    }
    Some((binary, lib_dirs))
}

fn find_model(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let dir = root.join("model");
    let model = find_file(&dir, "model.int8.onnx").or_else(|| find_file(&dir, "model.onnx"))?;
    let tokens = find_file(&dir, "tokens.txt")?;
    Some((model, tokens))
}

fn find_file(dir: &Path, target: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(target))
            {
                return Some(path);
            }
        }
    }
    None
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_wav_and_strips_sensevoice_tags() {
        assert!(validate_wav(b"not a wav").is_err());
        let mut wav = vec![0u8; 44];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        assert!(validate_wav(&wav).is_ok());
        assert_eq!(
            strip_sensevoice_tags("<|zh|><|NEUTRAL|><|Speech|>你好 Vetro 世界"),
            "你好 Vetro 世界"
        );
    }

    #[test]
    fn applies_deterministic_replacements_without_corrupting_words() {
        let rules = BTreeMap::from([
            ("claude code".into(), "Claude Code".into()),
            ("ai".into(), "AI".into()),
            ("微特罗".into(), "Vetro".into()),
        ]);
        assert_eq!(
            apply_replacements("在微特罗里用 claude code 改 main 里的 ai 配置", &rules),
            "在Vetro里用 Claude Code 改 main 里的 AI 配置"
        );
    }

    #[test]
    fn parses_sherpa_json_from_either_stdout_or_stderr() {
        let lines = [
            "Loading model...",
            r#"{"lang":"<|zh|>","text":"<|zh|>测试语音输入"}"#,
        ];
        let text = parse_sherpa_lines(lines.into_iter());
        assert_eq!(strip_sensevoice_tags(&text), "测试语音输入");
    }

    #[test]
    fn validates_voice_endpoint_url_and_rejects_credentials() {
        let mut settings = VoiceSettings::default();
        assert!(validate_settings(&settings).is_ok());
        settings.endpoint_url = Some("http://127.0.0.1:8080/v1/audio/transcriptions".into());
        assert!(validate_settings(&settings).is_ok());
        settings.endpoint_url =
            Some("http://user:pass@127.0.0.1:8080/v1/audio/transcriptions".into());
        assert!(validate_settings(&settings).is_err());
        settings.endpoint_url = Some("file:///etc/passwd".into());
        assert!(validate_settings(&settings).is_err());
    }
}
