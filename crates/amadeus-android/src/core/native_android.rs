use std::{
    ffi::{c_char, c_void, CStr, CString},
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

use android_activity::{AndroidApp, MainEvent, PollEvent};
use anyhow::{Context, Result};
use hound::SampleFormat;
use ndk::asset::AssetManager;

use amadeus_backend::{
    backend::TurnRequest, config::AgentRuntimeConfig, ui::AgentUiService, ConversationBackend,
    ExternalAgentClient, ModelToolCall, TextStreamSink,
};
use amadeus_client::{
    stt::{config::discover_stt_runtime_config, SttService, SttTranscript},
    tts::{discover_tts_runtime_config, filter::filter_for_tts, TtsRequest, TtsService},
};

const NATIVE_SESSION_ID: &str = "amadeus-app";
const LIP_SYNC_MIN_RMS: f32 = 0.012;
const LIP_SYNC_MAX_RMS: f32 = 0.180;

const STT_STATE_IDLE: i32 = 0;
const STT_STATE_PROCESSING: i32 = 2;
const STT_STATE_RESPONDING: i32 = 3;

static ANDROID_RUNTIME: OnceLock<AndroidUiRuntime> = OnceLock::new();
static VOICE_WAS_INTERRUPTED: AtomicBool = AtomicBool::new(false);
static IS_TTS_PLAYING: AtomicBool = AtomicBool::new(false);
static CURRENT_TURN_ID: AtomicU64 = AtomicU64::new(0);
static ANDROID_STT_STATE: AtomicI32 = AtomicI32::new(STT_STATE_IDLE);
static TTS_MUTE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    fn amadeus_cubism_android_init(
        model_json_path: *const c_char,
        native_window: *mut c_void,
    ) -> i32;
    fn amadeus_cubism_android_render_frame() -> i32;
    fn amadeus_cubism_android_destroy();
    fn amadeus_cubism_android_last_error_message() -> *const c_char;
    fn amadeus_cubism_android_set_lip_sync(value: f32);
    fn amadeus_cubism_android_set_expression(name: *const c_char);

    fn amadeus_aaudio_play_pcm_f32(
        samples: *const f32,
        num_frames: i32,
        sample_rate: i32,
        channels: i32,
    ) -> i32;
}

