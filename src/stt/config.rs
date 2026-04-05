use std::{
    env,
    path::{Path, PathBuf},
};

const DEFAULT_STT_MODEL_FILENAME: &str = "ggml-large-v3-turbo-q8_0.bin";
const DEFAULT_STT_MODEL_HF_REPO: &str = "ggerganov/whisper.cpp";
const DEFAULT_STT_ENERGY_THRESHOLD: f32 = 0.01;
const DEFAULT_STT_SILENCE_MS: u64 = 450;
const DEFAULT_STT_MIN_SPEECH_MS: u64 = 150;

#[derive(Clone, Debug)]
pub struct SttRuntimeConfig {
    pub enabled: bool,
    pub model_path: PathBuf,
    pub model_hf_repo: String,
    pub energy_threshold: f32,
    pub silence_ms: u64,
    #[allow(dead_code)]
    pub min_speech_ms: u64,
}

pub fn discover_stt_runtime_config(assets_root: &Path) -> SttRuntimeConfig {
    let enabled = !env_flag("AMADEUS_STT_DISABLED");
    let model_path = env::var("AMADEUS_STT_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            assets_root
                .join("models")
                .join("stt")
                .join(DEFAULT_STT_MODEL_FILENAME)
        });

    SttRuntimeConfig {
        enabled,
        model_path,
        model_hf_repo: DEFAULT_STT_MODEL_HF_REPO.to_string(),
        energy_threshold: DEFAULT_STT_ENERGY_THRESHOLD,
        silence_ms: DEFAULT_STT_SILENCE_MS,
        min_speech_ms: DEFAULT_STT_MIN_SPEECH_MS,
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
