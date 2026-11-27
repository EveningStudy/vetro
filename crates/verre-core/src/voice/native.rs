use std::{
    ffi::{c_char, CStr, CString},
    path::{Path, PathBuf},
    ptr::null,
};

use libloading::Library;
use verre_agent_api::{AgentError, Result};
use verre_proto::rpc::ErrorCode;

use super::ModelFiles;

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct FeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineTransducerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineParaformerModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineNemoEncDecCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineWhisperModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    language: *const c_char,
    task: *const c_char,
    tail_paddings: i32,
    enable_token_timestamps: i32,
    enable_segment_timestamps: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineTdnnModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineLMConfig {
    model: *const c_char,
    scale: f32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineSenseVoiceModelConfig {
    model: *const c_char,
    language: *const c_char,
    use_itn: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineMoonshineModelConfig {
    preprocessor: *const c_char,
    encoder: *const c_char,
    uncached_decoder: *const c_char,
    cached_decoder: *const c_char,
    merged_decoder: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineFireRedAsrModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineDolphinModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineZipformerCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineCanaryModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    src_lang: *const c_char,
    tgt_lang: *const c_char,
    use_pnc: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineWenetCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineOmnilingualAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineMedAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineFunAsrNanoModelConfig {
    encoder_adaptor: *const c_char,
    llm: *const c_char,
    embedding: *const c_char,
    tokenizer: *const c_char,
    system_prompt: *const c_char,
    user_prompt: *const c_char,
    max_new_tokens: i32,
    temperature: f32,
    top_p: f32,
    seed: i32,
    language: *const c_char,
    itn: i32,
    hotwords: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineFireRedAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineQwen3AsrModelConfig {
    conv_frontend: *const c_char,
    encoder: *const c_char,
    decoder: *const c_char,
    tokenizer: *const c_char,
    max_total_len: i32,
    max_new_tokens: i32,
    temperature: f32,
    top_p: f32,
    seed: i32,
    hotwords: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineCohereTranscribeModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    language: *const c_char,
    use_punct: i32,
    use_itn: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineModelConfig {
    transducer: OfflineTransducerModelConfig,
    paraformer: OfflineParaformerModelConfig,
    nemo_ctc: OfflineNemoEncDecCtcModelConfig,
    whisper: OfflineWhisperModelConfig,
    tdnn: OfflineTdnnModelConfig,
    tokens: *const c_char,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    telespeech_ctc: *const c_char,
    sense_voice: OfflineSenseVoiceModelConfig,
    moonshine: OfflineMoonshineModelConfig,
    fire_red_asr: OfflineFireRedAsrModelConfig,
    dolphin: OfflineDolphinModelConfig,
    zipformer_ctc: OfflineZipformerCtcModelConfig,
    canary: OfflineCanaryModelConfig,
    wenet_ctc: OfflineWenetCtcModelConfig,
    omnilingual: OfflineOmnilingualAsrCtcModelConfig,
    medasr: OfflineMedAsrCtcModelConfig,
    funasr_nano: OfflineFunAsrNanoModelConfig,
    fire_red_asr_ctc: OfflineFireRedAsrCtcModelConfig,
    qwen3_asr: OfflineQwen3AsrModelConfig,
    cohere_transcribe: OfflineCohereTranscribeModelConfig,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct HomophoneReplacerConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineCtcFstDecoderConfig {
    graph: *const c_char,
    max_active: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflineRecognizerConfig {
    feat_config: FeatureConfig,
    model_config: OfflineModelConfig,
    lm_config: OfflineLMConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    hotwords_file: *const c_char,
    hotwords_score: f32,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: f32,
    hr: HomophoneReplacerConfig,
    ctc_fst_decoder_config: OfflineCtcFstDecoderConfig,
}

#[repr(C)]
struct OfflineRecognizerResult {
    text: *const c_char,
    timestamps: *mut f32,
    count: i32,
    tokens: *const c_char,
    tokens_arr: *const *const c_char,
    json: *const c_char,
    lang: *const c_char,
    emotion: *const c_char,
    event: *const c_char,
    durations: *mut f32,
    ys_log_probs: *mut f32,
    segment_timestamps: *const f32,
    segment_durations: *const f32,
    segment_texts: *const c_char,
    segment_texts_arr: *const *const c_char,
    segment_count: i32,
}

#[repr(C)]
struct Wave {
    samples: *const f32,
    sample_rate: i32,
    num_samples: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflinePunctuationModelConfig {
    ct_transformer: *const c_char,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct OfflinePunctuationConfig {
    model: OfflinePunctuationModelConfig,
}

enum OpaqueRecognizer {}
enum OpaqueStream {}
enum OpaquePunctuation {}

type CreateRecognizerFn =
    unsafe extern "C" fn(*const OfflineRecognizerConfig) -> *const OpaqueRecognizer;
type DestroyRecognizerFn = unsafe extern "C" fn(*const OpaqueRecognizer);
type CreateStreamFn = unsafe extern "C" fn(*const OpaqueRecognizer) -> *const OpaqueStream;
type DestroyStreamFn = unsafe extern "C" fn(*const OpaqueStream);
type AcceptWaveformFn = unsafe extern "C" fn(*const OpaqueStream, i32, *const f32, i32);
type DecodeStreamFn = unsafe extern "C" fn(*const OpaqueRecognizer, *const OpaqueStream);
type GetResultFn = unsafe extern "C" fn(*const OpaqueStream) -> *const OfflineRecognizerResult;
type DestroyResultFn = unsafe extern "C" fn(*const OfflineRecognizerResult);
type ReadWaveFn = unsafe extern "C" fn(*const c_char, i32) -> *const Wave;
type FreeWaveFn = unsafe extern "C" fn(*const Wave);
type CreatePunctFn =
    unsafe extern "C" fn(*const OfflinePunctuationConfig) -> *const OpaquePunctuation;
type DestroyPunctFn = unsafe extern "C" fn(*const OpaquePunctuation);
type AddPunctFn = unsafe extern "C" fn(*const OpaquePunctuation, *const c_char) -> *const c_char;
type FreeTextFn = unsafe extern "C" fn(*const c_char);
type GetVersionStrFn = unsafe extern "C" fn() -> *const c_char;

pub(super) struct NativeEngine {
    recognizer: *const OpaqueRecognizer,
    punct: *const OpaquePunctuation,
    create_stream: CreateStreamFn,
    destroy_stream: DestroyStreamFn,
    accept_waveform: AcceptWaveformFn,
    decode_stream: DecodeStreamFn,
    get_result: GetResultFn,
    destroy_result: DestroyResultFn,
    read_wave: ReadWaveFn,
    free_wave: FreeWaveFn,
    destroy_recognizer: DestroyRecognizerFn,
    destroy_punct: Option<DestroyPunctFn>,
    add_punct: Option<AddPunctFn>,
    free_text: Option<FreeTextFn>,
    is_sensevoice: bool,
    // Keep Library last so C handles are destroyed before unloading the DLL/so/dylib.
    _lib: Library,
}

// All pointers inside NativeEngine are guarded by an exclusive mutex and never shared across threads concurrently.
unsafe impl Send for NativeEngine {}

impl Drop for NativeEngine {
    fn drop(&mut self) {
        unsafe {
            if !self.punct.is_null() {
                if let Some(destroy) = self.destroy_punct {
                    destroy(self.punct);
                }
                self.punct = null();
            }
            if !self.recognizer.is_null() {
                (self.destroy_recognizer)(self.recognizer);
                self.recognizer = null();
            }
        }
    }
}

pub(super) fn find_c_api_lib(runtime: &super::Runtime) -> Option<PathBuf> {
    let target = format!(
        "{}sherpa-onnx-c-api{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    for dir in &runtime.lib_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&target))
            {
                return Some(path);
            }
        }
    }
    None
}

impl NativeEngine {
    pub(super) fn load(
        lib_path: &Path,
        files: &ModelFiles,
        punct_model: Option<&Path>,
        language: Option<&str>,
        hotwords: &str,
        threads: i32,
    ) -> Result<Self> {
        let lib = load_library(lib_path)?;
        if let Ok(get_version) = symbol::<GetVersionStrFn>(&lib, b"SherpaOnnxGetVersionStr\0") {
            let ptr = unsafe { get_version() };
            if !ptr.is_null() {
                let version = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
                if !version.starts_with("2.0.") {
                    return Err(AgentError::new(
                        ErrorCode::AgentUnavailable,
                        format!("Unsupported sherpa-onnx C API version {version}, expected 2.0.x"),
                    ));
                }
            }
        }
        let c_strs = CStringArena::default();

        let create_recognizer =
            symbol::<CreateRecognizerFn>(&lib, b"SherpaOnnxCreateOfflineRecognizer\0")?;
        let destroy_recognizer =
            symbol::<DestroyRecognizerFn>(&lib, b"SherpaOnnxDestroyOfflineRecognizer\0")?;
        let create_stream = symbol::<CreateStreamFn>(&lib, b"SherpaOnnxCreateOfflineStream\0")?;
        let destroy_stream = symbol::<DestroyStreamFn>(&lib, b"SherpaOnnxDestroyOfflineStream\0")?;
        let accept_waveform =
            symbol::<AcceptWaveformFn>(&lib, b"SherpaOnnxAcceptWaveformOffline\0")?;
        let decode_stream = symbol::<DecodeStreamFn>(&lib, b"SherpaOnnxDecodeOfflineStream\0")?;
        let get_result = symbol::<GetResultFn>(&lib, b"SherpaOnnxGetOfflineStreamResult\0")?;
        let destroy_result =
            symbol::<DestroyResultFn>(&lib, b"SherpaOnnxDestroyOfflineRecognizerResult\0")?;
        let read_wave = symbol::<ReadWaveFn>(&lib, b"SherpaOnnxReadWaveFromBinaryData\0")?;
        let free_wave = symbol::<FreeWaveFn>(&lib, b"SherpaOnnxFreeWave\0")?;

        let mut config: OfflineRecognizerConfig = unsafe { std::mem::zeroed() };
        config.feat_config.sample_rate = 16_000;
        config.feat_config.feature_dim = 80;
        config.model_config.num_threads = threads;
        config.model_config.provider = c_strs.str("cpu")?;
        config.decoding_method = c_strs.str("greedy_search")?;
        config.hotwords_score = 1.5;
        config.max_active_paths = 4;

        let is_sensevoice = match files {
            ModelFiles::SenseVoice { model, tokens } => {
                config.model_config.tokens = c_strs.path(tokens)?;
                config.model_config.sense_voice.model = c_strs.path(model)?;
                config.model_config.sense_voice.language =
                    c_strs.str(language.unwrap_or("auto"))?;
                config.model_config.sense_voice.use_itn = 1;
                true
            }
            ModelFiles::Qwen3Asr {
                frontend,
                encoder,
                decoder,
                tokenizer,
            } => {
                config.model_config.qwen3_asr.conv_frontend = c_strs.path(frontend)?;
                config.model_config.qwen3_asr.encoder = c_strs.path(encoder)?;
                config.model_config.qwen3_asr.decoder = c_strs.path(decoder)?;
                config.model_config.qwen3_asr.tokenizer = c_strs.path(tokenizer)?;
                config.model_config.qwen3_asr.max_total_len = 1024;
                config.model_config.qwen3_asr.max_new_tokens = 512;
                config.model_config.qwen3_asr.temperature = 1e-6;
                config.model_config.qwen3_asr.top_p = 0.8;
                config.model_config.qwen3_asr.seed = 42;
                if !hotwords.is_empty() {
                    config.model_config.qwen3_asr.hotwords = c_strs.str(hotwords)?;
                }
                false
            }
            ModelFiles::FunAsrNano {
                encoder_adaptor,
                llm,
                embedding,
                tokenizer,
            } => {
                config.model_config.funasr_nano.encoder_adaptor = c_strs.path(encoder_adaptor)?;
                config.model_config.funasr_nano.llm = c_strs.path(llm)?;
                config.model_config.funasr_nano.embedding = c_strs.path(embedding)?;
                config.model_config.funasr_nano.tokenizer = c_strs.path(tokenizer)?;
                config.model_config.funasr_nano.max_new_tokens = 512;
                config.model_config.funasr_nano.temperature = 1e-6;
                config.model_config.funasr_nano.top_p = 0.8;
                config.model_config.funasr_nano.seed = 42;
                config.model_config.funasr_nano.itn = 1;
                if let Some(language) = language {
                    config.model_config.funasr_nano.language = c_strs.str(language)?;
                }
                if !hotwords.is_empty() {
                    config.model_config.funasr_nano.hotwords = c_strs.str(hotwords)?;
                }
                false
            }
        };

        let recognizer = unsafe { create_recognizer(&config) };
        if recognizer.is_null() {
            return Err(AgentError::new(
                ErrorCode::AgentUnavailable,
                "Failed to initialize sherpa-onnx offline recognizer",
            ));
        }

        let (punct, destroy_punct, add_punct, free_text) = if is_sensevoice {
            if let Some(punct_path) = punct_model {
                let create_punct =
                    symbol::<CreatePunctFn>(&lib, b"SherpaOnnxCreateOfflinePunctuation\0").ok();
                let destroy_punct =
                    symbol::<DestroyPunctFn>(&lib, b"SherpaOnnxDestroyOfflinePunctuation\0").ok();
                let add_punct =
                    symbol::<AddPunctFn>(&lib, b"SherpaOfflinePunctuationAddPunct\0").ok();
                let free_text =
                    symbol::<FreeTextFn>(&lib, b"SherpaOfflinePunctuationFreeText\0").ok();
                if let (Some(create), Some(destroy), Some(add), Some(free)) =
                    (create_punct, destroy_punct, add_punct, free_text)
                {
                    let mut pcfg: OfflinePunctuationConfig = unsafe { std::mem::zeroed() };
                    pcfg.model.ct_transformer = c_strs.path(punct_path)?;
                    pcfg.model.num_threads = 1;
                    pcfg.model.provider = c_strs.str("cpu")?;
                    let handle = unsafe { create(&pcfg) };
                    if handle.is_null() {
                        (null(), None, None, None)
                    } else {
                        (handle, Some(destroy), Some(add), Some(free))
                    }
                } else {
                    (null(), None, None, None)
                }
            } else {
                (null(), None, None, None)
            }
        } else {
            (null(), None, None, None)
        };

        Ok(Self {
            recognizer,
            punct,
            create_stream,
            destroy_stream,
            accept_waveform,
            decode_stream,
            get_result,
            destroy_result,
            read_wave,
            free_wave,
            destroy_recognizer,
            destroy_punct,
            add_punct,
            free_text,
            is_sensevoice,
            _lib: lib,
        })
    }

    pub(super) fn transcribe(&self, wav_bytes: &[u8]) -> Result<String> {
        let len = i32::try_from(wav_bytes.len())
            .map_err(|_| AgentError::new(ErrorCode::BadRequest, "Audio clip is too large"))?;
        let wave = unsafe { (self.read_wave)(wav_bytes.as_ptr().cast(), len) };
        if wave.is_null() {
            return Err(AgentError::new(
                ErrorCode::BadRequest,
                "Audio must be a valid 16-bit PCM WAV recording",
            ));
        }

        let stream = unsafe { (self.create_stream)(self.recognizer) };
        if stream.is_null() {
            unsafe { (self.free_wave)(wave) };
            return Err(AgentError::new(
                ErrorCode::Internal,
                "Could not allocate offline ASR stream",
            ));
        }

        let text = unsafe {
            let w = &*wave;
            (self.accept_waveform)(stream, w.sample_rate, w.samples, w.num_samples);
            (self.free_wave)(wave);
            (self.decode_stream)(self.recognizer, stream);
            let result = (self.get_result)(stream);
            let out = if result.is_null() {
                String::new()
            } else {
                let s = if (*result).text.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr((*result).text)
                        .to_string_lossy()
                        .into_owned()
                };
                (self.destroy_result)(result);
                s
            };
            (self.destroy_stream)(stream);
            out
        };

        if !self.is_sensevoice {
            return Ok(text);
        }

        let cleaned = super::strip_sensevoice_tags(&text);
        if cleaned.is_empty()
            || cleaned.contains(['。', '！', '？', '.', '!', '?', ',', '，'])
            || self.punct.is_null()
        {
            return Ok(cleaned);
        }

        if let (Some(add_punct), Some(free_text)) = (self.add_punct, self.free_text) {
            if let Ok(input) = CString::new(cleaned.as_str()) {
                let ptr = unsafe { add_punct(self.punct, input.as_ptr()) };
                if !ptr.is_null() {
                    let punctuated = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
                    unsafe { free_text(ptr) };
                    if !punctuated.trim().is_empty() && !punctuated.contains('\u{fffd}') {
                        return Ok(punctuated);
                    }
                }
            }
        }
        Ok(cleaned)
    }
}

#[derive(Default)]
struct CStringArena(std::cell::RefCell<Vec<CString>>);

impl CStringArena {
    fn str(&self, value: &str) -> Result<*const c_char> {
        let cstr = CString::new(value).map_err(|error| {
            AgentError::new(
                ErrorCode::BadRequest,
                format!("Voice parameter contains invalid NUL byte: {error}"),
            )
        })?;
        let ptr = cstr.as_ptr();
        self.0.borrow_mut().push(cstr);
        Ok(ptr)
    }

    fn path(&self, path: &Path) -> Result<*const c_char> {
        let value = path.to_str().ok_or_else(|| {
            AgentError::new(
                ErrorCode::Internal,
                format!("Non-UTF-8 path in voice model: {}", path.display()),
            )
        })?;
        self.str(value)
    }
}

fn load_library(path: &Path) -> Result<Library> {
    #[cfg(windows)]
    let loaded = unsafe {
        const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
        const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
        libloading::os::windows::Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
        .map(Library::from)
    };
    #[cfg(not(windows))]
    let loaded = unsafe { Library::new(path) };
    loaded.map_err(|error| {
        AgentError::new(
            ErrorCode::AgentUnavailable,
            format!(
                "Could not load voice shared library {}: {error}",
                path.display()
            ),
        )
    })
}

fn symbol<T: Copy>(lib: &Library, name: &[u8]) -> Result<T> {
    let resolved = unsafe { lib.get::<T>(name) }.map_err(|error| {
        let label = String::from_utf8_lossy(name);
        AgentError::new(
            ErrorCode::AgentUnavailable,
            format!("Missing symbol {label} in voice library: {error}"),
        )
    })?;
    Ok(*resolved)
}