fn last_cubism_error() -> String {
    unsafe {
        let ptr = amadeus_cubism_android_last_error_message();
        if ptr.is_null() {
            "unknown cubism error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

fn tts_echo_suppressed() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    TTS_MUTE_UNTIL_MS.load(Ordering::Relaxed) > now
}

fn set_tts_mute_window(ms: u64) {
    let until = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
        + ms;
    TTS_MUTE_UNTIL_MS.store(until, Ordering::Relaxed);
}

struct AndroidUiRuntime {
    agent_service: Mutex<Option<Arc<dyn ConversationBackend>>>,
    tts_service: Option<Arc<TtsService>>,
    stt_service: Option<Arc<SttService>>,
    voice_enabled: bool,
    stt_enabled: bool,
    internal_dir: PathBuf,
}

impl AndroidUiRuntime {
    fn initialize(internal_dir: &Path, _asset_manager: &AssetManager) -> Self {
        // Route all HuggingFace hub downloads (TTS model weights, tokenizers) into the
        // app-private internal storage directory so they are removed on uninstall.
        std::env::set_var("HF_HOME", internal_dir.join("hf_cache"));

        let mut agent_service: Option<Arc<dyn ConversationBackend>> = None;

        let ext_url = std::env::var("AMADEUS_EXTERNAL_AGENT_URL").ok();
        if let Some(url) = ext_url {
            if let Some(client) = ExternalAgentClient::from_url(url, None) {
                agent_service = Some(Arc::new(client));
            }
        } else if let Ok(mut runtime) =
            AgentRuntimeConfig::load(Some(internal_dir.to_path_buf()), None)
        {
            if runtime.services.local_llm {
                runtime.provider = amadeus_backend::config::LlmProvider::LlamaCpp;
            }
            runtime.normalize_provider_defaults();
            if runtime.model.is_some() || runtime.services.local_llm {
                agent_service = Some(Arc::new(AgentUiService::new(runtime)));
            }
        }

        let tts_config = discover_tts_runtime_config(None);
        let stt_config = discover_stt_runtime_config(internal_dir, None);

        let tts_service = if tts_config.enabled {
            match TtsService::new(tts_config) {
                Ok(svc) => Some(Arc::new(svc)),
                Err(e) => {
                    log::warn!("TTS init failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        let stt_service = if stt_config.enabled {
            match SttService::new(stt_config) {
                Ok(svc) => Some(Arc::new(svc)),
                Err(e) => {
                    log::warn!("STT init failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        let voice_enabled = tts_service.is_some();
        let stt_enabled = stt_service.is_some();

        AndroidUiRuntime {
            agent_service: Mutex::new(agent_service),
            tts_service,
            stt_service,
            voice_enabled,
            stt_enabled,
            internal_dir: internal_dir.to_path_buf(),
        }
    }
}

fn android_audio_play_pcm(samples: &[f32], sample_rate: u32) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let frames = samples.len() as i32;
    let rc =
        unsafe { amadeus_aaudio_play_pcm_f32(samples.as_ptr(), frames, sample_rate as i32, 1) };
    if rc != 0 {
        anyhow::bail!("AAudio playback failed (code {rc})");
    }
    Ok(())
}

fn wav_bytes_to_f32(wav: Vec<u8>) -> Result<(Vec<f32>, u32)> {
    let mut reader =
        hound::WavReader::new(Cursor::new(wav)).context("failed to decode synthesized WAV")?;
    let spec = reader.spec();

    let samples = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read i16 WAV samples")?,
        (SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read f32 WAV samples")?,
        _ => anyhow::bail!(
            "unsupported WAV format: {:?} {}-bit",
            spec.sample_format,
            spec.bits_per_sample
        ),
    };

    Ok((samples, spec.sample_rate))
}

pub struct NativeAndroidRuntime {
    app: AndroidApp,
}

impl NativeAndroidRuntime {
    pub fn new(app: AndroidApp) -> Result<Self> {
        Ok(Self { app })
    }

    pub fn run(self) -> Result<()> {
        let internal_dir = self
            .app
            .internal_data_path()
            .context("no internal data path from Android")?;

        let asset_manager = self.app.asset_manager();

        let model_path = extract_model_to_internal(&asset_manager, &internal_dir)?;

        let runtime = AndroidUiRuntime::initialize(&internal_dir, &asset_manager);
        let _ = ANDROID_RUNTIME.set(runtime);

        if let Some(stt) = ANDROID_RUNTIME.get().and_then(|r| r.stt_service.as_ref()) {
            let stt = Arc::clone(stt);
            thread::spawn(move || {
                stt.run_capture_loop(|transcript| {
                    on_stt_transcript(transcript);
                });
            });
        }

        let model_path_cstr = CString::new(
            model_path
                .to_str()
                .context("model path is not valid UTF-8")?,
        )
        .context("model path contains NUL byte")?;

        let mut cubism_initialized = false;
        let mut native_window_ptr: *mut c_void = std::ptr::null_mut();
        let mut running = true;

        while running {
            self.app
                .poll_events(Some(Duration::from_millis(16)), |event| match event {
                    PollEvent::Main(MainEvent::InitWindow { .. }) => {
                        if let Some(window) = self.app.native_window() {
                            let new_ptr = window.ptr().as_ptr() as *mut c_void;

                            if cubism_initialized && new_ptr != native_window_ptr {
                                unsafe { amadeus_cubism_android_destroy() };
                                cubism_initialized = false;
                            }

                            native_window_ptr = new_ptr;

                            if !cubism_initialized {
                                let rc = unsafe {
                                    amadeus_cubism_android_init(
                                        model_path_cstr.as_ptr(),
                                        native_window_ptr,
                                    )
                                };
                                if rc != 0 {
                                    let msg = last_cubism_error();
                                    log::error!("Cubism init failed: {msg}");
                                    running = false;
                                } else {
                                    cubism_initialized = true;
                                }
                            }
                        }
                    }
                    PollEvent::Main(MainEvent::TerminateWindow { .. }) => {
                        if cubism_initialized {
                            unsafe { amadeus_cubism_android_destroy() };
                            cubism_initialized = false;
                        }
                        native_window_ptr = std::ptr::null_mut();
                    }
                    PollEvent::Main(MainEvent::Destroy) => {
                        running = false;
                    }
                    _ => {}
                });

            if cubism_initialized {
                let rc = unsafe { amadeus_cubism_android_render_frame() };
                if rc != 0 {
                    let msg = last_cubism_error();
                    log::error!("Cubism render error: {msg}");
                    running = false;
                }
            }
        }

        if cubism_initialized {
            unsafe { amadeus_cubism_android_destroy() };
        }

        Ok(())
    }
}

fn extract_model_to_internal(asset_manager: &AssetManager, internal_dir: &Path) -> Result<PathBuf> {
    let model_dest = internal_dir.join("model");
    std::fs::create_dir_all(&model_dest)?;

    let mut asset_dir = asset_manager
        .open_dir(&std::ffi::CString::new("model").unwrap())
        .context("'model/' directory not found in APK assets")?;

    let mut model_json: Option<PathBuf> = None;

    for filename in &mut asset_dir {
        let name_str = filename.to_string_lossy();
        let asset_path = format!("model/{name_str}");
        let dest_path = model_dest.join(name_str.as_ref());

        if !dest_path.exists() {
            let mut asset = asset_manager
                .open(&std::ffi::CString::new(asset_path.as_str()).unwrap())
                .with_context(|| format!("failed to open APK asset: {asset_path}"))?;
            let data = asset.get_buffer()?;
            std::fs::write(&dest_path, data)?;
        }

        if name_str.ends_with(".model3.json") && model_json.is_none() {
            model_json = Some(dest_path);
        }
    }

    model_json.context("no .model3.json file found in APK model/ assets")
}

struct TextAccumulatorSink(mpsc::Sender<Option<String>>);

impl TextStreamSink for TextAccumulatorSink {
    fn push_delta(&mut self, delta: &str) {
        let _ = self.0.send(Some(delta.to_string()));
    }

    fn finish(&mut self) {
        let _ = self.0.send(None);
    }

    fn on_tool_call(&mut self, _call: ModelToolCall) {}
}

fn on_stt_transcript(transcript: SttTranscript) {
    if tts_echo_suppressed() {
        return;
    }

    ANDROID_STT_STATE.store(STT_STATE_PROCESSING, Ordering::Relaxed);

    let runtime = match ANDROID_RUNTIME.get() {
        Some(r) => r,
        None => return,
    };

    let agent = {
        let guard = runtime.agent_service.lock().unwrap();
        guard.as_ref().map(Arc::clone)
    };

    let Some(agent) = agent else {
        ANDROID_STT_STATE.store(STT_STATE_IDLE, Ordering::Relaxed);
        return;
    };

    let tts = match runtime.tts_service.as_ref() {
        Some(svc) => Arc::clone(svc),
        None => {
            ANDROID_STT_STATE.store(STT_STATE_IDLE, Ordering::Relaxed);
            return;
        }
    };

    let turn_id = CURRENT_TURN_ID.fetch_add(1, Ordering::Relaxed) + 1;
    ANDROID_STT_STATE.store(STT_STATE_RESPONDING, Ordering::Relaxed);

    thread::spawn(move || {
        let request = TurnRequest {
            prompt: transcript.text,
            session_id: Some(NATIVE_SESSION_ID.to_string()),
            voice_mode: false,
        };

        let (tx, rx) = mpsc::channel::<Option<String>>();
        let mut sink = TextAccumulatorSink(tx);

        let _ = agent.run_turn_streaming(request, &mut sink);
        drop(sink);

        let mut accumulated = String::new();
        while let Ok(Some(delta)) = rx.recv() {
            accumulated.push_str(&delta);
        }

        if accumulated.is_empty() || CURRENT_TURN_ID.load(Ordering::Relaxed) != turn_id {
            ANDROID_STT_STATE.store(STT_STATE_IDLE, Ordering::Relaxed);
            return;
        }

        let filtered = filter_for_tts(&accumulated);
        if !filtered.is_empty() {
            IS_TTS_PLAYING.store(true, Ordering::Relaxed);

            // AAudio playback below is blocking; drive lip sync on a side thread
            // stopped via IS_TTS_PLAYING once playback returns.
            let lip_sync_handle = thread::spawn(|| {
                let frame_dur = Duration::from_millis(16);
                let mut t: f32 = 0.0;
                while IS_TTS_PLAYING.load(Ordering::Relaxed) {
                    let value = 0.3 + 0.35 * (1.0 + (t * std::f32::consts::TAU * 3.0).sin());
                    unsafe { amadeus_cubism_android_set_lip_sync(value) };
                    t += frame_dur.as_secs_f32();
                    thread::sleep(frame_dur);
                }
                unsafe { amadeus_cubism_android_set_lip_sync(0.0) };
            });

            let req = TtsRequest {
                text: filtered,
                speaker: None,
                language: None,
            };

            match tts.synthesize(req) {
                Ok(wav_bytes) => match wav_bytes_to_f32(wav_bytes) {
                    Ok((samples, sample_rate)) => {
                        set_tts_mute_window(800);
                        if let Err(e) = android_audio_play_pcm(&samples, sample_rate) {
                            log::warn!("TTS playback error: {e}");
                        }
                    }
                    Err(e) => log::warn!("TTS WAV decode error: {e}"),
                },
                Err(e) => log::warn!("TTS synthesis error: {e}"),
            }

            IS_TTS_PLAYING.store(false, Ordering::Relaxed);
            let _ = lip_sync_handle.join();
        }

        if CURRENT_TURN_ID.load(Ordering::Relaxed) == turn_id {
            ANDROID_STT_STATE.store(STT_STATE_IDLE, Ordering::Relaxed);
        }
    });
}
