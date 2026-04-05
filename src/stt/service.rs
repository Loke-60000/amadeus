use std::{
    collections::HashSet,
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use cpal::{
    Device, SampleFormat, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use super::config::SttRuntimeConfig;

const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Maximum audio kept in the rolling buffer: 15 seconds.
const MAX_BUFFER_SAMPLES: usize = WHISPER_SAMPLE_RATE as usize * 15;

/// Minimum audio before running inference: 1 second.
const MIN_INFERENCE_SAMPLES: usize = WHISPER_SAMPLE_RATE as usize;

/// How often to run inference while speech is active (partial transcripts).
const STREAM_INTERVAL: Duration = Duration::from_millis(1_200);

/// Smoothed RMS of the microphone input (post-processing), stored as f32 bits.
static STT_MIC_LEVEL_BITS: AtomicU32 = AtomicU32::new(0);
/// Index of the device currently open for capture. -1 = none.
static STT_ACTIVE_DEVICE_INDEX: AtomicI32 = AtomicI32::new(-1);
/// Device names that have failed to open and should be excluded from the list.
static STT_DEVICE_BLACKLIST: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
/// Current device name list (post-blacklist). Written by service/worker, read by C API.
static STT_DEVICE_NAMES: std::sync::OnceLock<Mutex<Vec<String>>> = std::sync::OnceLock::new();

fn device_blacklist() -> &'static Mutex<HashSet<String>> {
    STT_DEVICE_BLACKLIST.get_or_init(|| Mutex::new(HashSet::new()))
}

fn device_names_list() -> &'static Mutex<Vec<String>> {
    STT_DEVICE_NAMES.get_or_init(|| Mutex::new(Vec::new()))
}

// Mic audio processing parameters — updated live from the UI, applied each chunk.
static MIC_GAIN_DB_BITS: AtomicU32 = AtomicU32::new(0); // 0.0 dB default
static MIC_GATE_THRESHOLD_BITS: AtomicU32 = AtomicU32::new(0); // 0.0 = off
static MIC_COMP_THRESHOLD_DB_BITS: AtomicU32 = AtomicU32::new(0xC1F00000); // -30.0 dB
static MIC_COMP_RATIO_BITS: AtomicU32 = AtomicU32::new(0x3F800000); // 1.0 = off

pub struct SttTranscript {
    pub text: String,
    /// `true` — utterance complete, send to agent.
    /// `false` — partial result, display only.
    pub is_final: bool,
}

enum SttCommand {
    StartListening,
    StopListening,
    SetSensitivity { energy_threshold: f32 },
    SetDevice(usize),
    ClearBuffer,
    Shutdown,
}

pub struct SttService {
    command_tx: mpsc::Sender<SttCommand>,
    listening: Arc<AtomicBool>,
    transcript_rx: Mutex<Option<mpsc::Receiver<SttTranscript>>>,
}

impl SttService {
    pub fn new(config: SttRuntimeConfig) -> Result<Self, String> {
        if !config.model_path.exists() {
            eprintln!("STT: model not found, downloading now...");
            download_model(&config.model_path, &config.model_hf_repo)
                .map_err(|e| format!("STT model download failed: {e}"))?;
        }

        let device_names = enumerate_input_device_names();
        if let Ok(mut guard) = device_names_list().lock() {
            *guard = device_names;
        }

        let (command_tx, command_rx) = mpsc::channel();
        let (transcript_tx, transcript_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let listening = Arc::new(AtomicBool::new(false));
        let listening_clone = listening.clone();

        thread::Builder::new()
            .name("amadeus-stt".to_string())
            .spawn(move || run_stt_worker(config, ready_tx, command_rx, transcript_tx, listening_clone))
            .map_err(|e| e.to_string())?;

        // Block until the worker has loaded the model and created CUDA state on its own thread.
        // All whisper GPU operations must live on the same thread that initializes CUDA.
        ready_rx
            .recv()
            .map_err(|_| "STT worker thread terminated unexpectedly".to_string())??;

        Ok(Self {
            command_tx,
            listening,
            transcript_rx: Mutex::new(Some(transcript_rx)),
        })
    }

    pub fn start_listening(&self) {
        self.listening.store(true, Ordering::Relaxed);
        let _ = self.command_tx.send(SttCommand::StartListening);
    }

    pub fn stop_listening(&self) {
        self.listening.store(false, Ordering::Relaxed);
        let _ = self.command_tx.send(SttCommand::StopListening);
    }

    pub fn is_listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }

    /// 0 = low sensitivity (threshold 0.025), 1 = medium (0.01), 2 = high (0.004).
    pub fn set_sensitivity(&self, level: i32) {
        let threshold = match level {
            0 => 0.025,
            2 => 0.004,
            _ => 0.01,
        };
        let _ = self.command_tx.send(SttCommand::SetSensitivity { energy_threshold: threshold });
    }

    pub fn set_device(&self, index: usize) {
        let _ = self.command_tx.send(SttCommand::SetDevice(index));
    }

    /// Discards all buffered audio and resets speech state. Used to flush echo captured
    /// during TTS playback so it doesn't trigger a transcription after Kurisu stops speaking.
    pub fn clear_buffer(&self) {
        let _ = self.command_tx.send(SttCommand::ClearBuffer);
    }

    /// Number of available (non-blacklisted) input devices.
    pub fn device_count() -> usize {
        device_names_list().lock().map(|v| v.len()).unwrap_or(0)
    }

    /// Name of the device at `index`, or None if out of range.
    pub fn device_name_at(index: usize) -> Option<String> {
        device_names_list()
            .lock()
            .ok()
            .and_then(|v| v.get(index).cloned())
    }

    /// Current smoothed microphone RMS in [0, 1]. Updated from the worker loop after processing.
    pub fn mic_level() -> f32 {
        f32::from_bits(STT_MIC_LEVEL_BITS.load(Ordering::Relaxed))
    }

    /// Mic input gain in dB. Positive values amplify, negative attenuate. Range: [-12, +12].
    pub fn set_mic_gain_db(&self, db: f32) {
        MIC_GAIN_DB_BITS.store(db.clamp(-36.0, 36.0).to_bits(), Ordering::Relaxed);
    }

