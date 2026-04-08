#[cfg(target_os = "linux")]
mod imp {
    use std::{
        collections::HashSet,
        env,
        ffi::{c_char, c_void, CStr, CString},
        fs,
        io::Cursor,
        path::{Path, PathBuf},
        process::Command,
        ptr,
        sync::{
            atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering},
            mpsc::{self, TryRecvError},
            Arc, Mutex, OnceLock,
        },
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{anyhow, bail, Context, Result};
    use hound::SampleFormat;
    use reqwest;
    use rodio::{buffer::SamplesBuffer, OutputStream, Sink};
    use serde_json::Value;

    use crate::{
        agent::{
            backend::TurnRequest,
            config::AgentRuntimeConfig,
            ConversationBackend, ExternalAgentClient,
            providers::ProvidersStore,
            settings_command::{settings_help, SettingsCommand},
            ui::AgentUiService,
            ModelToolCall, TextStreamSink,
        },
        live2d::config::Live2dPaths,
        stt::{config::discover_stt_runtime_config, SttService, SttTranscript},
        tts::{
            config::TtsRuntimeConfig, detection::is_japanese, discover_tts_runtime_config,
            filter::filter_for_tts, japanese::should_prebuffer_mixed_japanese_stream, TtsRequest,
            TtsService, TtsStreamEvent,
        },
    };

    const NATIVE_WINDOW_TITLE: &str = "Amadeus";
    const NATIVE_SESSION_ID: &str = "amadeus-app";
    const THIRD_PARTY_DIR_NAME: &str = "third_party";
    const LOCAL_RESOURCE_DIR_NAME: &str = "ressource";
    const CUBISM_FRAMEWORK_DIR_NAME: &str = "CubismNativeFramework";
    const CUBISM_SDK_DIR_NAME: &str = "CubismSdkForNative-5-r.4.1";
    const CUBISM_FRAMEWORK_DIR_ENV: &str = "AMADEUS_CUBISM_FRAMEWORK_DIR";
    const CUBISM_SDK_DIR_ENV: &str = "AMADEUS_CUBISM_SDK_DIR";
    const NATIVE_LOG_FILE_ENV: &str = "AMADEUS_NATIVE_LOG_FILE";
    const NATIVE_LOG_STDOUT_ENV: &str = "AMADEUS_NATIVE_LOG_STDOUT";
    const NATIVE_LOG_FILE_NAME: &str = "amadeus-native.log";
    const NATIVE_STREAM_EVENT_TOOL_ROUND: i32 = 1;
    const NATIVE_STREAM_EVENT_COMPLETED: i32 = 2;
    const NATIVE_STREAM_EVENT_ERROR: i32 = 3;
    const VOICE_SOFT_GAP_MS: u64 = 180;
    const VOICE_HARD_GAP_MS: u64 = 320;
    const VOICE_LINE_GAP_MS: u64 = 420;
    const NATIVE_FONT_DIR_NAME: &str = "fonts";
    const MIXED_LANGUAGE_STREAM_PREBUFFER_MS: usize = 180;
    const VOICE_NON_STREAMING_CHAR_THRESHOLD: usize = 20;
    const VOICE_NON_STREAMING_JAPANESE_CHAR_THRESHOLD: usize = 8;
    const VOICE_PRIME_MIN_ADVANCE_CHARS: usize = 12;
    const LIP_SYNC_WINDOW_MS: usize = 42;
    const LIP_SYNC_MIN_RMS: f32 = 0.012;
    const LIP_SYNC_MAX_RMS: f32 = 0.180;

    const STT_STATE_IDLE: i32 = 0;
    const STT_STATE_LISTENING: i32 = 1;
    const STT_STATE_PROCESSING: i32 = 2;
    const STT_STATE_RESPONDING: i32 = 3;

    type NativeTextDeltaCallback = unsafe extern "C" fn(*mut c_void, *const c_char);
    type NativeStreamEventCallback = unsafe extern "C" fn(*mut c_void, i32, *const c_char);

    const VOICE_LANG_AUTO: u8 = 0;
    const VOICE_LANG_ENGLISH: u8 = 1;
    const VOICE_LANG_JAPANESE: u8 = 2;

    static NATIVE_UI_RUNTIME: OnceLock<NativeUiRuntime> = OnceLock::new();
    /// Workspace root stashed before the Cubism viewer runs so amadeus_native_init_services()
    /// can call initialize_native_ui_runtime without needing it passed as a parameter.
    static NATIVE_WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();
    static NATIVE_PROVIDERS_STORE: OnceLock<Mutex<ProvidersStore>> = OnceLock::new();
    static NATIVE_PROVIDERS_NAMES: OnceLock<Vec<CString>> = OnceLock::new();
    static NATIVE_ACTIVE_PROVIDER_INDEX: AtomicI32 = AtomicI32::new(-1);
    static NATIVE_PROVIDER_CATALOG: OnceLock<ProviderCatalog> = OnceLock::new();
    /// Set when the user interrupts Kurisu mid-response so the next turn can acknowledge it.
    static VOICE_WAS_INTERRUPTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    /// True while a TTS segment is actively synthesising or playing.
    static IS_TTS_PLAYING: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    /// Monotonically-increasing counter; each new voice-turn gets the next value.
    /// Threads that don't hold the current ID skip the final state update.
    static CURRENT_TURN_ID: AtomicU64 = AtomicU64::new(0);
    static NATIVE_RUNTIME_INFO: OnceLock<CString> = OnceLock::new();
    // Device name list is owned by SttService (STT_DEVICE_NAMES in service.rs) and read
    // via SttService::device_count() / device_name_at(). No separate static needed here.
    static NATIVE_LIP_SYNC_VALUE_BITS: AtomicU32 = AtomicU32::new(0);
    static NATIVE_STT_STATE: AtomicI32 = AtomicI32::new(STT_STATE_IDLE);
    static NATIVE_VOICE_LANG_PREF: AtomicU8 = AtomicU8::new(VOICE_LANG_AUTO);
    /// Millisecond timestamp until which STT finals should be suppressed after TTS ends (echo window).
    static TTS_MUTE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

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

    unsafe extern "C" {
        fn amadeus_cubism_viewer_last_error_message() -> *const c_char;
        fn amadeus_cubism_viewer_run(
            model_json_path: *const c_char,
            window_title: *const c_char,
        ) -> i32;
    }

    pub fn run_native_viewer() -> Result<()> {
        run_native_viewer_with_logs_terminal(false)
    }

    pub fn run_native_viewer_with_logs_terminal(show_logs_terminal: bool) -> Result<()> {
        let workspace_root =
            env::current_dir().context("failed to determine the workspace root")?;
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let assets_root = manifest_dir.join("assets");
        let live2d = Live2dPaths::discover(&assets_root)
            .context("failed to discover Live2D model assets")?;
        let runtime_dir = prepare_shader_runtime(&manifest_dir)?;
        configure_native_log_output(&runtime_dir, show_logs_terminal)?;

        // Stash workspace root so amadeus_native_init_services() (called by C++ after the
        // boot loading phase) can complete the service initialization.
        let _ = NATIVE_WORKSPACE_ROOT.set(workspace_root.clone());

        // Initialize providers (lightweight JSON load) so the preflight can check
        // whether the Amadeus built-in LLM is the configured provider.
        initialize_providers(&workspace_root);

        // Kick off model pre-flight downloads now, before the boot screen renders,
        // so the loading bars in RunModelLoadingPhase() show real progress.
        preflight_model_downloads(&assets_root);

        let model_path = live2d
            .model_path
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", live2d.model_path.display()))?;

        // Expose assets directory so the C++ boot sequence can find frame images
        unsafe {
            env::set_var("AMADEUS_ASSETS_DIR", &assets_root);
        }

        let _cwd_guard = CurrentDirGuard::change_to(&runtime_dir)?;
        let model_path = path_to_cstring(&model_path)?;
        let window_title =
            CString::new(NATIVE_WINDOW_TITLE).context("native window title contains a NUL byte")?;

        let exit_code =
            unsafe { amadeus_cubism_viewer_run(model_path.as_ptr(), window_title.as_ptr()) };

        if exit_code == 0 {
            return Ok(());
        }

        let detail = read_last_error_message()
            .unwrap_or_else(|| format!("native Cubism viewer exited with code {exit_code}"));
        bail!(detail)
    }

    fn configure_native_log_output(runtime_dir: &Path, show_logs_terminal: bool) -> Result<()> {
        unsafe {
            env::remove_var(NATIVE_LOG_FILE_ENV);
            env::remove_var(NATIVE_LOG_STDOUT_ENV);
        }

        if !show_logs_terminal {
            return Ok(());
        }

        let log_path = runtime_dir.join(NATIVE_LOG_FILE_NAME);
        fs::write(&log_path, "")
            .with_context(|| format!("failed to initialize {}", log_path.display()))?;
        launch_native_logs_window(&log_path)?;

        unsafe {
            env::set_var(NATIVE_LOG_FILE_ENV, &log_path);
            env::set_var(NATIVE_LOG_STDOUT_ENV, "0");
        }

        Ok(())
    }

    fn launch_native_logs_window(log_path: &Path) -> Result<()> {
        let executable = env::current_exe().context("failed to resolve the current binary")?;
        Command::new(executable)
            .arg("logs-window")
            .arg("--log-file")
            .arg(log_path)
            .spawn()
            .context("failed to launch the Amadeus-logs viewer")?;
        Ok(())
    }

    struct NativeUiRuntime {
        /// Wrapped in `Mutex<Option<Arc<…>>>` so:
        ///   1. The service can be (re)initialized after a config save.
        ///   2. Callers can clone the Arc and **release the lock before running inference**,
        ///      preventing long LLM turns from blocking the render thread.
        agent_service: Mutex<Option<Arc<dyn ConversationBackend>>>,
        /// True when the app was started with `AMADEUS_EXTERNAL_AGENT_URL` set; in that case
        /// provider-selection and config-save flows skip rebuilding a local AgentUiService.
        use_external_agent: bool,
        voice_player: Option<NativeVoicePlayer>,
        stt_service: Option<Arc<SttService>>,
        voice_enabled: bool,
        stt_enabled: bool,
        status_message: CString,
        workspace_root: PathBuf,
    }

