use std::{
    collections::{BTreeMap, BTreeSet},
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
use tokio::sync::RwLock;
use vetro_agent_api::{
    process::{Process, SpawnSpec},
    AgentError, Result, WireLog,
};
use vetro_proto::{
    rpc::{
        ErrorCode, ServerEvent, VoiceModelInfo, VoiceSettings, VoiceStatus, VoiceTranscribeParams,
        VoiceTranscribeResult,
    },
    RawDirection, RawEntry,
};

use crate::{bad_request, internal};

#[cfg(windows)]
mod windows;

const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);

struct Asset {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
    bytes: u64,
}

#[derive(Clone, Copy)]
enum ModelKind {
    SenseVoice,
    Qwen3Asr,
    FunAsrNano,
}

struct ModelPreset {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    kind: ModelKind,
    asr: Asset,
    punctuation: Option<Asset>,
}

const MODELS: [ModelPreset; 3] = [
    ModelPreset {
        id: "sense-voice",
        name: "SenseVoice-Small + Punctuation",
        description: "Compact multilingual dictation with a separate Chinese and English punctuation model.",
        kind: ModelKind::SenseVoice,
        asr: Asset {
            name: "model",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2",
            sha256: "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e",
            bytes: 163_002_883,
        },
        punctuation: Some(Asset {
            name: "punctuation",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2",
            sha256: "c0d5aa5f8eeb686032345e180bedf39319dc2e0556781c6264bcadba8328a6e1",
            bytes: 64_717_756,
        }),
    },
    ModelPreset {
        id: "qwen3-asr",
        name: "Qwen3-ASR 0.6B (int8)",
        description: "Multilingual dictation with built-in punctuation and glossary support.",
        kind: ModelKind::Qwen3Asr,
        asr: Asset {
            name: "model",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2",
            sha256: "393f8a14e2f5fb96746aaab342997a40641001fbd5bf9592a080a8329178ee96",
            bytes: 878_702_423,
        },
        punctuation: None,
    },
    ModelPreset {
        id: "funasr-nano",
        name: "Fun-ASR-Nano (int8)",
        description: "Dictation with built-in punctuation, language hints and glossary support.",
        kind: ModelKind::FunAsrNano,
        asr: Asset {
            name: "model",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2",
            sha256: "eb43d7ccc2e86b243f6a03b7df361033dda66db9523d1a92bf6aca2b50c9476b",
            bytes: 841_730_611,
        },
        punctuation: None,
    },
];

impl ModelPreset {
    fn dir(&self, root: &Path) -> PathBuf {
        root.join("models").join(self.id)
    }

    fn download_bytes(&self) -> u64 {
        self.asr.bytes + self.punctuation.as_ref().map_or(0, |asset| asset.bytes)
    }
}

fn model_preset(id: Option<&str>) -> Result<&'static ModelPreset> {
    match id {
        None => Ok(&MODELS[0]),
        Some(id) => MODELS
            .iter()
            .find(|model| model.id == id)
            .ok_or_else(|| bad_request(format!("Unknown voice model: {id}"))),
    }
}

fn active_preset(settings: &VoiceSettings) -> &'static ModelPreset {
    model_preset(settings.model_id.as_deref()).unwrap_or(&MODELS[0])
}

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
    assets: RwLock<()>,
    installing: AtomicBool,
    last_error: Mutex<Option<String>>,
}

struct Runtime {
    binary: PathBuf,
    lib_dirs: Vec<PathBuf>,
}

enum ModelFiles {
    SenseVoice {
        model: PathBuf,
        tokens: PathBuf,
    },
    Qwen3Asr {
        frontend: PathBuf,
        encoder: PathBuf,
        decoder: PathBuf,
        tokenizer: PathBuf,
    },
    FunAsrNano {
        encoder_adaptor: PathBuf,
        llm: PathBuf,
        embedding: PathBuf,
        tokenizer: PathBuf,
    },
}

struct InstallProgress<'a>(&'a AtomicBool);

