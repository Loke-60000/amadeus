#[cfg(target_os = "linux")]
mod imp {
    use std::{
        collections::HashSet,
        env,
        ffi::{CStr, CString, c_char, c_void},
        fs,
        io::Cursor,
        path::{Path, PathBuf},
        process::Command,
        ptr,
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicU32, AtomicU64, Ordering},
            mpsc::{self, TryRecvError},
        },
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, anyhow, bail};
    use hound::SampleFormat;
    use rodio::{OutputStream, Sink, buffer::SamplesBuffer};

    use crate::{
        agent::{
            ModelToolCall, TextStreamSink,
            config::AgentRuntimeConfig,
            ui::{AgentUiService, AgentUiTurnRequest},
        },
        live2d::config::Live2dPaths,
        tts::{
            TtsRequest, TtsService, TtsStreamEvent, config::TtsRuntimeConfig,
            detection::is_japanese, discover_tts_runtime_config, filter::filter_for_tts,
            japanese::should_prebuffer_mixed_japanese_stream,
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

    type NativeTextDeltaCallback = unsafe extern "C" fn(*mut c_void, *const c_char);
    type NativeStreamEventCallback = unsafe extern "C" fn(*mut c_void, i32, *const c_char);

    static NATIVE_UI_RUNTIME: OnceLock<NativeUiRuntime> = OnceLock::new();
    static NATIVE_LIP_SYNC_VALUE_BITS: AtomicU32 = AtomicU32::new(0);

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
        initialize_native_ui_runtime(&workspace_root);
        let model_path = live2d
            .model_path
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", live2d.model_path.display()))?;

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
        agent_service: Option<AgentUiService>,
        voice_player: Option<NativeVoicePlayer>,
        agent_enabled: bool,
        voice_enabled: bool,
        status_message: CString,
    }

    impl NativeUiRuntime {
        fn initialize(workspace_root: &Path) -> Self {
            let mut agent_enabled = false;
            let mut provider = "unconfigured".to_string();
            let mut model = "(unset)".to_string();
            let mut agent_error = None;

            let agent_service =
                match AgentRuntimeConfig::load(Some(workspace_root.to_path_buf()), None) {
                    Ok(mut runtime) => {
                        runtime.normalize_provider_defaults();
                        provider = runtime.provider.to_string();
                        model = runtime
                            .model
                            .clone()
                            .unwrap_or_else(|| "(unset)".to_string());
                        if runtime.model.is_some() {
                            agent_enabled = true;
                            Some(AgentUiService::new(runtime))
                        } else {
                            None
                        }
                    }
                    Err(error) => {
                        agent_error = Some(error.to_string());
                        None
                    }
                };

            let (voice_player, voice_error) =
                initialize_native_voice_player(discover_tts_runtime_config());
            let voice_enabled = voice_player.is_some();

            let status = if let Some(error) = agent_error {
                format!("Native renderer is live, but the agent is unavailable: {error}")
            } else if !agent_enabled {
                "Native renderer is live, but no agent model is configured in .amadeus/config.json."
                    .to_string()
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

            Self {
                agent_service,
                voice_player,
                agent_enabled,
                voice_enabled,
                status_message: sanitize_c_string(&status),
            }
        }

        fn run_turn(&self, prompt: &str) -> Result<String> {
            let service = self
                .agent_service
                .as_ref()
                .context("the native agent runtime is not configured")?;
            let response = service.run_turn(AgentUiTurnRequest {
                prompt: prompt.to_string(),
                session_id: Some(NATIVE_SESSION_ID.to_string()),
            })?;
            Ok(response.reply)
        }

        fn run_turn_streaming(
            &self,
            prompt: &str,
            stream: &mut dyn TextStreamSink,
        ) -> Result<String> {
            let service = self
                .agent_service
                .as_ref()
                .context("the native agent runtime is not configured")?;
            let mut priming_stream =
                NativeStreamingVoicePrimer::new(stream, self.voice_player.as_ref());
            let response = service.run_turn_streaming(
                AgentUiTurnRequest {
                    prompt: prompt.to_string(),
                    session_id: Some(NATIVE_SESSION_ID.to_string()),
                },
                &mut priming_stream,
            )?;
            priming_stream.finish(&response.reply);
            Ok(response.reply)
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
            let window_samples =
                ((self.sample_rate as usize * LIP_SYNC_WINDOW_MS) / 1000).max(1);
            let window_start = played_samples.saturating_sub(window_samples / 2);
            let window_end = (window_start + window_samples).min(self.queued_samples.len());

            let target = lip_sync_target_from_samples(&self.queued_samples[window_start..window_end]);
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
                language: None,
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
                match command_receiver.recv() {
                    Ok(command) => command,
                    Err(_) => break,
                }
            };

            match command {
                VoiceCommand::Clear { generation } => {
                    current_generation = generation;
                    pending.clear();
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
                            language: None,
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
                language: None,
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
        let normalized = ((rms - LIP_SYNC_MIN_RMS) / (LIP_SYNC_MAX_RMS - LIP_SYNC_MIN_RMS))
            .clamp(0.0, 1.0);
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

    fn set_native_lip_sync_value(value: f32) {
        NATIVE_LIP_SYNC_VALUE_BITS.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
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
            .map(|runtime| i32::from(runtime.agent_enabled))
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

    #[cfg(test)]
    mod tests {
        use std::time::Duration;

        use super::{
            VOICE_HARD_GAP_MS, VOICE_SOFT_GAP_MS, collect_primeable_tts_segments,
            last_spoken_boundary_char, lip_sync_target_from_samples, normalize_lip_sync_rms,
            segment_pause_duration, should_prebuffer_mixed_language_segment,
            should_use_non_streaming_voice_path,
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
    use anyhow::{Result, bail};

    pub fn run_native_viewer() -> Result<()> {
        bail!("the native Cubism viewer is currently wired up only for Linux")
    }

    pub fn run_native_viewer_with_logs_terminal(_show_logs_terminal: bool) -> Result<()> {
        run_native_viewer()
    }
}

pub use imp::{run_native_viewer, run_native_viewer_with_logs_terminal};