    impl NativeUiRuntime {
        fn initialize(workspace_root: &Path) -> Self {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let assets_root = manifest_dir.join("assets");

            let mut provider = "unconfigured".to_string();
            let mut model = "(unset)".to_string();
            let mut agent_error = None;

            // Track services config for conditional loading; default to all-enabled if config
            // fails to load so the app still starts.
            let mut services_tts: Option<bool> = None;
            let mut services_stt: Option<bool> = None;

            // Prefer an external agent when AMADEUS_EXTERNAL_AGENT_URL is set; otherwise
            // fall back to the built-in in-process AgentUiService.
            let use_external_agent = std::env::var("AMADEUS_EXTERNAL_AGENT_URL").is_ok();

            let agent_service: Option<Arc<dyn ConversationBackend>> = if use_external_agent {
                match ExternalAgentClient::from_env() {
                    Some(client) => {
                        let url = std::env::var("AMADEUS_EXTERNAL_AGENT_URL").unwrap_or_default();
                        provider = format!("external @ {url}");
                        model = "remote".to_string();
                        Some(Arc::new(client))
                    }
                    None => {
                        agent_error = Some("AMADEUS_EXTERNAL_AGENT_URL is set but the client could not be constructed".to_string());
                        None
                    }
                }
            } else {
                match AgentRuntimeConfig::load(Some(workspace_root.to_path_buf()), None) {
                    Ok(mut runtime) => {
                        // When local LLM is requested override the provider so
                        // build_model_client() picks up LlamaCppClient.
                        if runtime.services.local_llm {
                            runtime.provider = crate::agent::config::LlmProvider::LlamaCpp;
                        }
                        services_tts = Some(runtime.services.tts);
                        services_stt = Some(runtime.services.stt);
                        runtime.normalize_provider_defaults();
                        provider = runtime.provider.to_string();
                        model = runtime.model.clone().unwrap_or_else(|| {
                            if runtime.services.local_llm {
                                "local/qwen3-4b-q8".to_string()
                            } else {
                                "(unset)".to_string()
                            }
                        });
                        if runtime.model.is_some() || runtime.services.local_llm {
                            Some(Arc::new(AgentUiService::new(runtime)))
                        } else {
                            None
                        }
                    }
                    Err(error) => {
                        agent_error = Some(error.to_string());
                        None
                    }
                }
            };

            let agent_enabled = agent_service.is_some();

            // Run TTS and STT initialization in parallel — the LLM pre-load thread
            // was already kicked off inside AgentUiService::new() above.
            let tts_config = discover_tts_runtime_config(services_tts);
            let stt_config = discover_stt_runtime_config(&assets_root, services_stt);

            let tts_result: std::sync::Mutex<Option<(Option<NativeVoicePlayer>, Option<String>)>> =
                std::sync::Mutex::new(None);
            let stt_result: std::sync::Mutex<Option<(Option<Arc<SttService>>, Option<String>)>> =
                std::sync::Mutex::new(None);

            thread::scope(|s| {
                s.spawn(|| {
                    *tts_result.lock().unwrap() = Some(initialize_native_voice_player(tts_config));
                });
                s.spawn(|| {
                    *stt_result.lock().unwrap() = Some(initialize_native_stt(stt_config));
                });
            });

            let (voice_player, voice_error) = tts_result.into_inner().unwrap().unwrap();
            let (stt_service, stt_error) = stt_result.into_inner().unwrap().unwrap();
            let voice_enabled = voice_player.is_some();
            let stt_enabled = stt_service.is_some();

            let status = if let Some(error) = agent_error {
                format!("Native renderer is live, but the agent is unavailable: {error}")
            } else if !agent_enabled {
                "Native renderer is live, but no agent model is configured in .amadeus/config.json."
                    .to_string()
            } else if voice_enabled && stt_enabled {
                format!(
                    "Connected to {provider} / {model}. Voice input ready. Press Esc to stop the current reply."
                )
            } else if voice_enabled {
                format!(
                    "Connected to {provider} / {model}. Type below and press Enter. Captions and voice begin as the reply streams in. Press Esc to stop the current reply."
                )
            } else if let Some(error) = voice_error {
                format!(
                    "Connected to {provider} / {model}. Voice is unavailable: {error}. Captions still update in-window."
                )
            } else {
                format!(
                    "Connected to {provider} / {model}. Type below and press Enter. Voice is disabled, but live captions still update in-window."
                )
            };

            if let Some(error) = stt_error {
                eprintln!("STT unavailable: {error}");
            }

            let runtime_info = if agent_enabled {
                format!("{provider} / {model}")
            } else {
                "agent not configured".to_string()
            };
            let _ = NATIVE_RUNTIME_INFO.set(sanitize_c_string(&runtime_info));

            Self {
                agent_service: Mutex::new(agent_service),
                use_external_agent,
                voice_player,
                stt_service,
                voice_enabled,
                stt_enabled,
                status_message: sanitize_c_string(&status),
                workspace_root: workspace_root.to_path_buf(),
            }
        }

        fn run_turn(&self, prompt: &str) -> Result<String> {
            if let Some(reply) = self.handle_settings_command(prompt)? {
                return Ok(reply);
            }
            // Clone the Arc and release the mutex immediately so the render thread is never
            // blocked for the duration of inference (which can be many seconds for local LLMs).
            let service = {
                let guard = self
                    .agent_service
                    .lock()
                    .map_err(|_| anyhow!("agent service mutex poisoned"))?;
                guard
                    .as_ref()
                    .context("the native agent runtime is not configured")?
                    .clone()
            };
            let response = service.run_turn(TurnRequest {
                prompt: prompt.to_string(),
                session_id: Some(NATIVE_SESSION_ID.to_string()),
                voice_mode: false,
            })?;
            Ok(response.reply)
        }

        fn run_turn_streaming(
            &self,
            prompt: &str,
            stream: &mut dyn TextStreamSink,
        ) -> Result<String> {
            if let Some(reply) = self.handle_settings_command(prompt)? {
                stream.on_text_delta(&reply)?;
                return Ok(reply);
            }
            // Release the lock before inference — same pattern as run_turn.
            let service = {
                let guard = self
                    .agent_service
                    .lock()
                    .map_err(|_| anyhow!("agent service mutex poisoned"))?;
                guard
                    .as_ref()
                    .context("the native agent runtime is not configured")?
                    .clone()
            };
            let mut priming_stream =
                NativeStreamingVoicePrimer::new(stream, self.voice_player.as_ref());
            let response = service.run_turn_streaming(
                TurnRequest {
                    prompt: prompt.to_string(),
                    session_id: Some(NATIVE_SESSION_ID.to_string()),
                    voice_mode: false,
                },
                &mut priming_stream,
            )?;
            priming_stream.finish(&response.reply);
            Ok(response.reply)
        }

        /// If `prompt` is a `/settings` command, execute it and return the reply.
        /// Returns `None` if this is a regular agent prompt.
        fn handle_settings_command(&self, prompt: &str) -> Result<Option<String>> {
            let trimmed = prompt.trim();
            if !trimmed.starts_with("/settings") {
                return Ok(None);
            }
            let args = trimmed["/settings".len()..].trim();
            if args.is_empty() || args == "help" {
                return Ok(Some(settings_help().to_string()));
            }
            let cmd = SettingsCommand::parse(args)?;
            let reply = cmd.apply(&self.workspace_root)?;
            // Reload the agent service's base config so changes take effect on the next turn.
            if let Ok(guard) = self.agent_service.lock() {
                if let Some(service) = guard.as_ref() {
                    service.reload_config();
                }
            }
            Ok(Some(reply))
        }

        /// Runs an agent turn triggered by voice input and pipes the reply to TTS directly,
        /// without needing C++ callbacks. Used by the STT dispatch thread.
        fn run_voice_turn(&self, prompt: &str) -> Result<()> {
            // Release the lock before inference — same pattern as run_turn.
            let service = {
                let guard = self
                    .agent_service
                    .lock()
                    .map_err(|_| anyhow!("agent service mutex poisoned"))?;
                guard
                    .as_ref()
                    .context("the native agent runtime is not configured")?
                    .clone()
            };

            // If the user interrupted the last response, prepend a note so Kurisu is aware.
            let was_interrupted = VOICE_WAS_INTERRUPTED.swap(false, Ordering::Relaxed);
            let effective_prompt = if was_interrupted {
                format!("[Note: your previous response was interrupted mid-sentence — you were cut off. Acknowledge briefly if relevant.]\n\n{prompt}")
            } else {
                prompt.to_string()
            };

            let mut voice_stream = SttVoiceEnqueueStream::new(self.voice_player.as_ref());
            let _response = service.run_turn_streaming(
                TurnRequest {
                    prompt: effective_prompt,
                    session_id: Some(NATIVE_SESSION_ID.to_string()),
                    voice_mode: true,
                },
                &mut voice_stream,
            )?;
            voice_stream.flush_remaining();
            Ok(())
        }
    }

    fn initialize_native_voice_player(
        tts_config: TtsRuntimeConfig,
    ) -> (Option<NativeVoicePlayer>, Option<String>) {
        if !tts_config.enabled {
            return (None, None);
        }

        match build_native_voice_player(&tts_config) {
            Ok(player) => (Some(player), None),
            Err(error) => (None, Some(error.to_string())),
        }
    }

    fn build_native_voice_player(tts_config: &TtsRuntimeConfig) -> Result<NativeVoicePlayer> {
        let service =
            TtsService::new(tts_config.clone()).map_err(|error| anyhow!(error.to_string()))?;
        NativeVoicePlayer::new(service)
    }

    fn initialize_native_stt(
        config: crate::stt::config::SttRuntimeConfig,
    ) -> (Option<Arc<SttService>>, Option<String>) {
        if !config.enabled {
            return (None, None);
        }

        match SttService::new(config) {
            Ok(stt) => (Some(Arc::new(stt)), None),
            Err(error) => (None, Some(error)),
        }
    }