impl Drop for InstallProgress<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl VoiceManager {
    pub(crate) fn status(&self, root: &Path, settings: &VoiceSettings) -> VoiceStatus {
        let runtime = find_runtime(root);
        let runtime_download = runtime_asset();
        let active = active_preset(settings);
        let models: Vec<_> = MODELS
            .iter()
            .map(|model| {
                let dir = model.dir(root);
                VoiceModelInfo {
                    id: model.id.into(),
                    name: model.name.into(),
                    description: model.description.into(),
                    download_bytes: model.download_bytes(),
                    installed: find_model(&dir, model.kind).is_some()
                        && (model.punctuation.is_none() || find_punctuation_model(&dir).is_some()),
                    installed_bytes: dir_size(&dir),
                    active: model.id == active.id,
                    native_punctuation: model.punctuation.is_none(),
                }
            })
            .collect();
        let installed = models.iter().any(|model| model.active && model.installed);
        let ready = runtime.is_some() && installed;
        let endpoint = configured_endpoint(settings);
        let mode = if endpoint.is_some() {
            "endpoint"
        } else if ready {
            "local"
        } else {
            "uninstalled"
        };
        VoiceStatus {
            supported: runtime_download.is_some(),
            installed,
            installing: self.installing.load(Ordering::Acquire),
            ready: endpoint.is_some() || ready,
            mode: mode.into(),
            model_name: active.name.into(),
            download_bytes: active.download_bytes()
                + if runtime.is_none() {
                    runtime_download.as_ref().map_or(0, |asset| asset.bytes)
                } else {
                    0
                },
            installed_bytes: dir_size(root),
            error: self.last_error.lock().unwrap().clone(),
            active_model_id: active.id.into(),
            models,
        }
    }

