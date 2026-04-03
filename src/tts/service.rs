use std::{
    collections::HashMap,
    f32::consts::PI,
    path::{Path, PathBuf},
    sync::mpsc,
};

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use qwen3_tts::{
    AudioBuffer, CODEC_EOS_TOKEN_ID, CodePredictor, CodePredictorConfig, ModelPaths,
    ParsedModelConfig, SynthesisOptions, TalkerConfig, TalkerModel, codec_tokens,
    compute_dtype_for_device,
    generation::{self, GenerationConfig, SamplingContext},
    models::{self, codec::Decoder12Hz},
    parse_device, special_tokens,
    tokenizer::TextTokenizer,
    tts_tokens,
};

use crate::{
    core::error::{AppError, AppResult},
    tts::{
        config::TtsRuntimeConfig,
        japanese::preload_japanese_tts_support,
        routing::{TtsRequest, ValidatedTtsRequest, ValidatedTtsSpan, validate_request},
    },
};

const SAMPLE_RATE: u32 = 24_000;
const FRAMES_PER_TEXT_TOKEN: usize = 6;
const MIN_GENERATED_FRAMES: usize = 48;
const MAX_GENERATED_FRAMES: usize = 360;
const DECODE_CHUNK_FRAMES: usize = 10;
const STREAM_FIRST_CHUNK_EMIT_FRAMES: usize = 5;
const STREAM_FIRST_CHUNK_TOTAL_FRAMES: usize = 48;
const STREAM_FIRST_CHUNK_DECODE_WINDOW_FRAMES: usize = 48;
const STREAM_STABLE_DECODE_WINDOW_FRAMES: usize = 80;
const STREAM_OVERLAP_SAMPLES: usize = 512;
const SAMPLES_PER_CODEC_FRAME: usize = 1_920;
const CPU_TTS_MEMORY_HINT_GIB: usize = 9;

pub struct TtsService {
    config: TtsRuntimeConfig,
    worker: mpsc::Sender<TtsCommand>,
}

pub enum TtsStreamEvent {
    Audio(AudioBuffer),
    Finished,
    Error(AppError),
}

impl TtsService {
    pub fn new(config: TtsRuntimeConfig) -> AppResult<Self> {
        let (worker, receiver) = mpsc::channel();
        let worker_config = config.clone();

        std::thread::Builder::new()
            .name("amadeus-tts".to_string())
            .spawn(move || run_tts_worker(worker_config, receiver))?;

        Ok(Self { config, worker })
    }

    pub fn preload(&self) -> AppResult<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let (respond_to, response) = mpsc::channel();
        self.worker
            .send(TtsCommand::Preload { respond_to })
            .map_err(|_| tts_worker_unavailable("preload request could not be queued"))?;

        response
            .recv()
            .map_err(|_| tts_worker_unavailable("preload response was not received"))?
    }

    pub fn synthesize(&self, request: TtsRequest) -> AppResult<Vec<u8>> {
        if !self.config.enabled {
            return Err(AppError::TtsDisabled);
        }

        let (respond_to, response) = mpsc::channel();
        self.worker
            .send(TtsCommand::Synthesize {
                request,
                respond_to,
            })
            .map_err(|_| tts_worker_unavailable("synthesis request could not be queued"))?;

        response
            .recv()
            .map_err(|_| tts_worker_unavailable("synthesis response was not received"))?
    }

    pub fn synthesize_streaming(
        &self,
        request: TtsRequest,
    ) -> AppResult<mpsc::Receiver<TtsStreamEvent>> {
        if !self.config.enabled {
            return Err(AppError::TtsDisabled);
        }

        let (events_to, events_from) = mpsc::channel();
        self.worker
            .send(TtsCommand::SynthesizeStreaming { request, events_to })
            .map_err(|_| {
                tts_worker_unavailable("streaming synthesis request could not be queued")
            })?;

        Ok(events_from)
    }

    pub fn prime(&self, request: TtsRequest) {
        if !self.config.enabled {
            return;
        }

        let _ = self.worker.send(TtsCommand::Prime { request });
    }
}

enum TtsCommand {
    Preload {
        respond_to: mpsc::Sender<AppResult<()>>,
    },
    Synthesize {
        request: TtsRequest,
        respond_to: mpsc::Sender<AppResult<Vec<u8>>>,
    },
    SynthesizeStreaming {
        request: TtsRequest,
        events_to: mpsc::Sender<TtsStreamEvent>,
    },
    Prime {
        request: TtsRequest,
    },
}