    /// Noise gate RMS threshold in [0, 1]. Chunks quieter than this are silenced. 0 = off.
    pub fn set_mic_gate(&self, threshold: f32) {
        MIC_GATE_THRESHOLD_BITS.store(threshold.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Compressor settings. threshold_db is the level above which compression kicks in.
    /// ratio > 1.0 enables compression (e.g., 4.0 = 4:1). ratio ≤ 1.0 bypasses.
    pub fn set_mic_compressor(&self, threshold_db: f32, ratio: f32) {
        MIC_COMP_THRESHOLD_DB_BITS.store(threshold_db.to_bits(), Ordering::Relaxed);
        MIC_COMP_RATIO_BITS.store(ratio.max(1.0).to_bits(), Ordering::Relaxed);
    }

    /// Index of the device currently open for capture (-1 = none / using default).
    pub fn active_device_index() -> i32 {
        STT_ACTIVE_DEVICE_INDEX.load(Ordering::Relaxed)
    }

    /// Takes the transcript receiver out of the service. Can only be called once.
    pub fn take_transcript_receiver(&self) -> Option<mpsc::Receiver<SttTranscript>> {
        self.transcript_rx.lock().ok()?.take()
    }
}

impl Drop for SttService {
    fn drop(&mut self) {
        let _ = self.command_tx.send(SttCommand::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// Audio processing chain: gain → noise gate → compressor
// Applied on every captured chunk before speech detection and inference.
// ---------------------------------------------------------------------------

struct AudioProcessor {
    /// Compressor envelope follower (linear RMS).
    comp_envelope: f32,
}

impl AudioProcessor {
    fn new() -> Self {
        Self { comp_envelope: 0.0 }
    }

    fn process(&mut self, samples: &mut Vec<f32>) {
        if samples.is_empty() {
            return;
        }

        let gain_db = f32::from_bits(MIC_GAIN_DB_BITS.load(Ordering::Relaxed));
        let gate = f32::from_bits(MIC_GATE_THRESHOLD_BITS.load(Ordering::Relaxed));
        let comp_threshold_db = f32::from_bits(MIC_COMP_THRESHOLD_DB_BITS.load(Ordering::Relaxed));
        let comp_ratio = f32::from_bits(MIC_COMP_RATIO_BITS.load(Ordering::Relaxed));

        let rms = chunk_rms(samples);

        // Noise gate: silence the chunk if it's below the threshold.
        if gate > 0.0 && rms < gate {
            for s in samples.iter_mut() {
                *s = 0.0;
            }
            self.comp_envelope *= 0.97; // let the compressor envelope decay
            return;
        }

        // Compressor: ratio of 1.0 means bypass.
        let comp_gain = if comp_ratio > 1.01 {
            // Envelope follower with fast attack (~2 chunks) and slow release (~30 chunks).
            let coeff = if rms > self.comp_envelope { 0.40 } else { 0.96 };
            self.comp_envelope = coeff * self.comp_envelope + (1.0 - coeff) * rms;

            let level_db = if self.comp_envelope > 1e-9 {
                20.0 * self.comp_envelope.log10()
            } else {
                -100.0
            };

            if level_db > comp_threshold_db {
                let reduction_db = (comp_threshold_db - level_db) * (1.0 - 1.0 / comp_ratio);
                10.0f32.powf(reduction_db / 20.0)
            } else {
                1.0
            }
        } else {
            self.comp_envelope *= 0.97;
            1.0
        };

        // Gain (in dB) combined with compressor gain reduction.
        let gain_linear = if gain_db.abs() > 0.01 {
            10.0f32.powf(gain_db / 20.0)
        } else {
            1.0
        };

        let total = gain_linear * comp_gain;
        if (total - 1.0).abs() > 0.001 {
            for s in samples.iter_mut() {
                *s = (*s * total).clamp(-1.0, 1.0);
            }
        }
    }
}

/// Returns devices that have a usable capture config AND haven't been blacklisted
/// after a failed stream-open.  Blacklisting happens lazily on first failure so
/// there are no test-open side effects (JACK client noise, race conditions) at startup.
fn real_input_devices() -> Vec<Device> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let blacklist = device_blacklist().lock().unwrap_or_else(|e| e.into_inner());
    devices
        .filter(|d| {
            if select_capture_config(d).is_err() {
                return false;
            }
            if let Ok(name) = d.name() {
                if blacklist.contains(&name) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Adds the device at `index` to the permanent blacklist for this session.
/// Returns true if the blacklist changed (device existed and wasn't already blocked).
fn blacklist_device(index: usize) -> bool {
    let device = real_input_devices().into_iter().nth(index);
    let Some(device) = device else { return false };
    let Ok(name) = device.name() else { return false };
    let mut bl = device_blacklist().lock().unwrap_or_else(|e| e.into_inner());
    bl.insert(name)
}

fn enumerate_input_device_names() -> Vec<String> {
    real_input_devices()
        .into_iter()
        .filter_map(|d| d.name().ok())
        .collect()
}

fn find_input_device(index: Option<usize>) -> Option<Device> {
    match index {
        None => cpal::default_host().default_input_device(),
        Some(n) => real_input_devices().into_iter().nth(n),
    }
}

fn run_stt_worker(
    config: SttRuntimeConfig,
    ready_tx: mpsc::Sender<Result<(), String>>,
    command_rx: mpsc::Receiver<SttCommand>,
    transcript_tx: mpsc::Sender<SttTranscript>,
    _listening: Arc<AtomicBool>,
) {
    lower_stt_worker_priority();

    // Load context and create state on THIS thread so CUDA context ownership stays here.
    eprintln!("STT: loading model...");
    let whisper_ctx = match load_whisper_context(&config.model_path) {
        Ok(ctx) => ctx,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let mut whisper_state = match whisper_ctx.create_state() {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("failed to create whisper state: {e}")));
            return;
        }
    };

    eprintln!("STT: model ready");
    let _ = ready_tx.send(Ok(()));

    let (mut audio_tx, mut audio_rx) = mpsc::sync_channel::<Vec<f32>>(128);

    let mut stream = match build_capture_stream(audio_tx.clone(), None) {
        Ok(s) => {
            STT_ACTIVE_DEVICE_INDEX.store(0, Ordering::Relaxed);
            s
        }
        Err(e) => {
            eprintln!("STT: failed to open microphone: {e}");
            return;
        }
    };

    let mut energy_threshold = config.energy_threshold;
    let silence_to_finalize = Duration::from_millis(config.silence_ms.max(800));

    // Rolling audio buffer and streaming state
    let mut audio_processor = AudioProcessor::new();
    let mut stream_buffer: Vec<f32> = Vec::with_capacity(MAX_BUFFER_SAMPLES);
    let mut last_speech: Option<Instant> = None;
    let mut last_inference = Instant::now();
    let mut last_partial = String::new();
    let mut speaking = false;
    let mut active = false;

    loop {
        // Drain commands without blocking
        loop {
            match command_rx.try_recv() {
                Ok(SttCommand::StartListening) => {
                    if !active {
                        active = true;
                        let _ = stream.play();
                    }
                }
                Ok(SttCommand::StopListening) => {
                    if active {
                        active = false;
                        let _ = stream.pause();
                        reset_stream_state(
                            &mut stream_buffer,
                            &mut last_speech,
                            &mut last_partial,
                            &mut speaking,
                        );
                        STT_MIC_LEVEL_BITS.store(0, Ordering::Relaxed);
                    }
                }
                Ok(SttCommand::SetSensitivity { energy_threshold: t }) => {
                    energy_threshold = t;
                }
                Ok(SttCommand::SetDevice(n)) => {
                    let was_active = active;
                    let _ = stream.pause();
                    let (new_tx, new_rx) = mpsc::sync_channel::<Vec<f32>>(128);
                    match build_capture_stream(new_tx.clone(), Some(n)) {
                        Ok(new_stream) => {
                            stream = new_stream;
                            let _ = std::mem::replace(&mut audio_tx, new_tx);
                            audio_rx = new_rx;
                            STT_ACTIVE_DEVICE_INDEX.store(n as i32, Ordering::Relaxed);
                            if was_active {
                                let _ = stream.play();
                            }
                        }
                        Err(e) => {
                            eprintln!("STT: failed to switch to device {n}: {e}");
                            // Blacklist this device and refresh the name list so C++ can't
                            // navigate to it again.
                            blacklist_device(n);
                            let fresh = enumerate_input_device_names();
                            if let Ok(mut guard) = device_names_list().lock() {
                                *guard = fresh;
                            }
                            // Clamp the active index to the new device count.
                            let count = device_names_list().lock().map(|v| v.len()).unwrap_or(1);
                            let clamped = STT_ACTIVE_DEVICE_INDEX.load(Ordering::Relaxed)
                                .min((count.saturating_sub(1)) as i32)
                                .max(0);
                            STT_ACTIVE_DEVICE_INDEX.store(clamped, Ordering::Relaxed);
                            if was_active {
                                let _ = stream.play();
                            }
                        }
                    }
                    STT_MIC_LEVEL_BITS.store(0, Ordering::Relaxed);
                    reset_stream_state(
                        &mut stream_buffer,
                        &mut last_speech,
                        &mut last_partial,
                        &mut speaking,
                    );
                }
                Ok(SttCommand::ClearBuffer) => {
                    reset_stream_state(
                        &mut stream_buffer,
                        &mut last_speech,
                        &mut last_partial,
                        &mut speaking,
                    );
                }
                Ok(SttCommand::Shutdown) => return,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        if !active {
            thread::sleep(Duration::from_millis(50));
            continue;
        }

        let mut samples = match audio_rx.recv_timeout(Duration::from_millis(80)) {
            Ok(s) => s,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Even during timeout: check if silence has been long enough to finalize
                if speaking {
                    if let Some(last_sp) = last_speech {
                        if last_sp.elapsed() >= silence_to_finalize {
                            finalize_utterance(
                                &mut whisper_state,
                                &transcript_tx,
                                &mut stream_buffer,
                                &mut last_speech,
                                &mut last_partial,
                                &mut speaking,
                            );
                        }
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };

        // Apply gain, noise gate, and compressor.
        audio_processor.process(&mut samples);

        // Mic meter shows the post-processing level.
        update_mic_level(&samples);

        // Accumulate into rolling buffer
        stream_buffer.extend_from_slice(&samples);
        if stream_buffer.len() > MAX_BUFFER_SAMPLES {
            let drop_count = stream_buffer.len() - MAX_BUFFER_SAMPLES;
            stream_buffer.drain(..drop_count);
        }

        // Speech energy detection
        let rms = chunk_rms(&samples);
        if rms >= energy_threshold {
            last_speech = Some(Instant::now());
            speaking = true;
        }

        if !speaking {
            continue;
        }

        // Finalize on silence
        if let Some(last_sp) = last_speech {
            if last_sp.elapsed() >= silence_to_finalize {
                finalize_utterance(
                    &mut whisper_state,
                    &transcript_tx,
                    &mut stream_buffer,
                    &mut last_speech,
                    &mut last_partial,
                    &mut speaking,
                );
                last_inference = Instant::now();
                continue;
            }
        }

        // Streaming partial inference every STREAM_INTERVAL while speech is active
        if last_inference.elapsed() >= STREAM_INTERVAL
            && stream_buffer.len() >= MIN_INFERENCE_SAMPLES
        {
            let text = transcribe_text(&mut whisper_state, &stream_buffer);
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() && !is_hallucination(&trimmed) && trimmed != last_partial {
                let _ = transcript_tx.send(SttTranscript {
                    text: trimmed.clone(),
                    is_final: false,
                });
                last_partial = trimmed;
            }
            last_inference = Instant::now();
        }
    }
}

fn finalize_utterance(
    state: &mut WhisperState,
    transcript_tx: &mpsc::Sender<SttTranscript>,
    stream_buffer: &mut Vec<f32>,
    last_speech: &mut Option<Instant>,
    last_partial: &mut String,
    speaking: &mut bool,
) {
    if stream_buffer.len() >= MIN_INFERENCE_SAMPLES {
        let snapshot = stream_buffer.clone();
        let text = transcribe_text(state, &snapshot);
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() && !is_hallucination(&trimmed) {
            let _ = transcript_tx.send(SttTranscript { text: trimmed, is_final: true });
        }
    }
    reset_stream_state(stream_buffer, last_speech, last_partial, speaking);
}

fn reset_stream_state(
    stream_buffer: &mut Vec<f32>,
    last_speech: &mut Option<Instant>,
    last_partial: &mut String,
    speaking: &mut bool,
) {
    stream_buffer.clear();
    *last_speech = None;
    last_partial.clear();
    *speaking = false;
}

fn build_capture_stream(
    audio_tx: mpsc::SyncSender<Vec<f32>>,
    device_index: Option<usize>,
) -> Result<cpal::Stream, String> {
    let device = find_input_device(device_index)
        .ok_or_else(|| "no microphone found".to_string())?;

    let (supported_config, needs_resample, actual_rate) = select_capture_config(&device)?;
    let channels = supported_config.channels() as usize;
    let format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();

    let err_fn = |err| eprintln!("STT microphone error: {err}");

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| {
                let processed = prepare_audio_chunk(data, channels, needs_resample, actual_rate);
                let _ = audio_tx.try_send(processed);
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                let float: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                let processed = prepare_audio_chunk(&float, channels, needs_resample, actual_rate);
                let _ = audio_tx.try_send(processed);
            },
            err_fn,
            None,
        ),
        SampleFormat::U8 => device.build_input_stream(
            &stream_config,
            move |data: &[u8], _| {
                let float: Vec<f32> =
                    data.iter().map(|&s| (s as f32 - 128.0) / 128.0).collect();
                let processed = prepare_audio_chunk(&float, channels, needs_resample, actual_rate);
                let _ = audio_tx.try_send(processed);
            },
            err_fn,
            None,
        ),
        other => return Err(format!("unsupported microphone sample format: {other:?}")),
    }
    .map_err(|e| e.to_string())?;

    Ok(stream)
}

fn update_mic_level(samples: &[f32]) {
    if samples.is_empty() {
        return;
    }
    let rms = chunk_rms(samples);
    let prev = f32::from_bits(STT_MIC_LEVEL_BITS.load(Ordering::Relaxed));
    // Fast attack, slow decay so the bar doesn't flicker
    let smoothed = if rms > prev { rms } else { prev * 0.85 + rms * 0.15 };
    STT_MIC_LEVEL_BITS.store(smoothed.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

fn chunk_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn select_capture_config(
    device: &cpal::Device,
) -> Result<(SupportedStreamConfig, bool, u32), String> {
    let mut ranges: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_input_configs()
        .map_err(|e| e.to_string())?
        .collect();

    ranges.sort_by_key(|r| match r.sample_format() {
        SampleFormat::F32 => 0u8,
        SampleFormat::I16 => 1,
        _ => 2,
    });

    let range = ranges
        .first()
        .ok_or_else(|| "no supported microphone configurations".to_string())?;

    let min_rate = range.min_sample_rate().0;
    let max_rate = range.max_sample_rate().0;

    if min_rate <= WHISPER_SAMPLE_RATE && WHISPER_SAMPLE_RATE <= max_rate {
        let config = range.clone().with_sample_rate(cpal::SampleRate(WHISPER_SAMPLE_RATE));
        Ok((config, false, WHISPER_SAMPLE_RATE))
    } else {
        let native_rate = 48_000u32.clamp(min_rate, max_rate);
        let config = range.clone().with_sample_rate(cpal::SampleRate(native_rate));
        Ok((config, true, native_rate))
    }
}

fn prepare_audio_chunk(
    data: &[f32],
    channels: usize,
    needs_resample: bool,
    actual_rate: u32,
) -> Vec<f32> {
    let mono = if channels > 1 {
        data.chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect::<Vec<f32>>()
    } else {
        data.to_vec()
    };

    if needs_resample {
        linear_resample(&mono, actual_rate, WHISPER_SAMPLE_RATE)
    } else {
        mono
    }
}

fn linear_resample(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let s0 = samples.get(idx).copied().unwrap_or(0.0);
        let s1 = samples.get(idx + 1).copied().unwrap_or(s0);
        output.push(s0 + frac * (s1 - s0));
    }

    output
}

fn load_whisper_context(model_path: &Path) -> Result<WhisperContext, String> {
    let path_str = model_path
        .to_str()
        .ok_or_else(|| "model path contains non-UTF-8 characters".to_string())?;

    let mut params = WhisperContextParameters::default();
    params.use_gpu(true);

    WhisperContext::new_with_params(path_str, params)
        .map_err(|e| format!("failed to load whisper model: {e}"))
}


fn transcribe_text(state: &mut WhisperState, samples: &[f32]) -> String {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    params.set_language(Some("auto"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_no_context(true);

    if state.full(params, samples).is_err() {
        return String::new();
    }

    let mut text = String::new();
    for segment in state.as_iter() {
        if let Ok(seg) = segment.to_str() {
            let seg = seg.trim();
            if !seg.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(seg);
            }
        }
    }
    text
}

fn is_hallucination(text: &str) -> bool {
    const KNOWN_HALLUCINATIONS: &[&str] = &[
        "Thank you for watching.",
        "Thanks for watching.",
        "Thank you.",
        "Thanks.",
        "Bye.",
        "Bye!",
        "...",
        ".",
        "Subtitles by",
        "Translation by",
        "Subscribe",
        "[BLANK_AUDIO]",
        "(silence)",
    ];

    let t = text.trim();
    KNOWN_HALLUCINATIONS.iter().any(|h| h.eq_ignore_ascii_case(t))
}

fn download_model(model_path: &Path, hf_repo: &str) -> anyhow::Result<()> {
    if let Some(parent) = model_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let filename = model_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ggml-large-v3-turbo.bin");

    let url = format!("https://huggingface.co/{hf_repo}/resolve/main/{filename}");
    eprintln!("STT: downloading {filename} from Hugging Face ({url}) ...");

    let mut response = reqwest::blocking::get(&url)?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {} downloading STT model", response.status());
    }

    let tmp_path = model_path.with_extension("bin.part");
    {
        let mut file = std::fs::File::create(&tmp_path)?;
        io::copy(&mut response, &mut file)?;
    }
    std::fs::rename(&tmp_path, model_path)?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn lower_stt_worker_priority() {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 5);
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_stt_worker_priority() {}
