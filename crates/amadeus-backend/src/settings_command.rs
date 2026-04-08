use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::config::{LlmProvider, DEFAULT_CONFIG_PATH};

/// Parsed representation of a `/settings` command.
pub enum SettingsCommand {
    Tts(bool),
    Stt(bool),
    /// Toggle local llama.cpp vs external API.
    Llm(LlmTarget),
    /// Change the LLM provider (written to `provider` in config.json).
    Provider(String),
    /// Change the model name (written to `model` in config.json).
    Model(String),
    /// Change the API base URL (written to `apiBase` in config.json).
    ApiBase(String),
    /// Change the max context window in tokens (written to `maxContextTokens`).
    Context(usize),
}

pub enum LlmTarget {
    Local,
    External,
}

impl SettingsCommand {
    /// Parse the arguments that follow the `/settings` token.
    ///
    /// For most keys, `<value>` is the next whitespace token.
    /// For `model` and `api-base`, the value is everything after the key
    /// (allowing spaces in URLs / model names).
    pub fn parse(args: &str) -> Result<Self> {
        let (key, rest) = split_key_rest(args)?;

        match key.to_ascii_lowercase().as_str() {
            "tts" => Ok(Self::Tts(parse_bool_arg(single_token(rest)?)?)),
            "stt" => Ok(Self::Stt(parse_bool_arg(single_token(rest)?)?)),
            "llm" => match single_token(rest)?.to_ascii_lowercase().as_str() {
                "local" => Ok(Self::Llm(LlmTarget::Local)),
                "external" => Ok(Self::Llm(LlmTarget::External)),
                other => bail!("unknown llm target {other:?}; expected local or external"),
            },
            "provider" => {
                let raw = single_token(rest)?.to_string();
                // Validate the provider string up-front so the error is immediate.
                LlmProvider::parse(&raw)?;
                Ok(Self::Provider(raw))
            }
            "model" => {
                let model = rest.trim().to_string();
                if model.is_empty() {
                    bail!("usage: /settings model <model-name>");
                }
                Ok(Self::Model(model))
            }
            "api-base" | "apibase" | "api_base" => {
                let url = rest.trim().to_string();
                if url.is_empty() {
                    bail!("usage: /settings api-base <url>");
                }
                Ok(Self::ApiBase(url))
            }
            "context" => {
                let n: usize = single_token(rest)?
                    .parse()
                    .context("context must be a positive integer (number of tokens)")?;
                if n < 512 {
                    bail!("context must be at least 512 tokens");
                }
                Ok(Self::Context(n))
            }
            other => bail!(
                "unknown setting {other:?}; supported: tts, stt, llm, provider, model, api-base, context"
            ),
        }
    }

    /// Apply this setting to `.amadeus/config.json` and return a feedback string.
    pub fn apply(&self, workspace_root: &Path) -> Result<String> {
        let config_path = resolve_config_path(workspace_root);
        let mut json = load_config_json(&config_path)?;

        let root = json
            .as_object_mut()
            .context("config.json must be a JSON object")?;

        let message = match self {
            // ── root-level fields ─────────────────────────────────────────────
            Self::Provider(raw) => {
                root.insert("provider".into(), Value::String(raw.clone()));
                format!("Provider set to {raw:?}.")
            }
            Self::Model(model) => {
                root.insert("model".into(), Value::String(model.clone()));
                format!("Model set to {model:?}.")
            }
            Self::ApiBase(url) => {
                root.insert("apiBase".into(), Value::String(url.clone()));
                format!("API base set to {url:?}.")
            }
            Self::Context(n) => {
                root.insert("maxContextTokens".into(), Value::Number((*n).into()));
                format!("Context window set to {n} tokens.")
            }

            // ── services sub-object ───────────────────────────────────────────
            Self::Tts(enabled) => {
                set_service(root, "tts", Value::Bool(*enabled));
                if *enabled {
                    "TTS enabled — takes effect on next restart.".to_string()
                } else {
                    "TTS disabled — takes effect on next restart.".to_string()
                }
            }
            Self::Stt(enabled) => {
                set_service(root, "stt", Value::Bool(*enabled));
                if *enabled {
                    "STT enabled — takes effect on next restart.".to_string()
                } else {
                    "STT disabled — takes effect on next restart.".to_string()
                }
            }
            Self::Llm(target) => match target {
                LlmTarget::Local => {
                    set_service(root, "localLlm", Value::Bool(true));
                    "LLM switched to local (llama.cpp) — takes effect on next restart.".to_string()
                }
                LlmTarget::External => {
                    set_service(root, "localLlm", Value::Bool(false));
                    "LLM switched to external provider — takes effect on next restart.".to_string()
                }
            },
        };

        save_config_json(&config_path, &json)?;
        Ok(message)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Set a field inside the `services` sub-object, creating it if absent.
fn set_service(root: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
    root.entry("services")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .expect("services must be a JSON object")
        .insert(key.into(), value);
}

/// Split `"key rest of line"` into `("key", "rest of line")`.
fn split_key_rest(args: &str) -> Result<(&str, &str)> {
    let args = args.trim();
    let key_end = args.find(char::is_whitespace).unwrap_or(args.len());
    let key = &args[..key_end];
    if key.is_empty() {
        bail!("usage: /settings <setting> <value>  — type /settings help for a list");
    }
    let rest = args[key_end..].trim_start();
    Ok((key, rest))
}

/// Require exactly one non-empty whitespace-delimited token from `rest`.
fn single_token(rest: &str) -> Result<&str> {
    let token = rest.split_whitespace().next().unwrap_or("");
    if token.is_empty() {
        bail!("missing value");
    }
    Ok(token)
}

fn parse_bool_arg(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" | "enable" | "enabled" => Ok(true),
        "off" | "false" | "0" | "no" | "disable" | "disabled" => Ok(false),
        other => bail!("expected on/off, got {other:?}"),
    }
}

fn resolve_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(DEFAULT_CONFIG_PATH)
}

fn load_config_json(path: &Path) -> Result<Value> {
    if path.exists() {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(Value::Object(serde_json::Map::new()))
    }
}

fn save_config_json(path: &Path, value: &Value) -> Result<()> {
    let pretty = serde_json::to_string_pretty(value).context("failed to serialise config.json")?;
    fs::write(path, pretty + "\n").with_context(|| format!("failed to write {}", path.display()))
}

/// Returns a short help string listing all available `/settings` sub-commands.
pub fn settings_help() -> &'static str {
    "\
Settings commands:
  /settings tts on|off            toggle TTS (Christina voice)
  /settings stt on|off            toggle STT (Whisper mic input)
  /settings llm local|external    switch between local llama.cpp and external API
  /settings provider <name>       set LLM provider (openai-chat, openai-responses, anthropic, gemini, ollama, llama-cpp)
  /settings model <name>          set model name (e.g. glm-5:cloud, gpt-4o, qwen3:30b-a3b)
  /settings api-base <url>        set API base URL (e.g. http://127.0.0.1:11434)
  /settings context <tokens>      set max context window in tokens (min 512)

Changes are saved to .amadeus/config.json and take effect on next restart.
Note: when local llama.cpp is enabled, the GGUF model is loaded into RAM at startup
and stays resident for the app lifetime — same as TTS and STT."
}