fn run_tts_worker(config: TtsRuntimeConfig, receiver: mpsc::Receiver<TtsCommand>) {
    lower_tts_worker_priority();

    let mut runtime = None;

    while let Ok(command) = receiver.recv() {
        match command {
            TtsCommand::Preload { respond_to } => {
                let result = ensure_runtime_loaded(&config, &mut runtime).map(|_| {
                    preload_japanese_tts_support();
                });
                let _ = respond_to.send(result);
            }
            TtsCommand::Synthesize {
                request,
                respond_to,
            } => {
                let result = validate_request(request).and_then(|request| {
                    ensure_runtime_loaded(&config, &mut runtime)?.synthesize(&request)
                });
                let _ = respond_to.send(result);
            }
            TtsCommand::SynthesizeStreaming { request, events_to } => {
                let result = validate_request(request).and_then(|request| {
                    ensure_runtime_loaded(&config, &mut runtime)?
                        .synthesize_streaming(&request, |chunk| {
                            events_to.send(TtsStreamEvent::Audio(chunk)).is_ok()
                        })
                });

                match result {
                    Ok(()) => {
                        let _ = events_to.send(TtsStreamEvent::Finished);
                    }
                    Err(error) => {
                        let _ = events_to.send(TtsStreamEvent::Error(error));
                    }
                }
            }
            TtsCommand::Prime { request } => {
                let _ = validate_request(request);
            }
        }
    }
}

fn ensure_runtime_loaded<'a>(
    config: &TtsRuntimeConfig,
    runtime: &'a mut Option<ChristinaTtsRuntime>,
) -> AppResult<&'a mut ChristinaTtsRuntime> {
    if runtime.is_none() {
        *runtime = Some(ChristinaTtsRuntime::load(config)?);
    }

    runtime
        .as_mut()
        .ok_or_else(|| tts_worker_unavailable("runtime was not initialized"))
}

fn tts_worker_unavailable(reason: &str) -> AppError {
    AppError::TtsRuntimeUnavailable {
        reason: format!("the TTS worker is unavailable: {reason}"),
    }
}