    /// Streams agent reply text directly to the voice player, sentence by sentence.
    /// Used when STT triggers a voice turn without C++ callback involvement.
    struct SttVoiceEnqueueStream<'a> {
        voice_player: Option<&'a NativeVoicePlayer>,
        buffer: String,
        start_generation: u64,
    }

    impl<'a> SttVoiceEnqueueStream<'a> {
        fn new(voice_player: Option<&'a NativeVoicePlayer>) -> Self {
            let start_generation = voice_player.map_or(0, |p| p.current_generation());
            Self {
                voice_player,
                buffer: String::new(),
                start_generation,
            }
        }

        /// Returns true if the user cleared the player mid-stream (i.e. pressed Esc).
        fn was_interrupted(&self) -> bool {
            self.voice_player
                .map_or(false, |p| p.current_generation() != self.start_generation)
        }

        fn flush_remaining(&mut self) {
            if self.was_interrupted() {
                self.buffer.clear();
                return;
            }
            let Some(player) = self.voice_player else {
                return;
            };
            let remaining = self.buffer.trim().to_string();
            self.buffer.clear();
            if !remaining.is_empty() {
                let _ = player.enqueue(&remaining);
            }
        }

        fn try_flush_sentence(&mut self) {
            if self.was_interrupted() {
                self.buffer.clear();
                return;
            }
            let Some(player) = self.voice_player else {
                return;
            };

            let boundary = self
                .buffer
                .rfind(|c| matches!(c, '.' | '!' | '?' | '\n' | '。' | '！' | '？'));

            if let Some(pos) = boundary {
                let end = pos + self.buffer[pos..].chars().next().map_or(1, char::len_utf8);
                let segment = self.buffer[..end].trim().to_string();
                self.buffer = self.buffer[end..].to_string();
                if !segment.is_empty() {
                    let _ = player.enqueue(&segment);
                }
            }
        }
    }

    impl TextStreamSink for SttVoiceEnqueueStream<'_> {
        fn on_text_delta(&mut self, delta: &str) -> anyhow::Result<()> {
            if self.was_interrupted() {
                self.buffer.clear();
                return Ok(());
            }
            self.buffer.push_str(delta);
            self.try_flush_sentence();
            Ok(())
        }

        fn on_tool_call_round(&mut self, _: &[ModelToolCall]) -> anyhow::Result<()> {
            self.buffer.clear();
            Ok(())
        }
    }

    /// Runs a voice turn synchronously. Only updates UI state if this thread still
    /// holds the current turn ID (i.e., no newer turn has been spawned since).
    fn dispatch_stt_transcript(text: &str, turn_id: u64) {
        let Some(runtime) = native_ui_runtime() else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }

        if let Err(e) = runtime.run_voice_turn(text) {
            eprintln!("STT dispatch failed: {e}");
        }

        // Only update state if we're still the latest turn
        if CURRENT_TURN_ID.load(Ordering::Relaxed) == turn_id {
            if runtime
                .stt_service
                .as_ref()
                .is_some_and(|s| s.is_listening())
            {
                NATIVE_STT_STATE.store(STT_STATE_LISTENING, Ordering::Relaxed);
            } else {
                NATIVE_STT_STATE.store(STT_STATE_IDLE, Ordering::Relaxed);
            }
        }
    }

    fn set_native_stt_state(state: i32) {
        NATIVE_STT_STATE.store(state, Ordering::Relaxed);
    }

    struct NativeStreamCallbackAdapter {
        user_data: *mut c_void,
        on_text_delta: Option<NativeTextDeltaCallback>,
        on_event: Option<NativeStreamEventCallback>,
    }

    impl NativeStreamCallbackAdapter {
        fn emit_text(&self, text: &str) {
            if let Some(callback) = self.on_text_delta {
                let text = sanitize_c_string(text);
                unsafe {
                    callback(self.user_data, text.as_ptr());
                }
            }
        }

        fn emit_event(&self, kind: i32, message: &str) {
            if let Some(callback) = self.on_event {
                let message = sanitize_c_string(message);
                unsafe {
                    callback(self.user_data, kind, message.as_ptr());
                }
            }
        }
    }

    impl TextStreamSink for NativeStreamCallbackAdapter {
        fn on_text_delta(&mut self, delta: &str) -> Result<()> {
            self.emit_text(delta);
            Ok(())
        }

        fn on_tool_call_round(&mut self, tool_calls: &[ModelToolCall]) -> Result<()> {
            self.emit_event(
                NATIVE_STREAM_EVENT_TOOL_ROUND,
                &format_tool_round_message(tool_calls),
            );
            Ok(())
        }
    }

    fn format_tool_round_message(tool_calls: &[ModelToolCall]) -> String {
        let names = tool_calls
            .iter()
            .map(|tool_call| tool_call.name.trim())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();

        if names.is_empty() {
            return "Running tools...".to_string();
        }

        format!("Running tools: {}", names.join(", "))
    }

    struct NativeStreamingVoicePrimer<'a> {
        downstream: &'a mut dyn TextStreamSink,
        voice_player: Option<&'a NativeVoicePlayer>,
        streamed_reply: String,
        primed_segments: HashSet<String>,
        last_primed_chars: usize,
    }

    impl<'a> NativeStreamingVoicePrimer<'a> {
        fn new(
            downstream: &'a mut dyn TextStreamSink,
            voice_player: Option<&'a NativeVoicePlayer>,
        ) -> Self {
            Self {
                downstream,
                voice_player,
                streamed_reply: String::new(),
                primed_segments: HashSet::new(),
                last_primed_chars: 0,
            }
        }

        fn finish(&mut self, full_reply: &str) {
            self.streamed_reply.clear();
            self.streamed_reply.push_str(full_reply);
            self.prime_buffer(true);
        }

        fn reset(&mut self) {
            self.streamed_reply.clear();
            self.primed_segments.clear();
            self.last_primed_chars = 0;
        }

        fn maybe_prime_after_delta(&mut self, delta: &str) {
            self.streamed_reply.push_str(delta);

            let total_chars = self.streamed_reply.chars().count();
            if !contains_primeable_boundary(delta)
                && total_chars < self.last_primed_chars + VOICE_PRIME_MIN_ADVANCE_CHARS
            {
                return;
            }

            self.prime_buffer(false);
        }

        fn prime_buffer(&mut self, force: bool) {
            let Some(voice_player) = self.voice_player else {
                return;
            };

            let total_chars = self.streamed_reply.chars().count();
            if !force && total_chars < self.last_primed_chars + VOICE_PRIME_MIN_ADVANCE_CHARS {
                return;
            }

            for segment in collect_primeable_tts_segments(&self.streamed_reply) {
                if self.primed_segments.insert(segment.clone()) {
                    voice_player.prime(&segment);
                }
            }

            self.last_primed_chars = total_chars;
        }
    }

    impl TextStreamSink for NativeStreamingVoicePrimer<'_> {
        fn on_text_delta(&mut self, delta: &str) -> Result<()> {
            self.downstream.on_text_delta(delta)?;
            self.maybe_prime_after_delta(delta);
            Ok(())
        }

        fn on_tool_call_round(&mut self, tool_calls: &[ModelToolCall]) -> Result<()> {
            self.reset();
            self.downstream.on_tool_call_round(tool_calls)
        }
    }

    fn contains_primeable_boundary(text: &str) -> bool {
        text.chars().any(is_primeable_tts_boundary)
    }

    fn is_primeable_tts_boundary(ch: char) -> bool {
        matches!(ch, '.' | '!' | '?' | '\n' | '\r' | '。' | '！' | '？')
    }

    fn collect_primeable_tts_segments(text: &str) -> Vec<String> {
        let filtered = filter_for_tts(text);
        let filtered = filtered.trim();
        if filtered.is_empty() {
            return Vec::new();
        }

        let mut segments = Vec::new();
        let mut start = 0usize;

        for (index, ch) in filtered.char_indices() {
            if !is_primeable_tts_boundary(ch) {
                continue;
            }

            let end = index + ch.len_utf8();
            push_primeable_tts_segment(&mut segments, &filtered[start..end]);
            start = end;
        }

        if start < filtered.len() {
            push_primeable_tts_segment(&mut segments, &filtered[start..]);
        }

        segments
    }

    fn push_primeable_tts_segment(segments: &mut Vec<String>, candidate: &str) {
        let candidate = candidate.trim();
        if candidate.is_empty() || !is_japanese(candidate) {
            return;
        }

        if segments
            .last()
            .is_some_and(|existing| existing == candidate)
        {
            return;
        }

        segments.push(candidate.to_string());
    }

    struct NativeVoicePlayer {
        tts: Arc<TtsService>,
        sender: mpsc::Sender<VoiceCommand>,
        generation: AtomicU64,
    }

    enum VoiceCommand {
        Clear { generation: u64 },
        Enqueue { generation: u64, text: String },
    }

    enum SegmentPauseOutcome {
        Continue,
        Cleared,
        Disconnected,
    }

    struct NativeLipSyncTracker {
        sample_rate: u32,
        queued_samples: Vec<f32>,
        playback_started_at: Option<Instant>,
        smoothed_value: f32,
    }

    impl NativeLipSyncTracker {
        fn new() -> Self {
            Self {
                sample_rate: 0,
                queued_samples: Vec::new(),
                playback_started_at: None,
                smoothed_value: 0.0,
            }
        }

        fn clear(&mut self) {
            self.sample_rate = 0;
            self.queued_samples.clear();
            self.playback_started_at = None;
            self.smoothed_value = 0.0;
            set_native_lip_sync_value(0.0);
        }

        fn append_samples(&mut self, sample_rate: u32, samples: &[f32]) {
            if samples.is_empty() {
                return;
            }

            if self.sample_rate != 0 && self.sample_rate != sample_rate {
                self.clear();
            }

            self.sample_rate = sample_rate;
            self.queued_samples.extend_from_slice(samples);
        }

        fn playback_started(&mut self) {
            if self.playback_started_at.is_none() {
                self.playback_started_at = Some(Instant::now());
            }
        }

        fn update(&mut self) {
            let Some(started_at) = self.playback_started_at else {
                set_native_lip_sync_value(0.0);
                return;
            };
            if self.sample_rate == 0 || self.queued_samples.is_empty() {
                set_native_lip_sync_value(0.0);
                return;
            }

            let played_samples = ((started_at.elapsed().as_secs_f64() * self.sample_rate as f64)
                as usize)
                .min(self.queued_samples.len());
            let window_samples = ((self.sample_rate as usize * LIP_SYNC_WINDOW_MS) / 1000).max(1);
            let window_start = played_samples.saturating_sub(window_samples / 2);
            let window_end = (window_start + window_samples).min(self.queued_samples.len());

            let target =
                lip_sync_target_from_samples(&self.queued_samples[window_start..window_end]);
            self.smoothed_value = if target >= self.smoothed_value {
                target
            } else {
                self.smoothed_value * 0.72 + target * 0.28
            };
            if self.smoothed_value < 0.005 && target < 0.005 {
                self.smoothed_value = 0.0;
            }
            set_native_lip_sync_value(self.smoothed_value);
        }
    }

    struct DecodedAudioChunk {
        sample_rate: u32,
        samples: Vec<f32>,
    }

    impl NativeVoicePlayer {
        fn new(tts: TtsService) -> Result<Self> {
            let tts = Arc::new(tts);
            let (sender, receiver) = mpsc::channel();
            let (ready_to, ready_from) = mpsc::channel();
            let worker_tts = Arc::clone(&tts);

            thread::Builder::new()
                .name("amadeus-native-voice".to_string())
                .spawn(move || run_voice_worker(worker_tts, receiver, ready_to))?;

            ready_from
                .recv()
                .map_err(|_| anyhow!("native voice worker did not finish initialization"))??;

            tts.preload().map_err(|error| anyhow!(error.to_string()))?;

            Ok(Self {
                tts,
                sender,
                generation: AtomicU64::new(0),
            })
        }

        fn clear(&self) {
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.sender.send(VoiceCommand::Clear { generation });
        }

        fn current_generation(&self) -> u64 {
            self.generation.load(Ordering::SeqCst)
        }

        fn enqueue(&self, text: &str) -> Result<()> {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(());
            }

            self.sender
                .send(VoiceCommand::Enqueue {
                    generation: self.generation.load(Ordering::SeqCst),
                    text: trimmed.to_string(),
                })
                .map_err(|_| anyhow!("native voice worker is unavailable"))
        }

        fn prime(&self, text: &str) {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }

            self.tts.prime(TtsRequest {
                text: trimmed.to_string(),
                speaker: None,
                language: current_tts_language_override(),
            });
        }
    }

    fn run_voice_worker(
        tts: Arc<TtsService>,
        command_receiver: mpsc::Receiver<VoiceCommand>,
        ready_to: mpsc::Sender<Result<()>>,
    ) {
        let (output_stream, output_handle) = match OutputStream::try_default() {
            Ok(stream) => {
                let _ = ready_to.send(Ok(()));
                stream
            }
            Err(error) => {
                let _ = ready_to.send(Err(anyhow!(
                    "failed to open the default audio device: {error}"
                )));
                return;
            }
        };
        let _output_stream = output_stream;
        let mut current_generation = 0u64;
        let mut pending = std::collections::VecDeque::new();

        loop {
            let command = if let Some(command) = pending.pop_front() {
                command
            } else {
                // Queue emptied — Kurisu finished speaking. Clear the playing flag,
                // set a short post-echo mute window, and flush the STT buffer so
                // captured TTS audio doesn't trigger a spurious turn.
                IS_TTS_PLAYING.store(false, Ordering::Relaxed);
                set_tts_mute_window(1_200);
                if let Some(runtime) = NATIVE_UI_RUNTIME.get() {
                    if let Some(stt) = &runtime.stt_service {
                        stt.clear_buffer();
                    }
                }
                match command_receiver.recv() {
                    Ok(command) => command,
                    Err(_) => break,
                }
            };

            match command {
                VoiceCommand::Clear { generation } => {
                    current_generation = generation;
                    pending.clear();
                    IS_TTS_PLAYING.store(false, Ordering::Relaxed);
                    set_native_lip_sync_value(0.0);
                }
                VoiceCommand::Enqueue { generation, text } => {
                    if generation != current_generation {
                        continue;
                    }

                    let text = filter_for_tts(&text);
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }

                    let pause_after_segment = segment_pause_duration(&text);
                    let sink = match Sink::try_new(&output_handle) {
                        Ok(sink) => sink,
                        Err(_) => continue,
                    };

                    // Mark TTS as actively playing so the dispatch loop can distinguish
                    // user interruptions from post-TTS echo.
                    IS_TTS_PLAYING.store(true, Ordering::Relaxed);

                    let use_non_streaming = should_use_non_streaming_voice_path(&text);
                    let mut stream = None;
                    let mut lip_sync = NativeLipSyncTracker::new();
                    let mut synthesis_done = false;
                    let mut playback_started = false;
                    let mut buffered_samples = 0usize;
                    let mut required_start_buffer_samples = 0usize;

                    if use_non_streaming {
                        let chunk = match decode_full_synthesized_segment(tts.as_ref(), &text) {
                            Ok(chunk) => chunk,
                            Err(error) => {
                                set_native_error(error.to_string());
                                continue;
                            }
                        };
                        lip_sync.append_samples(chunk.sample_rate, &chunk.samples);
                        sink.append(SamplesBuffer::new(1, chunk.sample_rate, chunk.samples));
                        set_native_error("");
                        lip_sync.playback_started();
                        sink.play();
                        playback_started = true;
                        synthesis_done = true;
                    } else {
                        required_start_buffer_samples = initial_stream_start_buffer_samples(&text);
                        stream = match tts.synthesize_streaming(TtsRequest {
                            text: text.clone(),
                            speaker: None,
                            language: current_tts_language_override(),
                        }) {
                            Ok(stream) => Some(stream),
                            Err(error) => {
                                set_native_error(error.to_string());
                                continue;
                            }
                        };
                    }

                    let mut segment_cleared = false;

                    loop {
                        if !synthesis_done {
                            if let Some(stream) = stream.as_ref() {
                                match stream.recv_timeout(Duration::from_millis(20)) {
                                    Ok(TtsStreamEvent::Audio(chunk)) => {
                                        set_native_error("");
                                        buffered_samples += chunk.samples.len();
                                        lip_sync.append_samples(chunk.sample_rate, &chunk.samples);
                                        sink.append(SamplesBuffer::new(
                                            1,
                                            chunk.sample_rate,
                                            chunk.samples,
                                        ));
                                        if !playback_started
                                            && buffered_samples >= required_start_buffer_samples
                                        {
                                            lip_sync.playback_started();
                                            sink.play();
                                            playback_started = true;
                                        }
                                    }
                                    Ok(TtsStreamEvent::Finished) => {
                                        synthesis_done = true;
                                    }
                                    Ok(TtsStreamEvent::Error(error)) => {
                                        set_native_error(error.to_string());
                                        synthesis_done = true;
                                    }
                                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                                        synthesis_done = true;
                                    }
                                }
                            }
                        }

                        if playback_started {
                            lip_sync.update();
                        }

                        if synthesis_done && !playback_started && !sink.empty() {
                            lip_sync.playback_started();
                            sink.play();
                            playback_started = true;
                        }

                        if synthesis_done && sink.empty() {
                            lip_sync.clear();
                            break;
                        }

                        match command_receiver.try_recv() {
                            Ok(VoiceCommand::Clear { generation }) => {
                                current_generation = generation;
                                pending.clear();
                                sink.stop();
                                lip_sync.clear();
                                segment_cleared = true;
                                break;
                            }
                            Ok(command) => pending.push_back(command),
                            Err(TryRecvError::Empty) => {
                                if !synthesis_done {
                                    continue;
                                }
                                thread::sleep(Duration::from_millis(20));
                            }
                            Err(TryRecvError::Disconnected) => {
                                sink.stop();
                                return;
                            }
                        }
                    }

                    if segment_cleared {
                        continue;
                    }

                    lip_sync.clear();

                    match wait_for_segment_gap(
                        &command_receiver,
                        &mut pending,
                        &mut current_generation,
                        pause_after_segment,
                    ) {
                        SegmentPauseOutcome::Continue => {}
                        SegmentPauseOutcome::Cleared => continue,
                        SegmentPauseOutcome::Disconnected => return,
                    }
                }
            }
        }
    }

    fn segment_pause_duration(text: &str) -> Duration {
        match last_spoken_boundary_char(text) {
            Some('\n') | Some('\r') => Duration::from_millis(VOICE_LINE_GAP_MS),
            Some('.') | Some('!') | Some('?') | Some('…') | Some('。') | Some('！')
            | Some('？') => Duration::from_millis(VOICE_HARD_GAP_MS),
            Some(',') | Some(';') | Some(':') | Some('、') | Some('，') | Some('；')
            | Some('：') => Duration::from_millis(VOICE_SOFT_GAP_MS),
            _ => Duration::ZERO,
        }
    }

    fn last_spoken_boundary_char(text: &str) -> Option<char> {
        let mut chars = text.trim_end().chars().rev().peekable();

        while let Some(&ch) = chars.peek() {
            if is_trailing_speech_closer(ch) {
                chars.next();
                continue;
            }
            break;
        }

        chars.next()
    }

    fn is_trailing_speech_closer(ch: char) -> bool {
        matches!(
            ch,
            ')' | ']' | '}' | '"' | '\'' | '»' | '”' | '’' | '」' | '』' | '】'
        )
    }

    fn should_use_non_streaming_voice_path(text: &str) -> bool {
        let threshold = if is_japanese(text) {
            VOICE_NON_STREAMING_JAPANESE_CHAR_THRESHOLD
        } else {
            VOICE_NON_STREAMING_CHAR_THRESHOLD
        };

        text.chars().count() <= threshold
    }

    fn initial_stream_start_buffer_samples(text: &str) -> usize {
        if should_prebuffer_mixed_language_segment(text) {
            24_000usize * MIXED_LANGUAGE_STREAM_PREBUFFER_MS / 1000
        } else {
            0
        }
    }

    fn should_prebuffer_mixed_language_segment(text: &str) -> bool {
        should_prebuffer_mixed_japanese_stream(text)
    }

    fn decode_full_synthesized_segment(tts: &TtsService, text: &str) -> Result<DecodedAudioChunk> {
        let wav = tts
            .synthesize(TtsRequest {
                text: text.to_string(),
                speaker: None,
                language: current_tts_language_override(),
            })
            .map_err(|error| anyhow!(error.to_string()))?;

        let mut reader = hound::WavReader::new(Cursor::new(wav))
            .context("failed to decode the synthesized fallback WAV")?;
        let spec = reader.spec();
        if spec.channels != 1 {
            bail!(
                "fallback synthesis produced {} channels, expected mono",
                spec.channels
            );
        }

        let samples = match (spec.sample_format, spec.bits_per_sample) {
            (SampleFormat::Int, 16) => reader
                .samples::<i16>()
                .map(|sample| sample.map(|sample| sample as f32 / i16::MAX as f32))
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to read the synthesized i16 fallback WAV")?,
            (SampleFormat::Float, 32) => reader
                .samples::<f32>()
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to read the synthesized f32 fallback WAV")?,
            _ => {
                bail!(
                    "unsupported fallback WAV format: {:?} {}-bit",
                    spec.sample_format,
                    spec.bits_per_sample
                )
            }
        };

        Ok(DecodedAudioChunk {
            sample_rate: spec.sample_rate,
            samples,
        })
    }

    fn lip_sync_target_from_samples(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }

        let mean_square =
            samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
        normalize_lip_sync_rms(mean_square.sqrt())
    }

    fn normalize_lip_sync_rms(rms: f32) -> f32 {
        let normalized =
            ((rms - LIP_SYNC_MIN_RMS) / (LIP_SYNC_MAX_RMS - LIP_SYNC_MIN_RMS)).clamp(0.0, 1.0);
        normalized.sqrt()
    }

    fn wait_for_segment_gap(
        command_receiver: &mpsc::Receiver<VoiceCommand>,
        pending: &mut std::collections::VecDeque<VoiceCommand>,
        current_generation: &mut u64,
        duration: Duration,
    ) -> SegmentPauseOutcome {
        if duration.is_zero() {
            return SegmentPauseOutcome::Continue;
        }

        let deadline = Instant::now() + duration;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return SegmentPauseOutcome::Continue;
            }

            let timeout = std::cmp::min(
                Duration::from_millis(20),
                deadline.saturating_duration_since(now),
            );
            match command_receiver.recv_timeout(timeout) {
                Ok(VoiceCommand::Clear { generation }) => {
                    *current_generation = generation;
                    pending.clear();
                    return SegmentPauseOutcome::Cleared;
                }
                Ok(command) => pending.push_back(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return SegmentPauseOutcome::Disconnected;
                }
            }
        }
    }

    fn initialize_native_ui_runtime(workspace_root: &Path) {
        let _ = NATIVE_UI_RUNTIME.get_or_init(|| NativeUiRuntime::initialize(workspace_root));
        initialize_providers(workspace_root);

        // Spawn STT dispatch thread after the runtime is stored in the OnceLock
        if let Some(runtime) = NATIVE_UI_RUNTIME.get() {
            if let Some(stt) = &runtime.stt_service {
                if let Some(transcript_rx) = stt.take_transcript_receiver() {
                    thread::Builder::new()
                        .name("amadeus-stt-dispatch".to_string())
                        .spawn(move || {
                            while let Ok(SttTranscript { text, is_final }) = transcript_rx.recv() {
                                if is_final {
                                    let tts_active = IS_TTS_PLAYING.load(Ordering::Relaxed);
                                    let echo_window = tts_echo_suppressed();

                                    if tts_active {
                                        // User spoke while Kurisu was talking — interrupt immediately.
                                        VOICE_WAS_INTERRUPTED.store(true, Ordering::Relaxed);
                                        IS_TTS_PLAYING.store(false, Ordering::Relaxed);
                                        set_tts_mute_window(0);
                                        if let Some(rt) = NATIVE_UI_RUNTIME.get() {
                                            if let Some(player) = &rt.voice_player {
                                                player.clear();
                                            }
                                            if let Some(stt) = &rt.stt_service {
                                                stt.clear_buffer();
                                            }
                                        }
                                        // Fall through to spawn the new turn below.
                                    } else if echo_window {
                                        // Post-TTS echo window — discard.
                                        set_native_stt_partial_text("");
                                        set_native_stt_state(STT_STATE_LISTENING);
                                        if let Some(rt) = NATIVE_UI_RUNTIME.get() {
                                            if let Some(stt) = &rt.stt_service {
                                                stt.clear_buffer();
                                            }
                                        }
                                        continue;
                                    } else if NATIVE_STT_STATE.load(Ordering::Relaxed)
                                        == STT_STATE_RESPONDING
                                    {
                                        // Already generating a response — this final is most
                                        // likely a VAD re-trigger from a brief pause, not a
                                        // new utterance.  Discard to avoid queuing duplicate
                                        // LLM requests.
                                        continue;
                                    }

                                    // Spawn the turn on its own thread so the dispatch loop
                                    // stays free to receive and act on the next transcript.
                                    let turn_id =
                                        CURRENT_TURN_ID.fetch_add(1, Ordering::Relaxed) + 1;
                                    set_native_stt_partial_text("");
                                    set_native_stt_state(STT_STATE_RESPONDING);
                                    let text_owned = text.clone();
                                    thread::Builder::new()
                                        .name("amadeus-voice-turn".to_string())
                                        .spawn(move || {
                                            dispatch_stt_transcript(&text_owned, turn_id)
                                        })
                                        .ok();
                                } else if !IS_TTS_PLAYING.load(Ordering::Relaxed)
                                    && !tts_echo_suppressed()
                                {
                                    set_native_stt_partial_text(&text);
                                    set_native_stt_state(STT_STATE_PROCESSING);
                                }
                            }
                        })
                        .ok();
                }
            }
        }
    }

    fn native_ui_runtime() -> Option<&'static NativeUiRuntime> {
        NATIVE_UI_RUNTIME.get()
    }

    struct CurrentDirGuard {
        original_dir: PathBuf,
    }

    impl CurrentDirGuard {
        fn change_to(target_dir: &Path) -> Result<Self> {
            let original_dir =
                env::current_dir().context("failed to read the current directory")?;
            env::set_current_dir(target_dir)
                .with_context(|| format!("failed to enter {}", target_dir.display()))?;
            Ok(Self { original_dir })
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.original_dir);
        }
    }

    fn prepare_shader_runtime(manifest_dir: &Path) -> Result<PathBuf> {
        let runtime_dir = manifest_dir.join("target").join("amadeus-native-runtime");
        let font_source_dir = manifest_dir.join("assets").join(NATIVE_FONT_DIR_NAME);
        let shader_source_dir = resolve_cubism_framework_src(manifest_dir)?
            .join("Rendering")
            .join("OpenGL")
            .join("Shaders")
            .join("Standard");
        let shader_runtime_dir = runtime_dir.join("FrameworkShaders");
        let font_runtime_dir = runtime_dir.join(NATIVE_FONT_DIR_NAME);

        copy_directory(&shader_source_dir, &shader_runtime_dir).with_context(|| {
            format!(
                "failed to prepare native shader runtime from {}",
                shader_source_dir.display()
            )
        })?;
        copy_directory(&font_source_dir, &font_runtime_dir).with_context(|| {
            format!(
                "failed to prepare native font runtime from {}",
                font_source_dir.display()
            )
        })?;

        let logo_src = manifest_dir.join("assets").join("app").join("logo.png");
        if logo_src.exists() {
            std::fs::copy(&logo_src, runtime_dir.join("logo.png"))
                .context("failed to copy app logo to runtime directory")?;
        }

        Ok(runtime_dir)
    }

    fn resolve_cubism_framework_src(manifest_dir: &Path) -> Result<PathBuf> {
        if let Some(override_dir) = env::var_os(CUBISM_SDK_DIR_ENV) {
            let override_dir = normalize_resource_path(manifest_dir, PathBuf::from(override_dir));
            if override_dir.exists() {
                return Ok(override_dir.join("Framework").join("src"));
            }

            bail!(
                "{CUBISM_SDK_DIR_ENV} points to a missing Cubism SDK: {}",
                override_dir.display()
            );
        }

        if let Some(override_dir) = env::var_os(CUBISM_FRAMEWORK_DIR_ENV) {
            let override_dir = normalize_resource_path(manifest_dir, PathBuf::from(override_dir));
            if override_dir.exists() {
                return Ok(override_dir);
            }

            bail!(
                "{CUBISM_FRAMEWORK_DIR_ENV} points to a missing Cubism Framework directory: {}",
                override_dir.display()
            );
        }

        let tracked_dir = manifest_dir
            .join(THIRD_PARTY_DIR_NAME)
            .join(CUBISM_FRAMEWORK_DIR_NAME)
            .join("src");
        if tracked_dir.exists() {
            return Ok(tracked_dir);
        }

        let preferred_dir = manifest_dir
            .join(LOCAL_RESOURCE_DIR_NAME)
            .join(CUBISM_SDK_DIR_NAME)
            .join("Framework")
            .join("src");
        if preferred_dir.exists() {
            return Ok(preferred_dir);
        }

        let legacy_dir = manifest_dir
            .join(CUBISM_SDK_DIR_NAME)
            .join("Framework")
            .join("src");
        if legacy_dir.exists() {
            return Ok(legacy_dir);
        }

        bail!(
            "Cubism Framework not found. Expected {}, {}, or {}",
            tracked_dir.display(),
            preferred_dir.display(),
            legacy_dir.display()
        )
    }

    fn normalize_resource_path(manifest_dir: &Path, candidate: PathBuf) -> PathBuf {
        if candidate.is_absolute() {
            candidate
        } else {
            manifest_dir.join(candidate)
        }
    }

    fn copy_directory(source_dir: &Path, destination_dir: &Path) -> Result<()> {
        fs::create_dir_all(destination_dir)
            .with_context(|| format!("failed to create {}", destination_dir.display()))?;

        for entry in fs::read_dir(source_dir)
            .with_context(|| format!("failed to read {}", source_dir.display()))?
        {
            let entry = entry?;
            let entry_type = entry.file_type()?;
            let destination_path = destination_dir.join(entry.file_name());

            if entry_type.is_dir() {
                copy_directory(&entry.path(), &destination_path)?;
            } else if entry_type.is_file() {
                fs::copy(entry.path(), &destination_path).with_context(|| {
                    format!(
                        "failed to copy {} to {}",
                        entry.path().display(),
                        destination_path.display()
                    )
                })?;
            }
        }

        Ok(())
    }

    fn path_to_cstring(path: &Path) -> Result<CString> {
        CString::new(path.as_os_str().to_string_lossy().into_owned())
            .with_context(|| format!("path contains a NUL byte: {}", path.display()))
    }

    fn sanitize_c_string(value: &str) -> CString {
        CString::new(value.replace('\0', " ")).unwrap_or_else(|_| {
            CString::new("native bridge string encoding failed")
                .expect("fallback C string should be valid")
        })
    }

    fn native_error_storage() -> &'static Mutex<CString> {
        static STORAGE: OnceLock<Mutex<CString>> = OnceLock::new();
        STORAGE.get_or_init(|| Mutex::new(sanitize_c_string("")))
    }

    fn current_tts_language_override() -> Option<String> {
        match NATIVE_VOICE_LANG_PREF.load(Ordering::Relaxed) {
            VOICE_LANG_ENGLISH => Some("english".to_string()),
            VOICE_LANG_JAPANESE => Some("japanese".to_string()),
            _ => None,
        }
    }

    fn set_native_lip_sync_value(value: f32) {
        NATIVE_LIP_SYNC_VALUE_BITS.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn native_stt_partial_text_storage() -> &'static Mutex<CString> {
        static STORAGE: OnceLock<Mutex<CString>> = OnceLock::new();
        STORAGE.get_or_init(|| Mutex::new(sanitize_c_string("")))
    }

    fn set_native_stt_partial_text(text: &str) {
        if let Ok(mut slot) = native_stt_partial_text_storage().lock() {
            *slot = sanitize_c_string(text);
        }
    }

    fn set_native_error(message: impl Into<String>) {
        if let Ok(mut slot) = native_error_storage().lock() {
            *slot = sanitize_c_string(&message.into());
        }
    }

    fn read_last_error_message() -> Option<String> {
        unsafe {
            let pointer = amadeus_cubism_viewer_last_error_message();
            if pointer.is_null() {
                return None;
            }

            CStr::from_ptr(pointer)
                .to_str()
                .ok()
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(ToOwned::to_owned)
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_bridge_status_message() -> *const c_char {
        native_ui_runtime()
            .map(|runtime| runtime.status_message.as_ptr())
            .unwrap_or(ptr::null())
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_agent_available() -> i32 {
        native_ui_runtime()
            .and_then(|runtime| runtime.agent_service.lock().ok())
            .map(|guard| i32::from(guard.is_some()))
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_voice_available() -> i32 {
        native_ui_runtime()
            .map(|runtime| i32::from(runtime.voice_enabled))
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_backend_last_error_message() -> *const c_char {
        native_error_storage()
            .lock()
            .map(|message| message.as_ptr())
            .unwrap_or(ptr::null())
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_lip_sync_value() -> f32 {
        f32::from_bits(NATIVE_LIP_SYNC_VALUE_BITS.load(Ordering::Relaxed))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_agent_turn(prompt: *const c_char) -> *mut c_char {
        let result = (|| -> Result<String> {
            let runtime = native_ui_runtime().context("native runtime was not initialized")?;
            if prompt.is_null() {
                bail!("prompt pointer was null")
            }

            let prompt = unsafe { CStr::from_ptr(prompt) }
                .to_str()
                .context("prompt was not valid UTF-8")?;
            runtime.run_turn(prompt)
        })();

        match result {
            Ok(reply) => sanitize_c_string(&reply).into_raw(),
            Err(error) => {
                set_native_error(error.to_string());
                ptr::null_mut()
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_agent_turn_stream(
        prompt: *const c_char,
        user_data: *mut c_void,
        on_text_delta: Option<NativeTextDeltaCallback>,
        on_event: Option<NativeStreamEventCallback>,
    ) -> i32 {
        let mut callback_adapter = NativeStreamCallbackAdapter {
            user_data,
            on_text_delta,
            on_event,
        };

        let result = (|| -> Result<String> {
            let runtime = native_ui_runtime().context("native runtime was not initialized")?;
            if prompt.is_null() {
                bail!("prompt pointer was null")
            }

            let prompt = unsafe { CStr::from_ptr(prompt) }
                .to_str()
                .context("prompt was not valid UTF-8")?;
            runtime.run_turn_streaming(prompt, &mut callback_adapter)
        })();

        match result {
            Ok(reply) => {
                callback_adapter.emit_event(NATIVE_STREAM_EVENT_COMPLETED, &reply);
                1
            }
            Err(error) => {
                let message = error.to_string();
                set_native_error(message.clone());
                callback_adapter.emit_event(NATIVE_STREAM_EVENT_ERROR, &message);
                0
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_free_string(value: *mut c_char) {
        if value.is_null() {
            return;
        }

        unsafe {
            drop(CString::from_raw(value));
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_voice_clear() {
        set_native_lip_sync_value(0.0);
        if let Some(player) = native_ui_runtime().and_then(|runtime| runtime.voice_player.as_ref())
        {
            if IS_TTS_PLAYING.load(Ordering::Relaxed) {
                VOICE_WAS_INTERRUPTED.store(true, Ordering::Relaxed);
                IS_TTS_PLAYING.store(false, Ordering::Relaxed);
                set_tts_mute_window(0);
            }
            player.clear();
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_voice_enqueue(text: *const c_char) -> i32 {
        let result = (|| -> Result<()> {
            let runtime = native_ui_runtime().context("native runtime was not initialized")?;
            let player = runtime
                .voice_player
                .as_ref()
                .context("native voice playback is unavailable")?;
            if text.is_null() {
                bail!("voice text pointer was null")
            }

            let text = unsafe { CStr::from_ptr(text) }
                .to_str()
                .context("voice text was not valid UTF-8")?;
            player.enqueue(text)
        })();

        match result {
            Ok(()) => 1,
            Err(error) => {
                set_native_lip_sync_value(0.0);
                set_native_error(error.to_string());
                0
            }
        }
    }

    /// Plays an audio file at `path` in a background thread and returns its
    /// duration in milliseconds (or the `fallback_ms` value on any error).
    /// The caller uses the returned duration to sync frame animation to the audio.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_boot_audio_play(path: *const c_char, fallback_ms: u32) -> u32 {
        use rodio::Source as _;
        use std::fs::File;
        use std::io::BufReader;

        let path_str = if path.is_null() {
            return fallback_ms;
        } else {
            match unsafe { CStr::from_ptr(path) }.to_str() {
                Ok(s) => s.to_owned(),
                Err(_) => return fallback_ms,
            }
        };

        let file = match File::open(&path_str) {
            Ok(f) => f,
            Err(_) => return fallback_ms,
        };

        let decoder = match rodio::Decoder::new(BufReader::new(file)) {
            Ok(d) => d,
            Err(_) => return fallback_ms,
        };

        let duration_ms: u32 = decoder
            .total_duration()
            .map(|d| d.as_millis() as u32)
            .unwrap_or(fallback_ms);

        // Play on a background thread so the C++ render loop is not blocked
        thread::spawn(move || {
            let file2 = match File::open(&path_str) {
                Ok(f) => f,
                Err(_) => return,
            };
            let decoder2 = match rodio::Decoder::new(BufReader::new(file2)) {
                Ok(d) => d,
                Err(_) => return,
            };
            if let Ok((_stream, handle)) = OutputStream::try_default() {
                if let Ok(sink) = Sink::try_new(&handle) {
                    sink.append(decoder2);
                    sink.sleep_until_end();
                }
            }
        });

        duration_ms
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_available() -> i32 {
        native_ui_runtime()
            .map(|runtime| i32::from(runtime.stt_enabled))
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_state() -> i32 {
        NATIVE_STT_STATE.load(Ordering::Relaxed)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_start() -> i32 {
        let Some(runtime) = native_ui_runtime() else {
            return 0;
        };
        let Some(stt) = runtime.stt_service.as_ref() else {
            return 0;
        };
        stt.start_listening();
        set_native_stt_state(STT_STATE_LISTENING);
        1
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_stop() -> i32 {
        let Some(runtime) = native_ui_runtime() else {
            return 0;
        };
        let Some(stt) = runtime.stt_service.as_ref() else {
            return 0;
        };
        stt.stop_listening();
        set_native_stt_state(STT_STATE_IDLE);
        1
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_set_sensitivity(level: i32) {
        if let Some(runtime) = native_ui_runtime() {
            if let Some(stt) = runtime.stt_service.as_ref() {
                stt.set_sensitivity(level);
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_voice_set_language(lang: i32) {
        let value = match lang {
            1 => VOICE_LANG_ENGLISH,
            2 => VOICE_LANG_JAPANESE,
            _ => VOICE_LANG_AUTO,
        };
        NATIVE_VOICE_LANG_PREF.store(value, Ordering::Relaxed);
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_agent_runtime_info() -> *const c_char {
        NATIVE_RUNTIME_INFO
            .get()
            .map(|s| s.as_ptr())
            .unwrap_or(ptr::null())
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_device_count() -> i32 {
        SttService::device_count() as i32
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_device_name(index: i32) -> *const c_char {
        // We materialise a CString per call and store it in a thread-local to give
        // the C++ caller a stable pointer for the duration of its stack frame.
        // This is safe because C++ only holds the pointer while inside CaptureSnapshot.
        use std::cell::RefCell;
        thread_local! {
            static SCRATCH: RefCell<Option<CString>> = const { RefCell::new(None) };
        }
        match SttService::device_name_at(index as usize) {
            Some(name) => SCRATCH.with(|s| {
                let cs = sanitize_c_string(&name);
                let ptr = cs.as_ptr();
                *s.borrow_mut() = Some(cs);
                ptr
            }),
            None => ptr::null(),
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_select_device(index: i32) {
        if let Some(runtime) = native_ui_runtime() {
            if let Some(stt) = runtime.stt_service.as_ref() {
                stt.set_device(index as usize);
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_mic_level() -> f32 {
        SttService::mic_level()
    }

    /// Returns the index of the device the STT worker actually has open (-1 = none/default).
    /// C++ should use this to keep its displayed device index in sync after failed switches.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_active_device_index() -> i32 {
        SttService::active_device_index()
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_set_mic_gain_db(db: f32) {
        if let Some(rt) = native_ui_runtime() {
            if let Some(stt) = &rt.stt_service {
                stt.set_mic_gain_db(db);
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_set_mic_gate(threshold: f32) {
        if let Some(rt) = native_ui_runtime() {
            if let Some(stt) = &rt.stt_service {
                stt.set_mic_gate(threshold);
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_set_mic_compressor(threshold_db: f32, ratio: f32) {
        if let Some(rt) = native_ui_runtime() {
            if let Some(stt) = &rt.stt_service {
                stt.set_mic_compressor(threshold_db, ratio);
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_partial_text() -> *const c_char {
        native_stt_partial_text_storage()
            .lock()
            .map(|g| g.as_ptr())
            .unwrap_or(ptr::null())
    }

    // ── providers bridge ──────────────────────────────────────────────────────

    fn initialize_providers(workspace_root: &Path) {
        let store = ProvidersStore::load(workspace_root);
        let active_idx = store.active_index().map(|i| i as i32).unwrap_or(-1);
        NATIVE_ACTIVE_PROVIDER_INDEX.store(active_idx, Ordering::Relaxed);

        let names: Vec<CString> = store
            .profiles()
            .iter()
            .map(|p| sanitize_c_string(&p.name))
            .collect();

        let _ = NATIVE_PROVIDERS_NAMES.set(names);
        let _ = NATIVE_PROVIDERS_STORE.set(Mutex::new(store));
        let _ = NATIVE_PROVIDER_CATALOG.set(ProviderCatalog::init());
    }

    /// Returns the number of provider profiles saved in `.amadeus/providers.json`.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_providers_count() -> i32 {
        NATIVE_PROVIDERS_NAMES
            .get()
            .map(|names| names.len() as i32)
            .unwrap_or(0)
    }

    /// Returns the display name of the provider at `index`, or null if out of range.
    /// The pointer is valid until the next call on the same thread.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_providers_name_at(index: i32) -> *const c_char {
        NATIVE_PROVIDERS_NAMES
            .get()
            .and_then(|names| names.get(index as usize))
            .map(|cs| cs.as_ptr())
            .unwrap_or(ptr::null())
    }

    /// Returns the index of the currently-active provider profile, or -1 if none is active.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_providers_active_index() -> i32 {
        NATIVE_ACTIVE_PROVIDER_INDEX.load(Ordering::Relaxed)
    }

    /// Select the provider profile at `index`: write its settings to `config.json` and reload
    /// the agent's in-memory config. Has no effect if `index` is out of range.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_providers_select(index: i32) {
        let result = (|| -> anyhow::Result<()> {
            let store = NATIVE_PROVIDERS_STORE
                .get()
                .context("providers store was not initialised")?;
            let guard = store
                .lock()
                .map_err(|_| anyhow::anyhow!("providers store mutex poisoned"))?;
            guard.select(index as usize)?;
            NATIVE_ACTIVE_PROVIDER_INDEX.store(index, Ordering::Relaxed);
            if let Some(runtime) = native_ui_runtime() {
                if runtime.use_external_agent {
                    // External agent — provider selection is a no-op for the local service.
                    return Ok(());
                }
                if let Ok(mut svc_guard) = runtime.agent_service.lock() {
                    if let Some(service) = svc_guard.as_ref() {
                        service.reload_config();
                    } else {
                        let workspace_root = runtime.workspace_root.clone();
                        if let Ok(mut runtime_cfg) =
                            AgentRuntimeConfig::load(Some(workspace_root), None)
                        {
                            if runtime_cfg.services.local_llm {
                                runtime_cfg.provider = crate::agent::config::LlmProvider::LlamaCpp;
                            }
                            runtime_cfg.normalize_provider_defaults();
                            if runtime_cfg.model.is_some() || runtime_cfg.services.local_llm {
                                *svc_guard =
                                    Some(Arc::new(AgentUiService::new(runtime_cfg)) as Arc<dyn ConversationBackend>);
                            }
                        }
                    }
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            set_native_error(e.to_string());
        }
    }

    // ── provider catalog bridge ────────────────────────────────────────────────
    //
    // Provider type indices (must match C++ overlay constants):
    //   0 = Anthropic
    //   1 = OpenAI
    //   2 = Gemini
    //   3 = OpenAI-compatible (custom endpoint)
    //   4 = Ollama            (fetches model list from /api/tags)
    //   5 = Llama.cpp         (user-supplied .gguf path)
    //   6 = Amadeus           (hardcoded Qwen3-4B-q8_0.gguf)

    const AMADEUS_BUILTIN_MODEL_PATH: &str = "assets/models/llm/Qwen3-4B-q8_0.gguf";

    static NATIVE_OLLAMA_MODELS: OnceLock<Mutex<Vec<CString>>> = OnceLock::new();
    // 0=idle/not-fetched  1=fetching  2=done  3=error
    static NATIVE_OLLAMA_FETCH_STATUS: AtomicI32 = AtomicI32::new(0);

    fn native_ollama_models() -> &'static Mutex<Vec<CString>> {
        NATIVE_OLLAMA_MODELS.get_or_init(|| Mutex::new(Vec::new()))
    }

    struct ProviderCatalog {
        display_names: Vec<CString>,
    }

    impl ProviderCatalog {
        fn init() -> Self {
            let display_names = [
                "Anthropic",
                "OpenAI",
                "Gemini",
                "OpenAI-compatible",
                "Ollama",
                "Llama.cpp",
                "Amadeus (built-in)",
            ]
            .iter()
            .map(|s| sanitize_c_string(s))
            .collect();
            Self { display_names }
        }
    }

    // ── helpers ────────────────────────────────────────────────────────────────

    fn read_config_top_key(workspace_root: &Path, key: &str) -> Option<String> {
        let path = workspace_root.join(crate::agent::config::DEFAULT_CONFIG_PATH);
        let raw = fs::read_to_string(path).ok()?;
        let json: Value = serde_json::from_str(&raw).ok()?;
        json.get(key)?.as_str().map(|s| s.to_string())
    }

    fn read_config_json(workspace_root: &Path) -> Value {
        let path = workspace_root.join(crate::agent::config::DEFAULT_CONFIG_PATH);
        fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
    }

    fn load_or_create_config_json(path: &Path) -> Result<Value> {
        if path.exists() {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse {}", path.display()))
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            Ok(Value::Object(serde_json::Map::new()))
        }
    }

    fn set_services_key(root: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
        root.entry("services")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .expect("services must be a JSON object")
            .insert(key.into(), value);
    }

    fn workspace_root() -> Option<PathBuf> {
        native_ui_runtime().map(|rt| rt.workspace_root.clone())
    }

    // Per-field string storages for returning C pointers safely.
    fn provider_field_storage(slot: usize) -> &'static Mutex<CString> {
        static SLOTS: OnceLock<[Mutex<CString>; 4]> = OnceLock::new();
        &SLOTS.get_or_init(|| {
            [
                Mutex::new(sanitize_c_string("")),
                Mutex::new(sanitize_c_string("")),
                Mutex::new(sanitize_c_string("")),
                Mutex::new(sanitize_c_string("")),
            ]
        })[slot]
    }

    fn store_provider_field(slot: usize, value: &str) -> *const c_char {
        let mut guard = provider_field_storage(slot).lock().unwrap();
        *guard = sanitize_c_string(value);
        guard.as_ptr()
    }

    // ── bridge functions ───────────────────────────────────────────────────────

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_type_count() -> i32 {
        NATIVE_PROVIDER_CATALOG
            .get()
            .map(|c| c.display_names.len() as i32)
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_type_name(index: i32) -> *const c_char {
        NATIVE_PROVIDER_CATALOG
            .get()
            .and_then(|c| c.display_names.get(index as usize))
            .map(|cs| cs.as_ptr())
            .unwrap_or(ptr::null())
    }

    /// Detect which of the 6 UI provider types is currently written in config.json.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_active_type_index() -> i32 {
        let root_path = match workspace_root() {
            Some(p) => p,
            None => return 0,
        };
        let json = read_config_json(&root_path);

        let local_llm = json
            .get("services")
            .and_then(|s| s.get("localLlm"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if local_llm {
            // Amadeus vs Llama.cpp: check if path matches the built-in model.
            let model_path = json
                .get("services")
                .and_then(|s| s.get("localLlmModelPath"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if model_path.contains("Qwen3-4B-q8_0") {
                return 6; // Amadeus
            }
            return 5; // generic Llama.cpp
        }

        let provider_str = json
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("openai-chat");

        match crate::agent::config::LlmProvider::parse(provider_str)
            .unwrap_or(crate::agent::config::LlmProvider::OpenAiChat)
        {
            crate::agent::config::LlmProvider::Anthropic => 0,
            crate::agent::config::LlmProvider::OpenAiChat
            | crate::agent::config::LlmProvider::OpenAiResponses => {
                // Distinguish OpenAI from OpenAI-compatible by checking if the endpoint
                // differs from the official OpenAI base.
                let api_base = json.get("apiBase").and_then(|v| v.as_str()).unwrap_or("");
                if api_base.is_empty() || api_base == "https://api.openai.com/v1" {
                    1 // OpenAI
                } else {
                    3 // OpenAI-compatible
                }
            }
            crate::agent::config::LlmProvider::Gemini => 2,
            crate::agent::config::LlmProvider::Ollama => 4,
            crate::agent::config::LlmProvider::LlamaCpp => 5,
        }
    }

    /// Return the current `model` value from config (slot 0).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_current_model() -> *const c_char {
        let value = workspace_root()
            .and_then(|r| read_config_top_key(&r, "model"))
            .unwrap_or_default();
        store_provider_field(0, &value)
    }

    /// Return the current `apiBase` value from config (slot 1).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_current_endpoint() -> *const c_char {
        let value = workspace_root()
            .and_then(|r| read_config_top_key(&r, "apiBase"))
            .unwrap_or_default();
        store_provider_field(1, &value)
    }

    /// Return the current `apiKey` value from config (slot 2).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_current_apikey() -> *const c_char {
        let value = workspace_root()
            .and_then(|r| read_config_top_key(&r, "apiKey"))
            .unwrap_or_default();
        store_provider_field(2, &value)
    }

    /// Return the current `services.localLlmModelPath` from config (slot 3).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_current_model_path() -> *const c_char {
        let value = workspace_root()
            .map(|r| read_config_json(&r))
            .and_then(|json| {
                json.get("services")?
                    .get("localLlmModelPath")?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        store_provider_field(3, &value)
    }

    /// Write a complete provider configuration to config.json.
    ///
    /// - `type_index`: 0=Anthropic 1=OpenAI 2=Gemini 3=OpenAI-compat 4=Llama.cpp 5=Amadeus
    /// - `model`:    model name (or .gguf path for Llama.cpp); pass empty for Amadeus
    /// - `endpoint`: API base URL (used for type 3 and optionally others); pass empty to keep default
    /// - `api_key`:  API key; pass empty to leave unchanged
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_provider_set_config(
        type_index: i32,
        model: *const c_char,
        endpoint: *const c_char,
        api_key: *const c_char,
    ) {
        let result = (|| -> Result<()> {
            let root_path = workspace_root().context("runtime not initialised")?;
            let config_path = root_path.join(crate::agent::config::DEFAULT_CONFIG_PATH);
            let mut json = load_or_create_config_json(&config_path)?;
            let root = json
                .as_object_mut()
                .context("config.json must be a JSON object")?;

            let model_str = unsafe { cstr_to_string(model) };
            let endpoint_str = unsafe { cstr_to_string(endpoint) };
            let api_key_str = unsafe { cstr_to_string(api_key) };

            match type_index {
                // ── Anthropic ──────────────────────────────────────────────────
                0 => {
                    root.insert("provider".into(), Value::String("anthropic".into()));
                    set_services_key(root, "localLlm", Value::Bool(false));
                    root.remove("apiBase");
                    if !model_str.is_empty() {
                        root.insert("model".into(), Value::String(model_str));
                    }
                    if !api_key_str.is_empty() {
                        root.insert("apiKey".into(), Value::String(api_key_str));
                    }
                }
                // ── OpenAI ────────────────────────────────────────────────────
                1 => {
                    root.insert("provider".into(), Value::String("openai-chat".into()));
                    set_services_key(root, "localLlm", Value::Bool(false));
                    root.insert(
                        "apiBase".into(),
                        Value::String("https://api.openai.com/v1".into()),
                    );
                    if !model_str.is_empty() {
                        root.insert("model".into(), Value::String(model_str));
                    }
                    if !api_key_str.is_empty() {
                        root.insert("apiKey".into(), Value::String(api_key_str));
                    }
                }
                // ── Gemini ────────────────────────────────────────────────────
                2 => {
                    root.insert("provider".into(), Value::String("gemini".into()));
                    set_services_key(root, "localLlm", Value::Bool(false));
                    root.remove("apiBase");
                    if !model_str.is_empty() {
                        root.insert("model".into(), Value::String(model_str));
                    }
                    if !api_key_str.is_empty() {
                        root.insert("apiKey".into(), Value::String(api_key_str));
                    }
                }
                // ── OpenAI-compatible ─────────────────────────────────────────
                3 => {
                    root.insert("provider".into(), Value::String("openai-chat".into()));
                    set_services_key(root, "localLlm", Value::Bool(false));
                    if !endpoint_str.is_empty() {
                        root.insert("apiBase".into(), Value::String(endpoint_str));
                    }
                    if !model_str.is_empty() {
                        root.insert("model".into(), Value::String(model_str));
                    }
                    if !api_key_str.is_empty() {
                        root.insert("apiKey".into(), Value::String(api_key_str));
                    }
                }
                // ── Ollama ────────────────────────────────────────────────────
                4 => {
                    root.insert("provider".into(), Value::String("ollama".into()));
                    set_services_key(root, "localLlm", Value::Bool(false));
                    let base = if endpoint_str.is_empty() {
                        "http://127.0.0.1:11434".to_string()
                    } else {
                        endpoint_str
                    };
                    root.insert("apiBase".into(), Value::String(base));
                    if !model_str.is_empty() {
                        root.insert("model".into(), Value::String(model_str));
                    }
                    root.remove("apiKey");
                }
                // ── Llama.cpp (user-supplied path) ────────────────────────────
                5 => {
                    root.insert("provider".into(), Value::String("llama-cpp".into()));
                    set_services_key(root, "localLlm", Value::Bool(true));
                    root.remove("model");
                    root.remove("apiBase");
                    root.remove("apiKey");
                    if !model_str.is_empty() {
                        set_services_key(root, "localLlmModelPath", Value::String(model_str));
                    }
                }
                // ── Amadeus built-in (Qwen3-4B-q8_0) ─────────────────────────
                6 => {
                    root.insert("provider".into(), Value::String("llama-cpp".into()));
                    set_services_key(root, "localLlm", Value::Bool(true));
                    set_services_key(
                        root,
                        "localLlmModelPath",
                        Value::String(AMADEUS_BUILTIN_MODEL_PATH.into()),
                    );
                    root.remove("model");
                    root.remove("apiBase");
                    root.remove("apiKey");
                }
                _ => {}
            }

            let pretty =
                serde_json::to_string_pretty(&json).context("failed to serialise config.json")?;
            fs::write(&config_path, pretty + "\n")
                .with_context(|| format!("failed to write {}", config_path.display()))?;

            if let Some(rt) = native_ui_runtime() {
                if !rt.use_external_agent {
                    let mut guard = rt
                        .agent_service
                        .lock()
                        .map_err(|_| anyhow::anyhow!("agent service mutex poisoned"))?;
                    if let Some(service) = guard.as_ref() {
                        // Service already exists — just reload config in place.
                        service.reload_config();
                    } else {
                        // Service was not initialized (e.g. first-time config save) — build it now.
                        let workspace_root = rt.workspace_root.clone();
                        if let Ok(mut runtime_cfg) =
                            AgentRuntimeConfig::load(Some(workspace_root), None)
                        {
                            if runtime_cfg.services.local_llm {
                                runtime_cfg.provider = crate::agent::config::LlmProvider::LlamaCpp;
                            }
                            runtime_cfg.normalize_provider_defaults();
                            if runtime_cfg.model.is_some() || runtime_cfg.services.local_llm {
                                *guard = Some(Arc::new(AgentUiService::new(runtime_cfg)) as Arc<dyn ConversationBackend>);
                            }
                        }
                    }
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            set_native_error(e.to_string());
        }
    }

    /// `1` while the model is inside a `<think>…</think>` block, `0` otherwise.
    /// Used by the overlay to drive future "thinking" animation states.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_llm_thinking() -> i32 {
        #[cfg(feature = "local-llm")]
        {
            i32::from(crate::agent::llm::llama_cpp::is_thinking())
        }
        #[cfg(not(feature = "local-llm"))]
        { 0 }
    }

    /// `1` while the local LLM background preload thread is still running (model not yet
    /// fully loaded), `0` once the model is ready (or if no local LLM is used).
    /// Used by the overlay to show a "Loading model..." badge.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_llm_loading() -> i32 {
        let ready = native_ui_runtime()
            .and_then(|rt| rt.agent_service.lock().ok())
            .and_then(|guard| guard.as_ref().map(|svc| svc.is_ready()))
            .unwrap_or(true);
        i32::from(!ready)
    }

    /// Convert a nullable C string pointer to an owned Rust String (empty if null).
    unsafe fn cstr_to_string(ptr: *const c_char) -> String {
        if ptr.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .trim()
            .to_string()
    }

    // ── Ollama model-list bridge ───────────────────────────────────────────────

    /// Kick off a background fetch of `GET <endpoint>/api/tags`.
    /// Status transitions: 0 (idle) → 1 (fetching) → 2 (done) | 3 (error).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_ollama_fetch_models(endpoint: *const c_char) {
        let base = unsafe { cstr_to_string(endpoint) };
        let base = if base.is_empty() {
            "http://127.0.0.1:11434".to_string()
        } else {
            base.trim_end_matches('/').to_string()
        };

        NATIVE_OLLAMA_FETCH_STATUS.store(1, Ordering::Relaxed);

        thread::Builder::new()
            .name("amadeus-ollama-fetch".into())
            .spawn(move || {
                let url = format!("{base}/api/tags");
                let result = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(8))
                    .build()
                    .and_then(|c| c.get(&url).send())
                    .and_then(|r| r.json::<serde_json::Value>());

                match result {
                    Ok(json) => {
                        let names: Vec<CString> = json["models"]
                            .as_array()
                            .iter()
                            .flat_map(|arr| arr.iter())
                            .filter_map(|m| m["name"].as_str())
                            .map(|s| sanitize_c_string(s))
                            .collect();
                        *native_ollama_models().lock().unwrap() = names;
                        NATIVE_OLLAMA_FETCH_STATUS.store(2, Ordering::Relaxed);
                    }
                    Err(e) => {
                        eprintln!("[amadeus] ollama fetch failed: {e}");
                        NATIVE_OLLAMA_FETCH_STATUS.store(3, Ordering::Relaxed);
                    }
                }
            })
            .ok();
    }

    /// 0=idle  1=fetching  2=done  3=error
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_ollama_fetch_status() -> i32 {
        NATIVE_OLLAMA_FETCH_STATUS.load(Ordering::Relaxed)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_ollama_model_count() -> i32 {
        native_ollama_models()
            .lock()
            .map(|g| g.len() as i32)
            .unwrap_or(0)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_ollama_model_at(index: i32) -> *const c_char {
        native_ollama_models()
            .lock()
            .ok()
            .and_then(|g| g.get(index as usize).map(|cs| cs.as_ptr()))
            .unwrap_or(ptr::null())
    }

    /// Find the index of `model_name` in the fetched list, or -1 if not found.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_ollama_model_index(model_name: *const c_char) -> i32 {
        let name = unsafe { cstr_to_string(model_name) };
        if name.is_empty() {
            return 0;
        }
        native_ollama_models()
            .lock()
            .ok()
            .and_then(|g| {
                g.iter()
                    .position(|cs| cs.to_string_lossy() == name.as_str())
                    .map(|i| i as i32)
            })
            .unwrap_or(0)
    }

    // ── GGUF model download bridge ─────────────────────────────────────────────

    // 0=idle  1=downloading  2=done  3=error
    static NATIVE_GGUF_DOWNLOAD_STATUS: AtomicI32 = AtomicI32::new(0);
    // 0–100
    static NATIVE_GGUF_DOWNLOAD_PROGRESS: AtomicI32 = AtomicI32::new(0);

    const GGUF_HF_URL: &str =
        "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q8_0.gguf";

    /// Returns the resolved model path for type 5 or 6, or None for other types.
    fn gguf_model_path_for_type(type_index: i32) -> Option<PathBuf> {
        match type_index {
            5 => workspace_root().map(|r| {
                let json = read_config_json(&r);
                let path_str = json
                    .get("services")
                    .and_then(|s| s.get("localLlmModelPath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if path_str.is_empty() {
                    PathBuf::new()
                } else {
                    PathBuf::from(path_str)
                }
            }),
            6 => workspace_root().map(|r| r.join(AMADEUS_BUILTIN_MODEL_PATH)),
            _ => None,
        }
    }

    /// Returns 1 if the model file for the given provider type exists on disk, 0 otherwise.
    /// Only meaningful for types 5 (Llama.cpp) and 6 (Amadeus built-in).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_gguf_model_exists(type_index: i32) -> i32 {
        gguf_model_path_for_type(type_index)
            .map(|p| {
                if !p.as_os_str().is_empty() && p.exists() {
                    1
                } else {
                    0
                }
            })
            .unwrap_or(0)
    }

    /// Kick off a background download of the GGUF model for the given provider type.
    /// Only types 5 and 6 are supported. For type 6 the path is hardcoded;
    /// for type 5 the path is read from the current config.
    /// Status: 0=idle → 1=downloading → 2=done | 3=error.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_gguf_download_start(type_index: i32) {
        let dest = match gguf_model_path_for_type(type_index) {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => return,
        };

        // Don't start a new download if one is already running.
        if NATIVE_GGUF_DOWNLOAD_STATUS.load(Ordering::Relaxed) == 1 {
            return;
        }

        NATIVE_GGUF_DOWNLOAD_STATUS.store(1, Ordering::Relaxed);
        NATIVE_GGUF_DOWNLOAD_PROGRESS.store(0, Ordering::Relaxed);

        thread::Builder::new()
            .name("amadeus-gguf-download".into())
            .spawn(move || {
                use std::io::{Read, Write};

                if let Some(parent) = dest.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("[amadeus] gguf download: failed to create dir: {e}");
                        NATIVE_GGUF_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                }

                let part_path = dest.with_extension("gguf.part");

                let mut response = match reqwest::blocking::Client::builder()
                    .build()
                    .and_then(|c| c.get(GGUF_HF_URL).send())
                {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("[amadeus] gguf download request failed: {e}");
                        NATIVE_GGUF_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                };

                if !response.status().is_success() {
                    eprintln!("[amadeus] gguf download: HTTP {}", response.status());
                    NATIVE_GGUF_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                    return;
                }

                let total = response.content_length();

                let mut file = match std::fs::File::create(&part_path) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("[amadeus] gguf download: failed to create temp file: {e}");
                        NATIVE_GGUF_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                };

                let mut downloaded: u64 = 0;
                let mut buf = vec![0u8; 1024 * 256];
                loop {
                    let n = match response.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) => {
                            eprintln!("[amadeus] gguf download read error: {e}");
                            NATIVE_GGUF_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                            return;
                        }
                    };
                    if n == 0 {
                        break;
                    }
                    if let Err(e) = file.write_all(&buf[..n]) {
                        eprintln!("[amadeus] gguf download write error: {e}");
                        NATIVE_GGUF_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                    downloaded += n as u64;
                    if let Some(total) = total {
                        let pct = (downloaded * 100 / total).min(99) as i32;
                        NATIVE_GGUF_DOWNLOAD_PROGRESS.store(pct, Ordering::Relaxed);
                    }
                }

                if let Err(e) = std::fs::rename(&part_path, &dest) {
                    eprintln!("[amadeus] gguf download rename failed: {e}");
                    NATIVE_GGUF_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                    return;
                }

                NATIVE_GGUF_DOWNLOAD_PROGRESS.store(100, Ordering::Relaxed);
                NATIVE_GGUF_DOWNLOAD_STATUS.store(2, Ordering::Relaxed);
                eprintln!("[amadeus] GGUF download complete: {}", dest.display());
            })
            .ok();
    }

    /// 0=idle  1=downloading  2=done  3=error
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_gguf_download_status() -> i32 {
        NATIVE_GGUF_DOWNLOAD_STATUS.load(Ordering::Relaxed)
    }

    /// 0–100 percent complete (only meaningful while status == 1).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_gguf_download_progress() -> i32 {
        NATIVE_GGUF_DOWNLOAD_PROGRESS.load(Ordering::Relaxed)
    }

    // ── STT model download bridge ──────────────────────────────────────────────

    // 0=idle  1=downloading  2=done  3=error
    static NATIVE_STT_DOWNLOAD_STATUS: AtomicI32 = AtomicI32::new(0);
    // 0–100
    static NATIVE_STT_DOWNLOAD_PROGRESS: AtomicI32 = AtomicI32::new(0);

    const STT_HF_URL: &str =
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin";
    const STT_MODEL_SUBPATH: &str = "models/stt/ggml-large-v3-turbo-q8_0.bin";

    fn stt_model_path() -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Some(manifest_dir.join("assets").join(STT_MODEL_SUBPATH))
    }

    /// 1 if the STT model file exists on disk, 0 otherwise.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_model_exists() -> i32 {
        stt_model_path()
            .map(|p| if p.exists() { 1 } else { 0 })
            .unwrap_or(0)
    }

    /// Start a background download of the STT (Whisper) model if not already running.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_download_start() {
        let dest = match stt_model_path() {
            Some(p) => p,
            None => return,
        };

        if NATIVE_STT_DOWNLOAD_STATUS.load(Ordering::Relaxed) == 1 {
            return;
        }

        NATIVE_STT_DOWNLOAD_STATUS.store(1, Ordering::Relaxed);
        NATIVE_STT_DOWNLOAD_PROGRESS.store(0, Ordering::Relaxed);

        thread::Builder::new()
            .name("amadeus-stt-download".into())
            .spawn(move || {
                use std::io::{Read, Write};

                if let Some(parent) = dest.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("[amadeus] stt download: failed to create dir: {e}");
                        NATIVE_STT_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                }

                let part_path = dest.with_extension("bin.part");

                let mut response = match reqwest::blocking::Client::builder()
                    .build()
                    .and_then(|c| c.get(STT_HF_URL).send())
                {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("[amadeus] stt download request failed: {e}");
                        NATIVE_STT_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                };

                if !response.status().is_success() {
                    eprintln!("[amadeus] stt download: HTTP {}", response.status());
                    NATIVE_STT_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                    return;
                }

                let total = response.content_length();

                let mut file = match std::fs::File::create(&part_path) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("[amadeus] stt download: failed to create temp file: {e}");
                        NATIVE_STT_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                };

                let mut downloaded: u64 = 0;
                let mut buf = vec![0u8; 1024 * 256];
                loop {
                    let n = match response.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) => {
                            eprintln!("[amadeus] stt download read error: {e}");
                            NATIVE_STT_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                            return;
                        }
                    };
                    if n == 0 {
                        break;
                    }
                    if let Err(e) = file.write_all(&buf[..n]) {
                        eprintln!("[amadeus] stt download write error: {e}");
                        NATIVE_STT_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                        return;
                    }
                    downloaded += n as u64;
                    if let Some(total) = total {
                        let pct = (downloaded * 100 / total).min(99) as i32;
                        NATIVE_STT_DOWNLOAD_PROGRESS.store(pct, Ordering::Relaxed);
                    }
                }

                if let Err(e) = std::fs::rename(&part_path, &dest) {
                    eprintln!("[amadeus] stt download rename failed: {e}");
                    NATIVE_STT_DOWNLOAD_STATUS.store(3, Ordering::Relaxed);
                    return;
                }

                NATIVE_STT_DOWNLOAD_PROGRESS.store(100, Ordering::Relaxed);
                NATIVE_STT_DOWNLOAD_STATUS.store(2, Ordering::Relaxed);
                eprintln!("[amadeus] STT download complete: {}", dest.display());
            })
            .ok();
    }

    /// 0=idle  1=downloading  2=done  3=error
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_download_status() -> i32 {
        NATIVE_STT_DOWNLOAD_STATUS.load(Ordering::Relaxed)
    }

    /// 0–100 percent complete (only meaningful while status == 1).
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_stt_download_progress() -> i32 {
        NATIVE_STT_DOWNLOAD_PROGRESS.load(Ordering::Relaxed)
    }

    // ── TTS model status bridge ────────────────────────────────────────────────

    /// Returns 1 if the TTS HuggingFace cache directory exists, 0 otherwise.
    /// TTS downloads lazily on first synthesis; this just tells the boot screen
    /// whether the cached weights are already present.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_tts_model_cached() -> i32 {
        let cache_base = std::env::var("HF_HUB_CACHE")
            .or_else(|_| std::env::var("HF_HOME").map(|h| format!("{h}/hub")))
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| format!("{h}/.cache/huggingface/hub"))
                    .unwrap_or_else(|_| ".cache/huggingface/hub".to_string())
            });
        let tts_dir = PathBuf::from(cache_base).join("models--Loke-60000--christina-TTS");
        if tts_dir.exists() {
            1
        } else {
            0
        }
    }

    // ── Pre-flight: start downloads before the boot screen ────────────────────

    /// Start all required model downloads that are missing, before the boot sequence
    /// renders. The C++ `RunModelLoadingPhase()` polls the status atomics to show progress.
    fn preflight_model_downloads(assets_root: &Path) {
        let _ = assets_root; // used indirectly via stt_model_path()

        // LLM: only download the Amadeus built-in if it's the configured provider.
        if amadeus_native_provider_active_type_index() == 6
            && amadeus_native_gguf_model_exists(6) == 0
        {
            amadeus_native_gguf_download_start(6);
        } else if amadeus_native_gguf_model_exists(6) == 1 {
            // Model already present — mark done so the boot bar shows Ready immediately.
            NATIVE_GGUF_DOWNLOAD_STATUS.store(2, Ordering::Relaxed);
            NATIVE_GGUF_DOWNLOAD_PROGRESS.store(100, Ordering::Relaxed);
        }

        // STT: download if missing (it's needed regardless of which LLM provider is used).
        if amadeus_native_stt_model_exists() == 0 {
            amadeus_native_stt_download_start();
        } else {
            NATIVE_STT_DOWNLOAD_STATUS.store(2, Ordering::Relaxed);
            NATIVE_STT_DOWNLOAD_PROGRESS.store(100, Ordering::Relaxed);
        }
    }

    // ── Post-boot service initialization ──────────────────────────────────────

    /// Called by C++ (cubism_bridge.cpp) after `RunModelLoadingPhase()` completes.
    /// Initialises TTS, STT, and the agent service — the heavy work that previously
    /// blocked the app before any window appeared.
    #[unsafe(no_mangle)]
    pub extern "C" fn amadeus_native_init_services() {
        let workspace_root = match NATIVE_WORKSPACE_ROOT.get() {
            Some(p) => p.clone(),
            None => {
                eprintln!("[amadeus] amadeus_native_init_services: workspace root not set");
                return;
            }
        };
        initialize_native_ui_runtime(&workspace_root);
    }

    mod tests {
        use std::time::Duration;

        use super::{
            collect_primeable_tts_segments, last_spoken_boundary_char,
            lip_sync_target_from_samples, normalize_lip_sync_rms, segment_pause_duration,
            should_prebuffer_mixed_language_segment, should_use_non_streaming_voice_path,
            VOICE_HARD_GAP_MS, VOICE_SOFT_GAP_MS,
        };

        #[test]
        fn short_segments_use_the_non_streaming_voice_path() {
            assert!(should_use_non_streaming_voice_path("Hello!"));
            assert!(should_use_non_streaming_voice_path("そうです！"));
            assert!(!should_use_non_streaming_voice_path(
                "Hello world. This is long enough to keep the streaming decoder path active."
            ));
        }

        #[test]
        fn japanese_threshold_is_much_smaller_than_english_threshold() {
            assert!(!should_use_non_streaming_voice_path(
                "こんにちは、元気ですか？"
            ));
            assert!(should_use_non_streaming_voice_path("はい。"));
        }

        #[test]
        fn mixed_japanese_and_english_segments_can_use_streaming_when_not_tiny() {
            let mixed = "Japanese: こんにちは。 English: Hello! I am Amadeus.";
            assert!(!should_use_non_streaming_voice_path(mixed));
        }

        #[test]
        fn normal_japanese_sentences_do_not_force_full_wav_fallback() {
            let japanese = "こんにちは。今日はAIについてゆっくり話します。";
            assert!(!should_use_non_streaming_voice_path(japanese));
        }

        #[test]
        fn artificial_segment_cuts_do_not_add_extra_gaps() {
            assert_eq!(
                segment_pause_duration("this was cut mid sentence"),
                Duration::ZERO
            );
            assert_eq!(
                segment_pause_duration("120 characters without punctuation should keep flowing"),
                Duration::ZERO
            );
        }

        #[test]
        fn punctuation_drives_native_segment_gaps() {
            assert_eq!(
                segment_pause_duration("Hello world."),
                Duration::from_millis(VOICE_HARD_GAP_MS)
            );
            assert_eq!(
                segment_pause_duration("Well,"),
                Duration::from_millis(VOICE_SOFT_GAP_MS)
            );
            assert_eq!(
                segment_pause_duration("こんにちは。"),
                Duration::from_millis(VOICE_HARD_GAP_MS)
            );
            assert_eq!(
                segment_pause_duration("ええと、"),
                Duration::from_millis(VOICE_SOFT_GAP_MS)
            );
        }

        #[test]
        fn trailing_quotes_use_the_underlying_boundary() {
            assert_eq!(last_spoken_boundary_char("\"Hello.\""), Some('.'));
            assert_eq!(last_spoken_boundary_char("「了解です。」"), Some('。'));
            assert_eq!(last_spoken_boundary_char("plain text"), Some('t'));
        }

        #[test]
        fn primeable_segments_focus_on_japanese_sentence_units() {
            let segments = collect_primeable_tts_segments(
                "Hello. こんにちは。今日はAIとGPUについて話します。 Final English.",
            );

            assert_eq!(
                segments,
                vec![
                    "こんにちは。".to_string(),
                    "今日はAIとGPUについて話します。".to_string(),
                ]
            );
        }

        #[test]
        fn primeable_segments_keep_the_current_japanese_tail() {
            let segments = collect_primeable_tts_segments("こんにちは。今日はAIについて");

            assert_eq!(
                segments,
                vec!["こんにちは。".to_string(), "今日はAIについて".to_string()]
            );
        }

        #[test]
        fn mixed_language_segments_use_start_buffering() {
            assert!(should_prebuffer_mixed_language_segment(
                "Hello こんにちは hello こんばんは"
            ));
            assert!(should_prebuffer_mixed_language_segment(
                "今日はdeep learning modelについて話します。"
            ));
            assert!(!should_prebuffer_mixed_language_segment(
                "今日はAIとGPUについて話します。"
            ));
            assert!(!should_prebuffer_mixed_language_segment(
                "こんにちは、元気ですか？"
            ));
            assert!(!should_prebuffer_mixed_language_segment(
                "Hello, how are you today?"
            ));
        }

        #[test]
        fn lip_sync_rms_normalization_clamps_to_a_valid_range() {
            assert_eq!(normalize_lip_sync_rms(0.0), 0.0);
            assert_eq!(normalize_lip_sync_rms(10.0), 1.0);

            let moderate = normalize_lip_sync_rms(0.12);
            assert!(moderate > 0.0);
            assert!(moderate < 1.0);
        }

        #[test]
        fn lip_sync_target_tracks_window_energy() {
            assert_eq!(lip_sync_target_from_samples(&[]), 0.0);
            assert_eq!(lip_sync_target_from_samples(&[0.0, 0.0, 0.0]), 0.0);

            let quiet = lip_sync_target_from_samples(&[0.05, -0.05, 0.05, -0.05]);
            let loud = lip_sync_target_from_samples(&[0.8, -0.8, 0.8, -0.8]);

            assert!(quiet > 0.0);
            assert!(loud > quiet);
            assert!(loud <= 1.0);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use anyhow::{bail, Result};

    pub fn run_native_viewer() -> Result<()> {
        bail!("the native Cubism viewer is currently wired up only for Linux")
    }

    pub fn run_native_viewer_with_logs_terminal(_show_logs_terminal: bool) -> Result<()> {
        run_native_viewer()
    }
}

pub use imp::{run_native_viewer, run_native_viewer_with_logs_terminal};