    pub(crate) async fn install<F>(
        &self,
        root: PathBuf,
        settings: &VoiceSettings,
        model_id: Option<&str>,
        publish: F,
    ) -> Result<VoiceStatus>
    where
        F: Fn(ServerEvent) + Send + Sync + 'static,
    {
        let runtime = runtime_asset().ok_or_else(|| {
            bad_request("Automatic local voice installation is not available for this platform")
        })?;
        let model = match model_id {
            Some(id) => model_preset(Some(id))?,
            None => active_preset(settings),
        };
        let _guard = self.assets.try_write().map_err(|_| assets_busy())?;
        self.installing.store(true, Ordering::Release);
        let progress = InstallProgress(&self.installing);
        *self.last_error.lock().unwrap() = None;
        let result = self.install_inner(&root, &runtime, model, &publish).await;
        drop(progress);
        match result {
            Ok(total_bytes) => {
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
                    total_bytes: 0,
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
        runtime: &Asset,
        model: &ModelPreset,
        publish: &F,
    ) -> Result<u64>
    where
        F: Fn(ServerEvent) + Send + Sync,
    {
        fs::create_dir_all(root).map_err(internal)?;
        check_subdir(root, Path::new("models"))?;
        let dir = model.dir(root);
        check_subdir(root, &Path::new("models").join(model.id))?;
        let mut downloads = Vec::new();
        if find_runtime(root).is_none() {
            downloads.push((runtime, root.join("runtime")));
        }
        let needs_model = find_model(&dir, model.kind).is_none();
        if needs_model {
            downloads.push((&model.asr, dir.clone()));
        }
        if let Some(punctuation) = &model.punctuation {
            if needs_model || find_punctuation_model(&dir).is_none() {
                downloads.push((punctuation, dir.join("punct")));
            }
        }
        let total_bytes = downloads.iter().map(|(asset, _)| asset.bytes).sum();
        let client = reqwest::Client::builder()
            .user_agent(format!("vetro/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(internal)?;

        let mut base_offset = 0;
        for (asset, dest) in downloads {
            download_and_extract(
                &client,
                root,
                &dest,
                asset,
                base_offset,
                total_bytes,
                publish,
            )
            .await?;
            base_offset += asset.bytes;
        }

        if find_runtime(root).is_none()
            || find_model(&dir, model.kind).is_none()
            || (model.punctuation.is_some() && find_punctuation_model(&dir).is_none())
        {
            return Err(internal(
                "Downloaded voice archives did not contain the expected binary and model files",
            ));
        }
        Ok(total_bytes)
    }

    pub(crate) async fn uninstall(
        &self,
        root: &Path,
        settings: &VoiceSettings,
        model_id: Option<&str>,
    ) -> Result<VoiceStatus> {
        let _guard = self.assets.try_write().map_err(|_| assets_busy())?;
        *self.last_error.lock().unwrap() = None;
        if let Some(id) = model_id {
            let model = model_preset(Some(id))?;
            check_subdir(root, Path::new("models"))?;
            remove_dir(&model.dir(root)).map_err(internal)?;
            if !MODELS.iter().any(|model| model.dir(root).is_dir()) {
                remove_dir(&root.join("runtime")).map_err(internal)?;
            }
        } else {
            remove_dir(root).map_err(internal)?;
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
            let _guard = self.assets.try_read().map_err(|_| assets_busy())?;
            let model = active_preset(settings);
            let dir = model.dir(root);
            let unavailable = || {
                AgentError::new(
                    ErrorCode::AgentUnavailable,
                    "Local voice model is not installed",
                )
            };
            let runtime = find_runtime(root).ok_or_else(unavailable)?;
            let files = find_model(&dir, model.kind).ok_or_else(unavailable)?;
            let text = transcribe_local(root, &runtime, &files, settings, language, &bytes).await?;
            if matches!(files, ModelFiles::SenseVoice { .. }) {
                punctuate(root, &runtime, &dir, strip_sensevoice_tags(&text)).await
            } else {
                text
            }
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
    model_preset(voice.model_id.as_deref())?;
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

fn assets_busy() -> AgentError {
    AgentError::new(ErrorCode::Busy, "Local voice files are in use")
}

fn remove_dir(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn check_subdir(root: &Path, relative: &Path) -> Result<()> {
    match root.join(relative).canonicalize() {
        Ok(actual) if actual != root.canonicalize().map_err(internal)?.join(relative) => Err(
            internal("Vetro voice subdirectory redirects outside its location"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(internal(error)),
    }
}

async fn download_and_extract<F>(
    client: &reqwest::Client,
    root: &Path,
    dest: &Path,
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
        downloaded += chunk.len() as u64;
        if downloaded > asset.bytes {
            return Err(internal(format!("Unexpected size for {}", asset.name)));
        }
        tmp.as_file_mut().write_all(&chunk).map_err(internal)?;
        hasher.update(&chunk);
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
    if downloaded != asset.bytes {
        return Err(internal(format!("Unexpected size for {}", asset.name)));
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
    let staging = tempfile::tempdir_in(root).map_err(internal)?;
    let staging = tokio::task::spawn_blocking(move || -> io::Result<tempfile::TempDir> {
        let extract_dir = staging.path().join("files");
        fs::create_dir(&extract_dir)?;
        let file = File::open(&archive_path)?;
        let decoder = bzip2_rs::DecoderReader::new(io::BufReader::new(file));
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            if kind.is_symlink() || kind.is_hard_link() {
                continue;
            }
            entry.unpack_in(&extract_dir)?;
        }
        Ok(staging)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(internal)?;
    }
    remove_dir(dest).map_err(internal)?;
    fs::rename(staging.path().join("files"), dest).map_err(internal)?;
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
    runtime: &Runtime,
    files: &ModelFiles,
    settings: &VoiceSettings,
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
    let mut spec = engine_spec(&runtime.binary, runtime);
    spec.args = transcription_args(files, settings, language);
    spec.args.push(format!("--num-threads={threads}").into());
    spec.args.push(wav_path.as_os_str().to_os_string());
    let log = run_engine(spec).await?;
    Ok(parse_sherpa_output(&log))
}

fn transcription_args(
    files: &ModelFiles,
    settings: &VoiceSettings,
    language: Option<&str>,
) -> Vec<OsString> {
    let (mut args, hotwords_flag) = match files {
        ModelFiles::SenseVoice { model, tokens } => (
            vec![
                arg_kv("--sense-voice-model=", model),
                arg_kv("--tokens=", tokens),
                "--sense-voice-use-itn=true".into(),
                format!("--sense-voice-language={}", language.unwrap_or("auto")).into(),
            ],
            None,
        ),
        ModelFiles::Qwen3Asr {
            frontend,
            encoder,
            decoder,
            tokenizer,
        } => (
            vec![
                arg_kv("--qwen3-asr-conv-frontend=", frontend),
                arg_kv("--qwen3-asr-encoder=", encoder),
                arg_kv("--qwen3-asr-decoder=", decoder),
                arg_kv("--qwen3-asr-tokenizer=", tokenizer),
                "--qwen3-asr-max-new-tokens=512".into(),
                "--qwen3-asr-max-total-len=1024".into(),
            ],
            Some("--qwen3-asr-hotwords"),
        ),
        ModelFiles::FunAsrNano {
            encoder_adaptor,
            llm,
            embedding,
            tokenizer,
        } => {
            let mut args = vec![
                arg_kv("--funasr-nano-encoder-adaptor=", encoder_adaptor),
                arg_kv("--funasr-nano-llm=", llm),
                arg_kv("--funasr-nano-embedding=", embedding),
                arg_kv("--funasr-nano-tokenizer=", tokenizer),
                "--funasr-nano-itn=true".into(),
                "--funasr-nano-max-new-tokens=512".into(),
            ];
            if let Some(language) = language {
                args.push(format!("--funasr-nano-language={language}").into());
            }
            (args, Some("--funasr-nano-hotwords"))
        }
    };
    if let Some(flag) = hotwords_flag {
        let words = glossary_hotwords(&settings.replacements);
        if !words.is_empty() {
            args.push(format!("{flag}={words}").into());
        }
    }
    args
}

fn glossary_hotwords(replacements: &BTreeMap<String, String>) -> String {
    replacements
        .values()
        .map(|word| word.trim())
        .filter(|word| {
            !word.is_empty() && word.len() <= 96 && !word.contains([',', '\r', '\n', '\0'])
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(32)
        .collect::<Vec<_>>()
        .join(",")
}

async fn punctuate(_root: &Path, runtime: &Runtime, dir: &Path, text: String) -> String {
    if text.is_empty() || text.contains(['。', '！', '？', '.', '!', '?', ',', '，']) {
        return text;
    }
    let Some(model) = find_punctuation_model(dir) else {
        return text;
    };
    let binary = runtime
        .binary
        .with_file_name(executable("sherpa-onnx-offline-punctuation"));
    if !binary.is_file() {
        return text;
    }
    #[cfg(windows)]
    let utf8_copy = if !text.is_ascii() || !model.as_os_str().is_ascii() {
        match windows::utf8_punctuation(_root, &binary) {
            Ok(copy) => Some(copy),
            Err(_) => return text,
        }
    } else {
        None
    };
    #[cfg(windows)]
    let binary = utf8_copy.as_ref().map_or(&binary, |(_, path)| path);
    #[cfg(not(windows))]
    let binary = &binary;
    let mut spec = engine_spec(binary, runtime);
    spec.args = vec![
        "--print-args=false".into(),
        arg_kv("--ct-transformer=", &model),
        "--".into(),
        text.as_str().into(),
    ];
    if let Ok(log) = run_engine(spec).await {
        if let Some(punctuated) = parse_punctuation_output(&log.snapshot()) {
            return punctuated;
        }
    }
    text
}

fn parse_punctuation_output(entries: &[RawEntry]) -> Option<String> {
    let mut lines = entries
        .iter()
        .filter(|entry| entry.direction == RawDirection::Received)
        .map(|entry| entry.text.trim())
        .filter(|line| !line.is_empty());
    let text = lines.next()?;
    (lines.next().is_none() && !text.contains('\u{fffd}')).then(|| text.to_owned())
}

fn engine_spec(binary: &Path, runtime: &Runtime) -> SpawnSpec {
    let mut spec = SpawnSpec::new(binary);
    spec.cwd = binary.parent().map(Path::to_path_buf);
    inject_library_env(&mut spec, &runtime.lib_dirs);
    spec
}

async fn run_engine(spec: SpawnSpec) -> Result<Arc<WireLog>> {
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
    let detail = process.exit_detail().await;
    if !status.success() {
        return Err(AgentError::new(
            ErrorCode::Internal,
            format!("Voice engine exited{detail}"),
        ));
    }
    Ok(log)
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

fn parse_sherpa_output(log: &WireLog) -> String {
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
    parse_sherpa_lines(lines)
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

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn find_runtime(root: &Path) -> Option<Runtime> {
    let files = files_in(&root.join("runtime"));
    let binary = find_named(&files, &executable("sherpa-onnx-offline"))?;
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
    Some(Runtime { binary, lib_dirs })
}

fn find_model(dir: &Path, kind: ModelKind) -> Option<ModelFiles> {
    let mut files = files_in(dir);
    match kind {
        ModelKind::SenseVoice => {
            let punct = dir.join("punct");
            files.retain(|path| !path.starts_with(&punct));
            Some(ModelFiles::SenseVoice {
                model: find_named(&files, "model.int8.onnx")
                    .or_else(|| find_named(&files, "model.onnx"))?,
                tokens: find_named(&files, "tokens.txt")?,
            })
        }
        ModelKind::Qwen3Asr => Some(ModelFiles::Qwen3Asr {
            frontend: find_onnx(&files, "conv_frontend")?,
            encoder: find_onnx(&files, "encoder")?,
            decoder: find_onnx(&files, "decoder")?,
            tokenizer: find_tokenizer(&files)?,
        }),
        ModelKind::FunAsrNano => Some(ModelFiles::FunAsrNano {
            encoder_adaptor: find_onnx(&files, "encoder_adaptor")?,
            llm: find_onnx(&files, "llm")?,
            embedding: find_onnx(&files, "embedding")?,
            tokenizer: find_tokenizer(&files)?,
        }),
    }
}

fn find_punctuation_model(dir: &Path) -> Option<PathBuf> {
    let files = files_in(&dir.join("punct"));
    find_onnx(&files, "model").or_else(|| find_onnx(&files, "ct-transformer"))
}

fn find_named(files: &[PathBuf], target: &str) -> Option<PathBuf> {
    files
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(target))
        })
        .cloned()
}

fn find_onnx(files: &[PathBuf], prefix: &str) -> Option<PathBuf> {
    files
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            (name.starts_with(prefix) && name.ends_with(".onnx"))
                .then_some((!name.contains("int8"), path))
        })
        .min()
        .map(|(_, path)| path.clone())
}

fn find_tokenizer(files: &[PathBuf]) -> Option<PathBuf> {
    let path = find_named(files, "tokenizer.json").or_else(|| find_named(files, "vocab.json"))?;
    path.parent().map(Path::to_path_buf)
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(current).into_iter().flatten().flatten() {
            if let Ok(kind) = entry.file_type() {
                if kind.is_dir() {
                    stack.push(entry.path());
                } else if kind.is_file() {
                    files.push(entry.path());
                }
            }
        }
    }
    files.sort_unstable();
    files
}

fn dir_size(dir: &Path) -> u64 {
    files_in(dir)
        .into_iter()
        .filter_map(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset_file(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name.split('/').collect::<PathBuf>());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "test data").unwrap();
        path
    }

    #[test]
    fn voice_presets_select_and_validate_model_ids() {
        assert_eq!(model_preset(None).unwrap().id, "sense-voice");
        for (id, bytes) in [
            ("sense-voice", 227_720_639),
            ("qwen3-asr", 878_702_423),
            ("funasr-nano", 841_730_611),
        ] {
            let settings = VoiceSettings {
                model_id: Some(id.into()),
                ..Default::default()
            };
            assert!(validate_settings(&settings).is_ok());
            let preset = active_preset(&settings);
            assert_eq!(preset.id, id);
            assert_eq!(preset.download_bytes(), bytes);
            for asset in std::iter::once(&preset.asr).chain(preset.punctuation.as_ref()) {
                assert_eq!(asset.sha256.len(), 64);
                assert!(asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
            }
        }
        let settings = VoiceSettings {
            model_id: Some("../model".into()),
            ..Default::default()
        };
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn discovers_model_assets_and_constructs_native_arguments() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let sense = MODELS[0].dir(root);
        let punctuation = asset_file(&sense, "punct/archive/model.int8.onnx");
        let tokens = asset_file(&sense, "archive/tokens.txt");
        assert!(find_model(&sense, ModelKind::SenseVoice).is_none());
        let model = asset_file(&sense, "archive/model.int8.onnx");
        let settings = VoiceSettings {
            replacements: BTreeMap::from([
                ("first".into(), "Vetro".into()),
                ("second".into(), "Vetro".into()),
                ("third".into(), "invalid,split".into()),
                ("fourth".into(), "  Kiro  ".into()),
            ]),
            ..Default::default()
        };
        let sense_args = transcription_args(
            &find_model(&sense, ModelKind::SenseVoice).unwrap(),
            &settings,
            None,
        );
        assert_eq!(
            sense_args,
            [
                arg_kv("--sense-voice-model=", &model),
                arg_kv("--tokens=", &tokens),
                "--sense-voice-use-itn=true".into(),
                "--sense-voice-language=auto".into(),
            ]
        );
        assert_eq!(find_punctuation_model(&sense), Some(punctuation));

        let qwen = MODELS[1].dir(root);
        let frontend = asset_file(&qwen, "archive/conv_frontend.onnx");
        asset_file(&qwen, "archive/encoder.onnx");
        let encoder = asset_file(&qwen, "archive/encoder.int8.onnx");
        let decoder = asset_file(&qwen, "archive/decoder.q4.onnx");
        assert!(find_model(&qwen, ModelKind::Qwen3Asr).is_none());
        let tokenizer = asset_file(&qwen, "archive/tokenizer/tokenizer.json");
        let qwen_args = transcription_args(
            &find_model(&qwen, ModelKind::Qwen3Asr).unwrap(),
            &settings,
            Some("zh"),
        );
        assert_eq!(
            qwen_args,
            [
                arg_kv("--qwen3-asr-conv-frontend=", &frontend),
                arg_kv("--qwen3-asr-encoder=", &encoder),
                arg_kv("--qwen3-asr-decoder=", &decoder),
                arg_kv("--qwen3-asr-tokenizer=", tokenizer.parent().unwrap()),
                "--qwen3-asr-max-new-tokens=512".into(),
                "--qwen3-asr-max-total-len=1024".into(),
                "--qwen3-asr-hotwords=Kiro,Vetro".into(),
            ]
        );

        let funasr = MODELS[2].dir(root);
        let adaptor = asset_file(&funasr, "archive/encoder_adaptor.int8.onnx");
        let llm = asset_file(&funasr, "archive/llm.int8.onnx");
        let embedding = asset_file(&funasr, "archive/embedding.onnx");
        let vocab = asset_file(&funasr, "archive/Qwen3/vocab.json");
        let files = find_model(&funasr, ModelKind::FunAsrNano).unwrap();
        assert_eq!(
            transcription_args(&files, &settings, Some("en")),
            [
                arg_kv("--funasr-nano-encoder-adaptor=", &adaptor),
                arg_kv("--funasr-nano-llm=", &llm),
                arg_kv("--funasr-nano-embedding=", &embedding),
                arg_kv("--funasr-nano-tokenizer=", vocab.parent().unwrap()),
                "--funasr-nano-itn=true".into(),
                "--funasr-nano-max-new-tokens=512".into(),
                "--funasr-nano-language=en".into(),
                "--funasr-nano-hotwords=Kiro,Vetro".into(),
            ]
        );
        assert_eq!(
            transcription_args(&files, &VoiceSettings::default(), None).len(),
            6
        );
    }

    #[test]
    fn only_reads_the_punctuation_result_line() {
        let entry = |direction, text: &str| RawEntry {
            ts: 0,
            direction,
            text: text.into(),
        };
        let mut log: Vec<_> = [
            "OfflinePunctuationConfig(model=ct-transformer)",
            "Elapsed seconds: 0.567 s",
            "Input text: 你好 Vetro",
            "Output text: ",
        ]
        .into_iter()
        .map(|line| entry(RawDirection::Stderr, line))
        .collect();
        assert_eq!(parse_punctuation_output(&log), None);
        log.push(entry(RawDirection::Received, "你好，Vetro。"));
        assert_eq!(
            parse_punctuation_output(&log).as_deref(),
            Some("你好，Vetro。")
        );
        log.push(entry(RawDirection::Received, "unexpected extra output"));
        assert_eq!(parse_punctuation_output(&log), None);
    }

    #[tokio::test]
    async fn reports_and_removes_models_independently_of_legacy_assets() {
        let root = tempfile::tempdir().unwrap();
        let voice = root.path().join("voice");
        let manager = VoiceManager::default();
        let settings = VoiceSettings::default();
        asset_file(&voice, "model/old/model.int8.onnx");
        asset_file(&voice, "model/old/tokens.txt");
        let binary = asset_file(
            &voice.join("runtime/bin"),
            &executable("sherpa-onnx-offline"),
        );
        let status = manager.status(&voice, &settings);
        assert_eq!(status.active_model_id, "sense-voice");
        assert!(!status.installed);
        assert!(!status.ready);
        assert_eq!(status.download_bytes, MODELS[0].download_bytes());
        assert_eq!(status.models.iter().filter(|model| model.active).count(), 1);

        let sense = MODELS[0].dir(&voice);
        asset_file(&sense, "model.int8.onnx");
        asset_file(&sense, "tokens.txt");
        asset_file(&sense, "punct/model.onnx");
        let qwen = MODELS[1].dir(&voice);
        for name in [
            "conv_frontend.onnx",
            "encoder.onnx",
            "decoder.onnx",
            "tokenizer/tokenizer.json",
        ] {
            asset_file(&qwen, name);
        }
        let status = manager.status(&voice, &settings);
        assert!(status.installed && status.ready);
        assert!(status.models[1].installed && status.models[1].native_punctuation);
        assert!(status.models[1].installed_bytes > 0);
        let status = manager
            .uninstall(&voice, &settings, Some("sense-voice"))
            .await
            .unwrap();
        assert!(!status.installed);
        assert!(status.models[1].installed && binary.exists());
        manager
            .uninstall(&voice, &settings, Some("qwen3-asr"))
            .await
            .unwrap();
        assert!(!binary.exists());
        assert!(voice.join("model").exists());
        manager.uninstall(&voice, &settings, None).await.unwrap();
        assert!(!voice.exists());
    }

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