#[cfg(target_os = "linux")]
fn lower_tts_worker_priority() {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_tts_worker_priority() {}

struct ChristinaTtsRuntime {
    device: Device,
    tokenizer: TextTokenizer,
    talker: TalkerModel,
    code_predictor: CodePredictor,
    decoder: Decoder12Hz,
    options: SynthesisOptions,
}

impl ChristinaTtsRuntime {
    fn load(config: &TtsRuntimeConfig) -> AppResult<Self> {
        validate_requested_backend(config)?;
        let device = parse_device(&config.device)
            .map_err(|error| runtime_error("failed to initialize the requested device", error))?;
        ensure_supported_device(config, &device)?;
        let assets = resolve_model_assets(config)?;
        let tokenizer = load_tokenizer(&assets, config)?;
        let parsed_config = ParsedModelConfig::from_file(&assets.config_path)
            .map_err(|error| runtime_error("failed to parse the model config", error))?;

        let model_weights = load_weights(&assets.model_weights, &device)
            .map_err(|error| runtime_error("failed to load talker weights", error))?;
        let decoder_weights = load_f32_weights(&assets.decoder_weights, &device)
            .map_err(|error| runtime_error("failed to load decoder weights", error))?;

        let compute_dtype = compute_dtype_for_device(&device);
        let talker = TalkerModel::from_weights_with_config_dtype(
            &model_weights,
            TalkerConfig::from_parsed(&parsed_config),
            &device,
            compute_dtype,
        )
        .map_err(|error| runtime_error("failed to construct the talker model", error))?;

        let vb = VarBuilder::from_tensors(model_weights.clone(), compute_dtype, &device);
        let code_predictor = CodePredictor::new(
            CodePredictorConfig::from_parsed(&parsed_config),
            vb.pp("talker").pp("code_predictor"),
        )
        .map_err(|error| runtime_error("failed to construct the code predictor", error))?;

        let decoder = Decoder12Hz::from_weights(&decoder_weights, Default::default())
            .map_err(|error| runtime_error("failed to construct the audio decoder", error))?;

        Ok(Self {
            device,
            tokenizer,
            talker,
            code_predictor,
            decoder,
            options: SynthesisOptions::default(),
        })
    }

    fn synthesize(&mut self, request: &ValidatedTtsRequest) -> AppResult<Vec<u8>> {
        let audio = self.synthesize_audio(request)?;
        Ok(encode_wav(&audio))
    }

    fn synthesize_audio(&mut self, request: &ValidatedTtsRequest) -> AppResult<AudioBuffer> {
        let mut samples = Vec::new();
        self.synthesize_streaming(request, |chunk| {
            samples.extend(chunk.samples);
            true
        })?;
        Ok(AudioBuffer::new(samples, SAMPLE_RATE))
    }

    fn synthesize_streaming<F>(
        &mut self,
        request: &ValidatedTtsRequest,
        mut on_chunk: F,
    ) -> AppResult<()>
    where
        F: FnMut(AudioBuffer) -> bool,
    {
        for span in &request.spans {
            self.synthesize_span_streaming(span, &mut on_chunk)?;
        }

        Ok(())
    }

    fn synthesize_span_streaming<F>(
        &mut self,
        request: &ValidatedTtsSpan,
        on_chunk: &mut F,
    ) -> AppResult<()>
    where
        F: FnMut(AudioBuffer) -> bool,
    {
        let input_ids = self
            .tokenizer
            .encode(&request.text)
            .map_err(|error| synthesis_error("failed to tokenize the input text", error))?;
        let request_options = self.request_options(input_ids.len());
        let gen_config = request_options.to_gen_config();
        let mut sampling_ctx = SamplingContext::new(request_options.seed);

        let (trailing_text_hidden, trailing_text_len, tts_pad_embed) =
            build_trailing_text(&self.talker, &input_ids).map_err(|error| {
                synthesis_error("failed to prepare the text conditioning", error)
            })?;

        let mut kv_caches = self.talker.new_kv_caches(gen_config.max_new_tokens + 256);
        let (hidden, logits) = prefill_custom_voice(
            &self.talker,
            &input_ids,
            request.speaker.token_id(),
            request.language.token_id(),
            &mut kv_caches,
        )
        .map_err(|error| synthesis_error("failed during the talker prefill stage", error))?;

        let prefill_len = hidden
            .dim(1)
            .map_err(|error| synthesis_error("failed to inspect the prefill output", error))?;
        let offset = prefill_len;
        let last_hidden = hidden
            .i((.., prefill_len - 1..prefill_len, ..))
            .map_err(|error| synthesis_error("failed to extract the last hidden state", error))?;

        self.generate_audio_chunks(
            &gen_config,
            &mut sampling_ctx,
            &mut kv_caches,
            offset,
            last_hidden,
            &logits,
            &trailing_text_hidden,
            trailing_text_len,
            &tts_pad_embed,
            request_options.chunk_frames,
            on_chunk,
        )
    }

    fn request_options(&self, text_token_count: usize) -> SynthesisOptions {
        let mut options = self.options.clone();
        options.max_length = estimate_max_frames(text_token_count);
        options.chunk_frames = DECODE_CHUNK_FRAMES;
        options
    }

    fn generate_audio_chunks<F>(
        &self,
        gen_config: &GenerationConfig,
        sampling_ctx: &mut SamplingContext,
        kv_caches: &mut [models::AnyKVCache],
        mut offset: usize,
        mut last_hidden: Tensor,
        initial_logits: &Tensor,
        trailing_text_hidden: &Tensor,
        trailing_text_len: usize,
        tts_pad_embed: &Tensor,
        chunk_frames: usize,
        on_chunk: &mut F,
    ) -> AppResult<()>
    where
        F: FnMut(AudioBuffer) -> bool,
    {
        let suppression_mask = generation::build_suppression_mask(
            codec_tokens::CODEC_VOCAB_SIZE,
            CODEC_EOS_TOKEN_ID,
            &self.device,
        )
        .map_err(|error| synthesis_error("failed to build the token suppression mask", error))?;

        let mut generated_tokens = Vec::new();
        let mut token_count = 0usize;
        let logits_2d = initial_logits
            .squeeze(1)
            .map_err(|error| synthesis_error("failed to prepare the first token logits", error))?;
        let logits_2d = apply_generation_penalties(
            &logits_2d,
            &generated_tokens,
            gen_config,
            token_count,
            Some(&suppression_mask),
        )?;

        let mut semantic_token_tensor = generation::sample(&logits_2d, gen_config, sampling_ctx)
            .map_err(|error| synthesis_error("failed to sample the first semantic token", error))?;
        let mut semantic_token = semantic_token_tensor
            .flatten_all()
            .map_err(|error| synthesis_error("failed to flatten the first sampled token", error))?
            .to_vec1::<u32>()
            .map_err(|error| synthesis_error("failed to read the first sampled token", error))?[0];
        generated_tokens.push(semantic_token);
        token_count += 1;

        let mut emitted_audio = false;
        let stable_emit_frames = chunk_frames.max(1);
        let mut code_buffer = Vec::with_capacity(gen_config.max_new_tokens);
        let mut frames_since_emit = 0usize;
        let mut total_frames_emitted = 0usize;
        let mut decoded_tail = None;
        let mut cp_kv_caches = self.code_predictor.new_kv_caches();

        for frame_idx in 0..gen_config.max_new_tokens {
            if is_eos_token(semantic_token, gen_config) {
                break;
            }

            let semantic_embed = self
                .talker
                .get_codec_embedding_from_tensor(&semantic_token_tensor)
                .map_err(|error| {
                    synthesis_error("failed to look up the semantic embedding", error)
                })?;
            let acoustic_tensor = self
                .code_predictor
                .generate_acoustic_codes(&last_hidden, &semantic_embed, &mut cp_kv_caches)
                .map_err(|error| synthesis_error("failed to generate acoustic codes", error))?;
            let acoustic_codes = acoustic_tensor
                .flatten_all()
                .map_err(|error| synthesis_error("failed to flatten the acoustic codes", error))?
                .to_vec1::<u32>()
                .map_err(|error| synthesis_error("failed to read the acoustic codes", error))?;

            let mut frame = Vec::with_capacity(16);
            frame.push(semantic_token);
            frame.extend_from_slice(&acoustic_codes);
            code_buffer.push(frame);
            frames_since_emit += 1;

            let (emit_every_frames, decode_window_frames) =
                streaming_phase(code_buffer.len(), stable_emit_frames);
            if frames_since_emit >= emit_every_frames {
                frames_since_emit = 0;
                if let Some(audio_chunk) = self.decode_streaming_step(
                    &code_buffer,
                    emit_every_frames,
                    decode_window_frames,
                    &mut decoded_tail,
                )? {
                    total_frames_emitted = code_buffer.len();
                    emitted_audio = true;
                    if !on_chunk(audio_chunk) {
                        return Ok(());
                    }
                }
            }

            let acoustic_embed_sum = self
                .code_predictor
                .get_acoustic_embeddings_sum(&acoustic_codes, &self.device)
                .map_err(|error| synthesis_error("failed to build acoustic embeddings", error))?;
            let summed = semantic_embed.add(&acoustic_embed_sum).map_err(|error| {
                synthesis_error("failed to fuse semantic and acoustic embeddings", error)
            })?;
            let text_addition = if frame_idx < trailing_text_len {
                trailing_text_hidden.i((.., frame_idx..frame_idx + 1, ..))
            } else {
                Ok(tts_pad_embed.clone())
            }
            .map_err(|error| {
                synthesis_error("failed to fetch trailing text conditioning", error)
            })?;
            let step_input = summed.add(&text_addition).map_err(|error| {
                synthesis_error("failed to prepare the next talker step", error)
            })?;

            let (hidden, new_logits) = self
                .talker
                .generate_step_with_embed(&step_input, kv_caches, offset)
                .map_err(|error| {
                    synthesis_error("failed during the talker generation step", error)
                })?;
            offset += 1;
            last_hidden = hidden;

            let logits_2d = new_logits
                .squeeze(1)
                .map_err(|error| synthesis_error("failed to prepare logits for sampling", error))?;
            let logits_2d = apply_generation_penalties(
                &logits_2d,
                &generated_tokens,
                gen_config,
                token_count,
                Some(&suppression_mask),
            )?;

            semantic_token_tensor = generation::sample(&logits_2d, gen_config, sampling_ctx)
                .map_err(|error| {
                    synthesis_error("failed to sample the next semantic token", error)
                })?;
            semantic_token = semantic_token_tensor
                .flatten_all()
                .map_err(|error| synthesis_error("failed to flatten the sampled token", error))?
                .to_vec1::<u32>()
                .map_err(|error| synthesis_error("failed to read the sampled token", error))?[0];
            generated_tokens.push(semantic_token);
            token_count += 1;
        }

        if let Some(audio_chunk) = self.decode_streaming_flush(
            &code_buffer,
            total_frames_emitted,
            STREAM_STABLE_DECODE_WINDOW_FRAMES,
            &mut decoded_tail,
        )? {
            emitted_audio = true;
            let _ = on_chunk(audio_chunk);
        }

        if !emitted_audio {
            return Err(AppError::TtsSynthesisFailed {
                reason: "the model did not generate any audio frames".to_string(),
            });
        }

        Ok(())
    }

    fn decode_audio_chunk(&self, frame_codes: &[Vec<u32>]) -> AppResult<AudioBuffer> {
        if frame_codes.is_empty() {
            return Err(AppError::TtsSynthesisFailed {
                reason: "the model did not generate any audio frames".to_string(),
            });
        }

        let codes_tensor = qwen3_tts::codes_to_tensor(frame_codes, &self.device)
            .map_err(|error| synthesis_error("failed to pack the generated codec frames", error))?;
        let waveform = self
            .decoder
            .decode(&codes_tensor)
            .map_err(|error| synthesis_error("failed to decode codec frames into audio", error))?;
        AudioBuffer::from_tensor(waveform, SAMPLE_RATE)
            .map_err(|error| synthesis_error("failed to convert the decoded waveform", error))
    }

    fn decode_streaming_step(
        &self,
        frame_codes: &[Vec<u32>],
        emit_every_frames: usize,
        decode_window_frames: usize,
        decoded_tail: &mut Option<Vec<f32>>,
    ) -> AppResult<Option<AudioBuffer>> {
        if frame_codes.is_empty() {
            return Ok(None);
        }

        let window_len = decode_window_frames.max(emit_every_frames).max(1);
        let start = frame_codes.len().saturating_sub(window_len);
        let decoded = self.decode_audio_chunk(&frame_codes[start..])?;
        let samples_per_frame =
            samples_per_generated_frame(decoded.samples.len(), frame_codes.len() - start);
        let step_samples = emit_every_frames.saturating_mul(samples_per_frame);
        let mut chunk = decoded.samples;

        if step_samples > 0 && chunk.len() > step_samples {
            chunk = chunk[chunk.len() - step_samples..].to_vec();
        }

        let chunk = post_process_stream_chunk(chunk, decoded_tail, false);
        if chunk.is_empty() {
            return Ok(None);
        }

        Ok(Some(AudioBuffer::new(chunk, SAMPLE_RATE)))
    }

    fn decode_streaming_flush(
        &self,
        frame_codes: &[Vec<u32>],
        total_frames_emitted: usize,
        decode_window_frames: usize,
        decoded_tail: &mut Option<Vec<f32>>,
    ) -> AppResult<Option<AudioBuffer>> {
        let remaining_frames = frame_codes.len().saturating_sub(total_frames_emitted);
        if remaining_frames == 0 {
            return Ok(None);
        }

        let context_frames =
            total_frames_emitted.min(decode_window_frames.saturating_sub(remaining_frames));
        let start = total_frames_emitted.saturating_sub(context_frames);
        let decoded = self.decode_audio_chunk(&frame_codes[start..])?;
        let samples_per_frame =
            samples_per_generated_frame(decoded.samples.len(), frame_codes.len() - start);
        let skip_samples = context_frames.saturating_mul(samples_per_frame);
        let mut chunk = decoded.samples;

        if skip_samples > 0 {
            if skip_samples >= chunk.len() {
                chunk.clear();
            } else {
                chunk = chunk[skip_samples..].to_vec();
            }
        }

        let chunk = post_process_stream_chunk(chunk, decoded_tail, true);
        if chunk.is_empty() {
            return Ok(None);
        }

        Ok(Some(AudioBuffer::new(chunk, SAMPLE_RATE)))
    }
}

fn streaming_phase(total_frames_generated: usize, stable_emit_frames: usize) -> (usize, usize) {
    if total_frames_generated < STREAM_FIRST_CHUNK_TOTAL_FRAMES {
        (
            STREAM_FIRST_CHUNK_EMIT_FRAMES.min(stable_emit_frames),
            STREAM_FIRST_CHUNK_DECODE_WINDOW_FRAMES,
        )
    } else {
        (stable_emit_frames, STREAM_STABLE_DECODE_WINDOW_FRAMES)
    }
}

fn samples_per_generated_frame(sample_count: usize, frame_count: usize) -> usize {
    if frame_count == 0 {
        return SAMPLES_PER_CODEC_FRAME;
    }

    (sample_count / frame_count).max(1)
}

fn post_process_stream_chunk(
    mut chunk: Vec<f32>,
    decoded_tail: &mut Option<Vec<f32>>,
    final_chunk: bool,
) -> Vec<f32> {
    if chunk.is_empty() {
        return chunk;
    }

    if let Some(previous_tail) = decoded_tail.as_ref() {
        apply_hann_crossfade(previous_tail, &mut chunk, STREAM_OVERLAP_SAMPLES);
    } else {
        apply_hann_fade_in(&mut chunk, STREAM_OVERLAP_SAMPLES);
    }

    let full_chunk = chunk.clone();
    if final_chunk {
        apply_hann_fade_out(&mut chunk, STREAM_OVERLAP_SAMPLES);
    } else if STREAM_OVERLAP_SAMPLES > 0 && chunk.len() > STREAM_OVERLAP_SAMPLES * 2 {
        chunk.truncate(chunk.len() - STREAM_OVERLAP_SAMPLES);
    }

    *decoded_tail = Some(full_chunk);
    chunk
}

fn apply_hann_crossfade(previous_tail: &[f32], current_chunk: &mut [f32], overlap_samples: usize) {
    let overlap = overlap_samples
        .min(previous_tail.len())
        .min(current_chunk.len());
    if overlap == 0 {
        return;
    }

    let previous_slice = &previous_tail[previous_tail.len() - overlap..];
    let denom = (overlap.saturating_sub(1)).max(1) as f32;

    for (index, current_sample) in current_chunk.iter_mut().take(overlap).enumerate() {
        let t = index as f32 / denom;
        let fade_out = 0.5 * (1.0 + (PI * t).cos());
        let fade_in = 0.5 * (1.0 - (PI * t).cos());
        *current_sample = previous_slice[index] * fade_out + *current_sample * fade_in;
    }
}

fn apply_hann_fade_in(samples: &mut [f32], overlap_samples: usize) {
    let fade_len = overlap_samples.min(samples.len());
    if fade_len == 0 {
        return;
    }

    let denom = (fade_len.saturating_sub(1)).max(1) as f32;
    for (index, sample) in samples.iter_mut().take(fade_len).enumerate() {
        let t = index as f32 / denom;
        let fade_in = 0.5 * (1.0 - (PI * t).cos());
        *sample *= fade_in;
    }
}

fn apply_hann_fade_out(samples: &mut [f32], overlap_samples: usize) {
    let fade_len = overlap_samples.min(samples.len());
    if fade_len == 0 {
        return;
    }

    let start = samples.len() - fade_len;
    let denom = (fade_len.saturating_sub(1)).max(1) as f32;
    for (index, sample) in samples.iter_mut().skip(start).enumerate() {
        let t = index as f32 / denom;
        let fade_out = 0.5 * (1.0 + (PI * t).cos());
        *sample *= fade_out;
    }
}

fn is_eos_token(token: u32, gen_config: &GenerationConfig) -> bool {
    gen_config.eos_token_id == Some(token) || token == CODEC_EOS_TOKEN_ID
}

fn validate_requested_backend(config: &TtsRuntimeConfig) -> AppResult<()> {
    let requested = config.device.trim().to_ascii_lowercase();
    if requested.starts_with("cuda") && !cfg!(feature = "cuda") {
        return Err(AppError::TtsRuntimeUnavailable {
            reason: "CUDA was requested, but this binary was not built with CUDA support; rebuild with cargo run --features cuda".to_string(),
        });
    }

    if requested == "metal" && !cfg!(feature = "metal") {
        return Err(AppError::TtsRuntimeUnavailable {
            reason: "Metal was requested, but this binary was not built with Metal support; rebuild with cargo run --features metal".to_string(),
        });
    }

    Ok(())
}

fn ensure_supported_device(config: &TtsRuntimeConfig, device: &Device) -> AppResult<()> {
    if matches!(device, Device::Cpu) && !config.allow_cpu_fallback {
        let selection = if config.device.trim().is_empty()
            || config.device.trim().eq_ignore_ascii_case("auto")
        {
            "automatic device selection fell back to CPU"
        } else {
            "CPU inference was requested"
        };

        let recovery = if cfg!(feature = "cuda") || cfg!(feature = "metal") {
            "choose a GPU device or set AMADEUS_TTS_ALLOW_CPU=1 to override"
        } else if cfg!(target_os = "macos") {
            "rebuild with cargo run --features metal, or set AMADEUS_TTS_ALLOW_CPU=1 to override"
        } else {
            "rebuild with cargo run --features cuda, or set AMADEUS_TTS_ALLOW_CPU=1 to override"
        };

        return Err(AppError::TtsRuntimeUnavailable {
            reason: format!(
                "{selection}, but CPU fallback is disabled because qwen3-tts can use about {CPU_TTS_MEMORY_HINT_GIB} GB of RAM on CPU and may be OOM-killed; {recovery}"
            ),
        });
    }

    Ok(())
}

#[derive(Debug)]
struct ResolvedModelAssets {
    model_weights: PathBuf,
    decoder_weights: PathBuf,
    config_path: PathBuf,
    tokenizer_path: Option<PathBuf>,
}

fn resolve_model_assets(config: &TtsRuntimeConfig) -> AppResult<ResolvedModelAssets> {
    let source_path = PathBuf::from(&config.model_source);
    if source_path.exists() {
        let model_root = resolve_local_model_root(&source_path)?;
        let model_weights = if source_path.is_file() {
            source_path.clone()
        } else {
            model_root.join("model.safetensors")
        };
        let decoder_weights = resolve_local_decoder_weights(&model_root)?;
        let tokenizer_path = find_local_tokenizer(&model_root);

        return Ok(ResolvedModelAssets {
            model_weights,
            decoder_weights,
            config_path: model_root.join("config.json"),
            tokenizer_path,
        });
    }

    let model_paths = ModelPaths::download(Some(&config.model_source))
        .map_err(|error| runtime_error("failed to download the Hugging Face model", error))?;

    Ok(ResolvedModelAssets {
        model_weights: model_paths.model_weights,
        decoder_weights: model_paths.decoder_weights,
        config_path: model_paths.config,
        tokenizer_path: Some(model_paths.tokenizer),
    })
}

fn resolve_local_model_root(source_path: &Path) -> AppResult<PathBuf> {
    if source_path.is_dir() {
        return Ok(source_path.to_path_buf());
    }

    source_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::TtsRuntimeUnavailable {
            reason: format!(
                "could not determine the model directory from {}",
                source_path.display()
            ),
        })
}

