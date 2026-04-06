use std::env;

const DEFAULT_TTS_MODEL: &str = "Loke-60000/christina-TTS";
const DEFAULT_TTS_TOKENIZER: &str = "Qwen/Qwen2-0.5B";
const DEFAULT_TTS_DEVICE: &str = "auto";

#[derive(Clone, Debug)]
pub struct TtsRuntimeConfig {
    pub enabled: bool,
    pub model_source: String,
    pub tokenizer_source: String,
    pub device: String,
    pub allow_cpu_fallback: bool,
}

/// Discover TTS runtime config.  `services_enabled` overrides `AMADEUS_TTS_DISABLED` when set.
pub fn discover_tts_runtime_config(services_enabled: Option<bool>) -> TtsRuntimeConfig {
    let enabled = services_enabled.unwrap_or_else(|| !env_flag("AMADEUS_TTS_DISABLED"));

    TtsRuntimeConfig {
        enabled,
        model_source: env::var("AMADEUS_TTS_MODEL")
            .unwrap_or_else(|_| DEFAULT_TTS_MODEL.to_string()),
        tokenizer_source: env::var("AMADEUS_TTS_TOKENIZER")
            .unwrap_or_else(|_| DEFAULT_TTS_TOKENIZER.to_string()),
        device: env::var("AMADEUS_TTS_DEVICE").unwrap_or_else(|_| DEFAULT_TTS_DEVICE.to_string()),
        allow_cpu_fallback: env_flag("AMADEUS_TTS_ALLOW_CPU"),
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
