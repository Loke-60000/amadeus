use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::config::{LlmProvider, AMADEUS_DIR_NAME, DEFAULT_CONFIG_PATH};

pub const PROVIDERS_FILE_NAME: &str = "providers.json";

/// A named, saved provider configuration that can be activated from the settings panel.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    /// Display name shown in the settings UI.
    pub name: String,
    /// Provider type string (e.g. "ollama", "openai-chat", "anthropic").
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
}

/// Manages the list of saved provider profiles stored in `.amadeus/providers.json`.
pub struct ProvidersStore {
    profiles: Vec<ProviderProfile>,
    workspace_root: PathBuf,
}

impl ProvidersStore {
    /// Load from `.amadeus/providers.json`. Returns an empty store if the file does not exist.
    pub fn load(workspace_root: &Path) -> Self {
        let path = providers_path(workspace_root);
        let profiles = load_profiles(&path).unwrap_or_default();
        Self {
            profiles,
            workspace_root: workspace_root.to_path_buf(),
        }
    }

    pub fn profiles(&self) -> &[ProviderProfile] {
        &self.profiles
    }

    /// Apply profile at `index` to `config.json`, mark it as the active provider, and return
    /// the profile's display name.
    pub fn select(&self, index: usize) -> Result<String> {
        let profile = self
            .profiles
            .get(index)
            .with_context(|| format!("provider index {index} out of range"))?;

        // Validate provider string before touching config.json.
        LlmProvider::parse(&profile.provider).with_context(|| {
            format!(
                "invalid provider {:?} in profile {:?}",
                profile.provider, profile.name
            )
        })?;

        let config_path = self.workspace_root.join(DEFAULT_CONFIG_PATH);
        let mut json = load_config_json(&config_path)?;
        let root = json
            .as_object_mut()
            .context("config.json must be a JSON object")?;

        root.insert("provider".into(), Value::String(profile.provider.clone()));

        if let Some(model) = &profile.model {
            root.insert("model".into(), Value::String(model.clone()));
        }
        if let Some(api_base) = &profile.api_base {
            root.insert("apiBase".into(), Value::String(api_base.clone()));
        }
        if let Some(api_key) = &profile.api_key {
            root.insert("apiKey".into(), Value::String(api_key.clone()));
        }
        if let Some(temp) = profile.temperature {
            root.insert("temperature".into(), serde_json::json!(temp));
        }
        if let Some(max_out) = profile.max_output_tokens {
            root.insert("maxOutputTokens".into(), Value::Number(max_out.into()));
        }
        if let Some(max_ctx) = profile.max_context_tokens {
            root.insert("maxContextTokens".into(), Value::Number(max_ctx.into()));
        }

        // Record which profile is active so we can restore the selection on the next launch.
        root.insert("activeProvider".into(), Value::String(profile.name.clone()));

        save_config_json(&config_path, &json)?;
        Ok(profile.name.clone())
    }

    /// Returns the index of the active profile by matching the `activeProvider` field in
    /// `config.json` against the loaded profile names, or `None` when unset or unmatched.
    pub fn active_index(&self) -> Option<usize> {
        let config_path = self.workspace_root.join(DEFAULT_CONFIG_PATH);
        let raw = fs::read_to_string(config_path).ok()?;
        let json: Value = serde_json::from_str(&raw).ok()?;
        let active_name = json.get("activeProvider")?.as_str()?;
        self.profiles.iter().position(|p| p.name == active_name)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn providers_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(AMADEUS_DIR_NAME)
        .join(PROVIDERS_FILE_NAME)
}

fn load_profiles(path: &Path) -> Option<Vec<ProviderProfile>> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
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