fn resolve_local_decoder_weights(model_root: &Path) -> AppResult<PathBuf> {
    let direct = model_root
        .join("speech_tokenizer")
        .join("model.safetensors");
    if direct.exists() {
        return Ok(direct);
    }

    if let Some(parent) = model_root.parent() {
        let sibling = parent.join("speech_tokenizer").join("model.safetensors");
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    Err(AppError::TtsRuntimeUnavailable {
        reason: format!(
            "could not find speech_tokenizer/model.safetensors next to {}",
            model_root.display()
        ),
    })
}

fn find_local_tokenizer(model_root: &Path) -> Option<PathBuf> {
    let tokenizer_json = model_root.join("tokenizer.json");
    if tokenizer_json.exists() {
        return Some(tokenizer_json);
    }

    let vocab_json = model_root.join("vocab.json");
    let merges_txt = model_root.join("merges.txt");
    if vocab_json.exists() && merges_txt.exists() {
        return Some(model_root.to_path_buf());
    }

    None
}

fn load_tokenizer(
    assets: &ResolvedModelAssets,
    config: &TtsRuntimeConfig,
) -> AppResult<TextTokenizer> {
    if let Some(path) = &assets.tokenizer_path {
        return if path.is_file() {
            TextTokenizer::from_file(path)
                .map_err(|error| runtime_error("failed to load the tokenizer", error))
        } else {
            let path_string = path.to_string_lossy().to_string();
            TextTokenizer::from_pretrained(&path_string)
                .map_err(|error| runtime_error("failed to load the local tokenizer", error))
        };
    }

    TextTokenizer::from_pretrained(&config.tokenizer_source).map_err(|error| {
        runtime_error(
            format!(
                "failed to load a tokenizer; tried the model checkout and then {}",
                config.tokenizer_source
            ),
            error,
        )
    })
}

fn load_weights(
    path: &Path,
    device: &Device,
) -> Result<HashMap<String, Tensor>, candle_core::Error> {
    candle_core::safetensors::load(path, device)
}

fn load_f32_weights(
    path: &Path,
    device: &Device,
) -> Result<HashMap<String, Tensor>, candle_core::Error> {
    let weights = candle_core::safetensors::load(path, device)?;
    weights
        .into_iter()
        .map(|(name, tensor)| {
            if tensor.dtype() == DType::BF16 {
                tensor
                    .to_dtype(DType::F32)
                    .map(|converted| (name, converted))
            } else {
                Ok((name, tensor))
            }
        })
        .collect()
}

fn build_trailing_text(
    talker: &TalkerModel,
    input_ids: &[u32],
) -> anyhow::Result<(Tensor, usize, Tensor)> {
    let trailing_text_hidden = if input_ids.len() > 1 {
        let remaining_proj = talker.get_projected_text_embeddings(&input_ids[1..])?;
        let tts_eos_embed = talker.get_tts_eos_embed()?;
        Tensor::cat(&[&remaining_proj, &tts_eos_embed], 1)?
    } else {
        talker.get_tts_eos_embed()?
    };
    let trailing_text_len = trailing_text_hidden.dim(1)?;
    let tts_pad_embed = talker.get_tts_pad_embed()?;

    Ok((trailing_text_hidden, trailing_text_len, tts_pad_embed))
}

fn prefill_custom_voice(
    talker: &TalkerModel,
    text_tokens: &[u32],
    speaker_token_id: u32,
    language_token_id: u32,
    kv_caches: &mut [models::AnyKVCache],
) -> anyhow::Result<(Tensor, Tensor)> {
    let role_prefix_hidden = talker.get_projected_text_embeddings(&[
        special_tokens::IM_START,
        special_tokens::ASSISTANT,
        special_tokens::NEWLINE,
    ])?;

    let codec_ids = Tensor::new(
        &[
            codec_tokens::CODEC_THINK,
            codec_tokens::CODEC_THINK_BOS,
            language_token_id,
            codec_tokens::CODEC_THINK_EOS,
            speaker_token_id,
            codec_tokens::CODEC_PAD,
            codec_tokens::CODEC_BOS,
        ],
        role_prefix_hidden.device(),
    )?;
    let codec_embed = talker.get_codec_embedding_batch(&codec_ids)?;

    let text_special =
        talker.get_projected_text_embeddings(&[tts_tokens::TTS_PAD, tts_tokens::TTS_BOS])?;
    let hidden_size = talker.config().hidden_size;
    let tts_pad = text_special.i((.., 0..1, ..))?;
    let tts_bos = text_special.i((.., 1..2, ..))?;
    let tts_pad_expanded = tts_pad.broadcast_as((1, 5, hidden_size))?;
    let tts_text_embed = Tensor::cat(&[&tts_pad_expanded, &tts_bos], 1)?;

    let codec_first6 = codec_embed.i((.., ..6, ..))?;
    let codec_hidden = tts_text_embed.add(&codec_first6)?;
    let mut hidden = Tensor::cat(&[&role_prefix_hidden, &codec_hidden], 1)?;

    if let Some(&first_text_token) = text_tokens.first() {
        let first_text_proj = talker.get_projected_text_embeddings(&[first_text_token])?;
        let codec_bos_embed = codec_embed.i((.., 6..7, ..))?;
        let combined = first_text_proj.add(&codec_bos_embed)?;
        hidden = Tensor::cat(&[&hidden, &combined], 1)?;
    }

    run_prefill_layers(talker, hidden, kv_caches)
}

fn run_prefill_layers(
    talker: &TalkerModel,
    mut hidden: Tensor,
    kv_caches: &mut [models::AnyKVCache],
) -> anyhow::Result<(Tensor, Tensor)> {
    let seq_len = hidden.dim(1)?;
    let mask = models::transformer::create_causal_mask(seq_len, 0, hidden.device())?;

    for (index, layer) in talker.layers_iter().enumerate() {
        hidden = layer.forward(
            &hidden,
            talker.rope(),
            Some(&mask),
            Some(&mut kv_caches[index]),
            0,
        )?;
    }

    hidden = talker.apply_norm(&hidden)?;
    let last_hidden = hidden.i((.., seq_len - 1..seq_len, ..))?;
    let logits = talker.apply_codec_head(&last_hidden)?;

    Ok((hidden, logits))
}

fn apply_generation_penalties(
    logits: &Tensor,
    generated_tokens: &[u32],
    config: &GenerationConfig,
    token_count: usize,
    suppression_mask: Option<&generation::SuppressionMask>,
) -> AppResult<Tensor> {
    let logits = logits
        .to_dtype(DType::F32)
        .map_err(|error| synthesis_error("failed to cast logits for sampling", error))?;
    let logits = if config.repetition_penalty != 1.0 && !generated_tokens.is_empty() {
        let previous_tokens = Tensor::new(generated_tokens, logits.device()).map_err(|error| {
            synthesis_error("failed to construct the repetition penalty tensor", error)
        })?;
        generation::apply_repetition_penalty(&logits, &previous_tokens, config.repetition_penalty)
            .map_err(|error| synthesis_error("failed to apply the repetition penalty", error))?
    } else {
        logits
    };
    let logits = if let Some(mask) = suppression_mask {
        generation::apply_token_suppression_with_mask(&logits, mask)
            .map_err(|error| synthesis_error("failed to apply token suppression", error))?
    } else {
        generation::apply_token_suppression(
            &logits,
            codec_tokens::CODEC_VOCAB_SIZE,
            CODEC_EOS_TOKEN_ID,
        )
        .map_err(|error| synthesis_error("failed to apply token suppression", error))?
    };

    if token_count < config.min_new_tokens {
        if let Some(eos_token_id) = config.eos_token_id {
            let vocab_size = logits.dim(1).map_err(|error| {
                synthesis_error("failed to read the sampling vocabulary size", error)
            })?;
            let batch = logits.dim(0).map_err(|error| {
                synthesis_error("failed to read the sampling batch size", error)
            })?;
            let mut eos_mask = vec![0.0f32; vocab_size];
            if let Some(slot) = eos_mask.get_mut(eos_token_id as usize) {
                *slot = 1.0;
            }

            let eos_mask = Tensor::new(eos_mask.as_slice(), logits.device())
                .and_then(|mask| mask.unsqueeze(0))
                .and_then(|mask| mask.broadcast_as((batch, vocab_size)))
                .map_err(|error| {
                    synthesis_error("failed to build the EOS suppression mask", error)
                })?;
            let neg_inf = Tensor::new(&[f32::NEG_INFINITY], logits.device())
                .and_then(|value| value.broadcast_as((batch, vocab_size)))
                .map_err(|error| {
                    synthesis_error("failed to build the EOS penalty tensor", error)
                })?;
            let zeros = Tensor::zeros((batch, vocab_size), DType::F32, logits.device()).map_err(
                |error| synthesis_error("failed to build the EOS comparison tensor", error),
            )?;
            let is_eos = eos_mask
                .gt(&zeros)
                .map_err(|error| synthesis_error("failed to compare the EOS mask", error))?;

            return is_eos.where_cond(&neg_inf, &logits).map_err(|error| {
                synthesis_error("failed to suppress EOS for min_new_tokens", error)
            });
        }
    }

    Ok(logits)
}

fn estimate_max_frames(text_token_count: usize) -> usize {
    text_token_count
        .saturating_mul(FRAMES_PER_TEXT_TOKEN)
        .clamp(MIN_GENERATED_FRAMES, MAX_GENERATED_FRAMES)
}

fn encode_wav(audio: &AudioBuffer) -> Vec<u8> {
    let channels = 1u16;
    let bits_per_sample = 16u16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = audio.sample_rate * u32::from(block_align);
    let data_len = (audio.samples.len() * usize::from(bits_per_sample / 8)) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&audio.sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());

    for sample in &audio.samples {
        let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        wav.extend_from_slice(&pcm.to_le_bytes());
    }

    wav
}

fn runtime_error(context: impl Into<String>, error: impl std::fmt::Display) -> AppError {
    AppError::TtsRuntimeUnavailable {
        reason: format!("{}: {error}", context.into()),
    }
}

fn synthesis_error(context: impl Into<String>, error: impl std::fmt::Display) -> AppError {
    AppError::TtsSynthesisFailed {
        reason: format!("{}: {error}", context.into()),
    }
}

#[cfg(test)]
mod smoke_tests {
    use crate::tts::config::discover_tts_runtime_config;

    use super::{TtsRequest, TtsService};

    #[test]
    #[ignore = "requires the local qwen3-tts runtime and model assets"]
    fn local_runtime_synthesizes_basic_english_and_japanese_audio() {
        let service =
            TtsService::new(discover_tts_runtime_config()).expect("TTS service should initialize");

        service.preload().expect("TTS runtime should preload");

        let english = service
            .synthesize(TtsRequest {
                text: "Hello world. This is a voice check.".to_string(),
                speaker: None,
                language: None,
            })
            .expect("English synthesis should succeed");
        assert!(
            english.len() > 44,
            "English synthesis produced an empty WAV"
        );

        let japanese = service
            .synthesize(TtsRequest {
                text: "こんにちは。これは音声の確認です。".to_string(),
                speaker: None,
                language: None,
            })
            .expect("Japanese synthesis should succeed");
        assert!(
            japanese.len() > 44,
            "Japanese synthesis produced an empty WAV"
        );

        for text in [
            "Hello!",
            "**そうです！**",
            "**はい、くりすです！**",
            "**くりす**",
        ] {
            let audio = service
                .synthesize(TtsRequest {
                    text: text.to_string(),
                    speaker: None,
                    language: None,
                })
                .unwrap_or_else(|error| panic!("Short synthesis failed for {text:?}: {error}"));
            assert!(
                audio.len() > 44,
                "Short synthesis produced an empty WAV for {text:?}"
            );
        }
    }
}
