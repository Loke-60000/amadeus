use std::{
    collections::{BTreeSet, HashMap},
    env, fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::mcp::types::McpServerConfig;

const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
pub(crate) const AMADEUS_DIR_NAME: &str = ".amadeus";
pub(crate) const AMADEUS_SESSIONS_DIR_NAME: &str = "sessions";
pub(crate) const AMADEUS_WORKSPACE_DIR_NAME: &str = "workspace";
pub const DEFAULT_CONFIG_PATH: &str = ".amadeus/config.json";
const DEFAULT_SESSION_ID: &str = "main";
const DEFAULT_MAX_TOOL_ROUNDS: usize = 8;
const DEFAULT_TEMPERATURE: f32 = 0.2;
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 2_048;
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 16_000;
const DEFAULT_MAX_TIMEOUT_SECS: u64 = 120;
const DEFAULT_MAX_OUTPUT_CHARS: usize = 32_000;
const DEFAULT_AUTONOMY_INTERVAL_SECS: u64 = 180;
const DEFAULT_AUTONOMY_IDLE_BACKOFF_SECS: u64 = 420;
const DEFAULT_AUTONOMY_MAX_CYCLES_PER_RUN: usize = 1;
const DEFAULT_AUTONOMY_RESEARCH_ABSENT_USER_MINS: u64 = 180;
const DEFAULT_AUTONOMY_RESEARCH_MAX_PENDING_NOTES: usize = 6;

#[derive(Clone, Debug)]
pub struct AutonomyResearchConfig {
    pub enabled: bool,
    pub absent_user_minutes: u64,
    pub max_pending_notes: usize,
    pub topics: Vec<String>,
}

impl Default for AutonomyResearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            absent_user_minutes: DEFAULT_AUTONOMY_RESEARCH_ABSENT_USER_MINS,
            max_pending_notes: DEFAULT_AUTONOMY_RESEARCH_MAX_PENDING_NOTES,
            topics: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AutonomyConfig {
    pub enabled: bool,
    pub auto_start: bool,
    pub interval_secs: u64,
    pub idle_backoff_secs: u64,
    pub max_cycles_per_run: usize,
    pub research: AutonomyResearchConfig,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_start: false,
            interval_secs: DEFAULT_AUTONOMY_INTERVAL_SECS,
            idle_backoff_secs: DEFAULT_AUTONOMY_IDLE_BACKOFF_SECS,
            max_cycles_per_run: DEFAULT_AUTONOMY_MAX_CYCLES_PER_RUN,
            research: AutonomyResearchConfig::default(),
        }
    }
}

impl AutonomyConfig {
    pub fn initial_delay_secs(&self) -> u64 {
        self.interval_secs.min(30)
    }
}

/// Controls which services load at startup.  Stored under `services` in config.json.
#[derive(Clone, Debug)]
pub struct ServicesConfig {
    /// Load the TTS (Christina voice) engine.  Default: true.
    pub tts: bool,
    /// Load the STT (Whisper) engine.  Default: true.
    pub stt: bool,
    /// Use the local llama.cpp model instead of the configured external provider.  Default: false.
    pub local_llm: bool,
    /// Path to the GGUF model file for local inference.
    /// Defaults to `assets/models/llm/Qwen3-4B-q8_0.gguf` relative to the manifest.
    pub local_llm_model_path: Option<PathBuf>,
}

impl Default for ServicesConfig {
    fn default() -> Self {
        Self {
            tts: true,
            stt: true,
            local_llm: false,
            local_llm_model_path: None,
        }
    }
}

pub(crate) fn amadeus_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(AMADEUS_DIR_NAME)
}

pub(crate) fn default_bootstrap_workspace_dir(workspace_root: &Path) -> PathBuf {
    amadeus_dir(workspace_root).join(AMADEUS_WORKSPACE_DIR_NAME)
}

pub(crate) fn default_sessions_dir(workspace_root: &Path) -> PathBuf {
    amadeus_dir(workspace_root).join(AMADEUS_SESSIONS_DIR_NAME)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmProvider {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
    Gemini,
    Ollama,
    /// Local llama.cpp inference — model loaded from `ServicesConfig::local_llm_model_path`.
    LlamaCpp,
}

impl LlmProvider {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "openai" | "openai-chat" | "openai-chat-completions" | "chat-completions" => {
                Ok(Self::OpenAiChat)
            }
            "openai-responses" | "responses" => Ok(Self::OpenAiResponses),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "gemini" | "google" => Ok(Self::Gemini),
            "ollama" => Ok(Self::Ollama),
            "llama-cpp" | "llama_cpp" | "llamacpp" | "local" => Ok(Self::LlamaCpp),
            other => bail!(
                "unsupported provider {other:?}; expected openai-chat, openai-responses, anthropic, gemini, ollama, or llama-cpp"
            ),
        }
    }

    pub fn default_api_base(self) -> &'static str {
        match self {
            Self::OpenAiChat | Self::OpenAiResponses => DEFAULT_API_BASE,
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::Ollama => "http://127.0.0.1:11434",
            Self::LlamaCpp => "",
        }
    }

    pub fn default_api_key(self) -> Option<String> {
        match self {
            Self::OpenAiChat | Self::OpenAiResponses => {
                env::var("OPENAI_API_KEY").ok().and_then(normalize_optional)
            }
            Self::Anthropic => env::var("ANTHROPIC_API_KEY")
                .ok()
                .and_then(normalize_optional),
            Self::Gemini => env::var("GEMINI_API_KEY")
                .ok()
                .and_then(normalize_optional)
                .or_else(|| env::var("GOOGLE_API_KEY").ok().and_then(normalize_optional)),
            Self::Ollama | Self::LlamaCpp => None,
        }
    }
}

impl fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenAiChat => write!(f, "openai-chat"),
            Self::OpenAiResponses => write!(f, "openai-responses"),
            Self::Anthropic => write!(f, "anthropic"),
            Self::Gemini => write!(f, "gemini"),
            Self::Ollama => write!(f, "ollama"),
            Self::LlamaCpp => write!(f, "llama-cpp"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellSecurityMode {
    Ask,
    Allowlist,
    Full,
}

impl ShellSecurityMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ask" => Ok(Self::Ask),
            "allowlist" | "strict" => Ok(Self::Allowlist),
            "full" => Ok(Self::Full),
            other => {
                bail!("unsupported shell security mode {other:?}; expected ask, allowlist, or full")
            }
        }
    }
}

impl fmt::Display for ShellSecurityMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ask => write!(f, "ask"),
            Self::Allowlist => write!(f, "allowlist"),
            Self::Full => write!(f, "full"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShellPolicyConfig {
    pub mode: ShellSecurityMode,
    pub allow_shell: bool,
    pub allowed_bins: BTreeSet<String>,
    pub max_timeout_secs: u64,
    pub max_output_chars: usize,
}

impl Default for ShellPolicyConfig {
    fn default() -> Self {
        let allowed_bins = [
            "awk", "cargo", "cat", "cp", "echo", "find", "git", "grep", "head", "ls", "mkdir",
            "mv", "printf", "pwd", "rg", "rustc", "sed", "tail", "touch", "wc",
        ]
        .into_iter()
        .map(ToString::to_string)
        .collect();

        Self {
            mode: ShellSecurityMode::Ask,
            allow_shell: false,
            allowed_bins,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
            max_output_chars: DEFAULT_MAX_OUTPUT_CHARS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AgentRuntimeConfig {
    pub workspace_root: PathBuf,
    pub session_id: String,
    pub provider: LlmProvider,
    pub model: Option<String>,
    pub api_base: String,
    pub api_key: Option<String>,
    /// API key for web search (Brave Search API). Falls back to DuckDuckGo if absent.
    pub search_api_key: Option<String>,
    /// MCP server configurations loaded from `.amadeus/config.json` → `mcpServers`.
    pub mcp_servers: HashMap<String, McpServerConfig>,
    pub temperature: f32,
    pub max_output_tokens: usize,
    pub max_context_tokens: usize,
    pub max_tool_rounds: usize,
    pub autonomy: AutonomyConfig,
    pub shell_policy: ShellPolicyConfig,
    pub services: ServicesConfig,
    /// Set at turn-time (never persisted) to indicate the turn came from voice/S2S input.
    pub voice_mode: bool,
}

impl AgentRuntimeConfig {
    pub fn load(
        workspace_root_override: Option<PathBuf>,
        config_path_override: Option<PathBuf>,
    ) -> Result<Self> {
        let workspace_root = match workspace_root_override {
            Some(workspace_root) => workspace_root,
            None => env::current_dir().context("failed to determine the current workspace")?,
        };
        let mut runtime = Self::with_defaults(workspace_root);

        if let Some(config_path) =
            resolve_config_path(&runtime.workspace_root, config_path_override.as_deref())?
        {
            let config_dir = config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            let file_config = load_json_config(&config_path)?;
            runtime.apply_json_config(file_config, &config_dir)?;
        }

        runtime.apply_env_overrides()?;
        runtime.normalize_provider_defaults();
        Ok(runtime)
    }

    fn with_defaults(workspace_root: PathBuf) -> Self {
        let shell_policy = ShellPolicyConfig::default();
        let provider = LlmProvider::OpenAiChat;

        Self {
            workspace_root,
            session_id: DEFAULT_SESSION_ID.to_string(),
            provider,
            model: None,
            api_base: provider.default_api_base().to_string(),
            api_key: provider.default_api_key(),
            search_api_key: env::var("AMADEUS_SEARCH_API_KEY")
                .ok()
                .and_then(normalize_optional),
            mcp_servers: HashMap::new(),
            temperature: DEFAULT_TEMPERATURE,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_context_tokens: DEFAULT_MAX_CONTEXT_TOKENS,
            max_tool_rounds: DEFAULT_MAX_TOOL_ROUNDS,
            autonomy: AutonomyConfig::default(),
            shell_policy,
            services: ServicesConfig::default(),
            voice_mode: false,
        }
    }

    fn apply_json_config(
        &mut self,
        config: JsonAgentRuntimeConfig,
        config_dir: &Path,
    ) -> Result<()> {
        let provider_override = config.provider.as_deref();
        let api_base_override = config.api_base.clone().and_then(normalize_optional);
        let api_key_override = config.api_key.clone().and_then(normalize_optional);

        if let Some(workspace_root) = config.workspace_root {
            self.workspace_root = resolve_config_path_value(config_dir, workspace_root);
        }
        if let Some(session_id) = config.session_id.and_then(normalize_optional) {
            self.session_id = session_id;
        }
        if let Some(provider) = provider_override {
            let previous_provider = self.provider;
            self.provider = LlmProvider::parse(&provider)?;
            if api_base_override.is_none()
                && should_refresh_api_base(&self.api_base, previous_provider)
            {
                self.api_base = self.provider.default_api_base().to_string();
            }
            if api_key_override.is_none()
                && should_refresh_api_key(self.api_key.as_deref(), previous_provider)
            {
                self.api_key = self.provider.default_api_key();
            }
        }
        if let Some(model) = config.model {
            self.model = normalize_optional(model);
        }
        if let Some(api_base) = api_base_override {
            self.api_base = api_base;
        }
        if let Some(api_key) = api_key_override {
            self.api_key = Some(api_key);
        }
        if let Some(temperature) = config.temperature {
            self.temperature = temperature;
        }
        if let Some(max_output_tokens) = config.max_output_tokens {
            self.max_output_tokens = max_output_tokens.max(1);
        }
        if let Some(max_context_tokens) = config.max_context_tokens {
            self.max_context_tokens = max_context_tokens.max(512);
        }
        if let Some(max_tool_rounds) = config.max_tool_rounds {
            self.max_tool_rounds = max_tool_rounds.max(1);
        }
        if let Some(autonomy) = config.autonomy {
            self.apply_json_autonomy(autonomy);
        }
        if let Some(shell_policy) = config.shell_policy {
            self.apply_json_shell_policy(shell_policy)?;
        }
        if let Some(mcp_servers) = config.mcp_servers {
            self.mcp_servers.extend(mcp_servers);
        }
        if let Some(services) = config.services {
            self.apply_json_services(services, config_dir);
        }
        Ok(())
    }

    fn apply_json_autonomy(&mut self, autonomy: JsonAutonomyConfig) {
        if let Some(enabled) = autonomy.enabled {
            self.autonomy.enabled = enabled;
        }
        if let Some(auto_start) = autonomy.auto_start {
            self.autonomy.auto_start = auto_start;
        }
        if let Some(interval_secs) = autonomy.interval_secs {
            self.autonomy.interval_secs = interval_secs.max(1);
        }
        if let Some(idle_backoff_secs) = autonomy.idle_backoff_secs {
            self.autonomy.idle_backoff_secs = idle_backoff_secs.max(1);
        }
        if let Some(max_cycles_per_run) = autonomy.max_cycles_per_run {
            self.autonomy.max_cycles_per_run = max_cycles_per_run.max(1);
        }
        if let Some(research) = autonomy.research {
            self.apply_json_autonomy_research(research);
        }
    }

    fn apply_json_autonomy_research(&mut self, research: JsonAutonomyResearchConfig) {
        if let Some(enabled) = research.enabled {
            self.autonomy.research.enabled = enabled;
        }
        if let Some(absent_user_minutes) = research.absent_user_minutes {
            self.autonomy.research.absent_user_minutes = absent_user_minutes.max(1);
        }
        if let Some(max_pending_notes) = research.max_pending_notes {
            self.autonomy.research.max_pending_notes = max_pending_notes.max(1);
        }
        if let Some(topics) = research.topics {
            self.autonomy.research.topics =
                topics.into_iter().filter_map(normalize_optional).collect();
        }
    }

    fn apply_json_shell_policy(&mut self, shell_policy: JsonShellPolicyConfig) -> Result<()> {
        if let Some(mode) = shell_policy.mode {
            self.shell_policy.mode = ShellSecurityMode::parse(&mode)?;
        }
        if let Some(allow_shell) = shell_policy.allow_shell {
            self.shell_policy.allow_shell = allow_shell;
        }
        if let Some(allowed_bins) = shell_policy.allowed_bins {
            self.shell_policy.allowed_bins.extend(
                allowed_bins
                    .into_iter()
                    .filter_map(normalize_optional)
                    .map(|value| value.to_ascii_lowercase()),
            );
        }
        if let Some(max_timeout_secs) = shell_policy.max_timeout_secs {
            self.shell_policy.max_timeout_secs = max_timeout_secs.max(1);
        }
        if let Some(max_output_chars) = shell_policy.max_output_chars {
            self.shell_policy.max_output_chars = max_output_chars.max(1_000);
        }
        Ok(())
    }

    fn apply_json_services(&mut self, services: JsonServicesConfig, _config_dir: &Path) {
        if let Some(tts) = services.tts {
            self.services.tts = tts;
        }
        if let Some(stt) = services.stt {
            self.services.stt = stt;
        }
        if let Some(local_llm) = services.local_llm {
            self.services.local_llm = local_llm;
        }
        if let Some(path) = services.local_llm_model_path {
            // Model paths are relative to workspace root, not the .amadeus/ config dir.
            self.services.local_llm_model_path = Some(resolve_config_path_value(
                &self.workspace_root.clone(),
                path,
            ));
        }
    }

    fn apply_env_overrides(&mut self) -> Result<()> {
        let provider_override = env::var("AMADEUS_AGENT_PROVIDER").ok();
        let api_base_override = env::var("AMADEUS_AGENT_API_BASE")
            .ok()
            .and_then(normalize_optional);
        let api_key_override = env::var("AMADEUS_AGENT_API_KEY")
            .ok()
            .and_then(normalize_optional);

        if let Some(raw) = provider_override {
            let previous_provider = self.provider;
            self.provider = LlmProvider::parse(&raw)?;
            if api_base_override.is_none()
                && should_refresh_api_base(&self.api_base, previous_provider)
            {
                self.api_base = self.provider.default_api_base().to_string();
            }
            if api_key_override.is_none()
                && should_refresh_api_key(self.api_key.as_deref(), previous_provider)
            {
                self.api_key = self.provider.default_api_key();
            }
        }

        if let Some(raw) = env::var("AMADEUS_AGENT_SECURITY").ok() {
            self.shell_policy.mode = ShellSecurityMode::parse(&raw)?;
        }

        if let Some(raw) = env::var("AMADEUS_AGENT_ALLOW_SHELL").ok() {
            self.shell_policy.allow_shell = parse_bool_flag(&raw);
        }

        if let Some(raw) = env::var("AMADEUS_AGENT_ALLOWED_BINS").ok() {
            let extra = raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase());
            self.shell_policy.allowed_bins.extend(extra);
        }

        if let Some(raw) = env::var("AMADEUS_AGENT_COMMAND_TIMEOUT").ok() {
            if let Ok(timeout_secs) = raw.trim().parse::<u64>() {
                self.shell_policy.max_timeout_secs = timeout_secs.max(1);
            }
        }

        if let Some(raw) = env::var("AMADEUS_AGENT_MAX_OUTPUT_CHARS").ok() {
            if let Ok(max_output_chars) = raw.trim().parse::<usize>() {
                self.shell_policy.max_output_chars = max_output_chars.max(1_000);
            }
        }

        if let Some(raw) = env::var("AMADEUS_AGENT_MODEL").ok() {
            self.model = normalize_optional(raw);
        }
        if let Some(api_base) = api_base_override {
            self.api_base = api_base;
        }
        if let Some(api_key) = api_key_override {
            self.api_key = Some(api_key);
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_TEMPERATURE").ok() {
            if let Ok(temperature) = raw.parse::<f32>() {
                self.temperature = temperature;
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_MAX_OUTPUT_TOKENS").ok() {
            if let Ok(max_output_tokens) = raw.parse::<usize>() {
                self.max_output_tokens = max_output_tokens.max(1);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_MAX_CONTEXT_TOKENS").ok() {
            if let Ok(max_context_tokens) = raw.parse::<usize>() {
                self.max_context_tokens = max_context_tokens.max(512);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_MAX_TOOL_ROUNDS").ok() {
            if let Ok(max_tool_rounds) = raw.parse::<usize>() {
                self.max_tool_rounds = max_tool_rounds.max(1);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY").ok() {
            self.autonomy.enabled = parse_bool_flag(&raw);
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_AUTO_START").ok() {
            self.autonomy.auto_start = parse_bool_flag(&raw);
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_INTERVAL").ok() {
            if let Ok(interval_secs) = raw.trim().parse::<u64>() {
                self.autonomy.interval_secs = interval_secs.max(1);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_IDLE_BACKOFF").ok() {
            if let Ok(idle_backoff_secs) = raw.trim().parse::<u64>() {
                self.autonomy.idle_backoff_secs = idle_backoff_secs.max(1);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_MAX_CYCLES").ok() {
            if let Ok(max_cycles_per_run) = raw.trim().parse::<usize>() {
                self.autonomy.max_cycles_per_run = max_cycles_per_run.max(1);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_RESEARCH").ok() {
            self.autonomy.research.enabled = parse_bool_flag(&raw);
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_RESEARCH_ABSENT_USER_MINS").ok() {
            if let Ok(absent_user_minutes) = raw.trim().parse::<u64>() {
                self.autonomy.research.absent_user_minutes = absent_user_minutes.max(1);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_RESEARCH_MAX_PENDING_NOTES").ok() {
            if let Ok(max_pending_notes) = raw.trim().parse::<usize>() {
                self.autonomy.research.max_pending_notes = max_pending_notes.max(1);
            }
        }
        if let Some(raw) = env::var("AMADEUS_AGENT_AUTONOMY_RESEARCH_TOPICS").ok() {
            self.autonomy.research.topics = raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect();
        }

        Ok(())
    }

    pub fn normalize_provider_defaults(&mut self) {
        self.model = self.model.take().and_then(normalize_optional);
        // LlamaCpp has no remote API base; skip the default substitution.
        if self.provider != LlmProvider::LlamaCpp {
            self.api_base = normalize_optional(self.api_base.clone())
                .unwrap_or_else(|| self.provider.default_api_base().to_string());
        }
        self.api_key = self
            .api_key
            .take()
            .and_then(normalize_optional)
            .or_else(|| self.provider.default_api_key());

        // Apply the default GGUF model path when local LLM is enabled and no path was configured.
        if self.services.local_llm && self.services.local_llm_model_path.is_none() {
            self.services.local_llm_model_path = Some(
                self.workspace_root
                    .join("assets/models/llm/Qwen3-4B-q8_0.gguf"),
            );
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct JsonServicesConfig {
    tts: Option<bool>,
    stt: Option<bool>,
    #[serde(alias = "localLlm")]
    local_llm: Option<bool>,
    #[serde(alias = "localLlmModelPath")]
    local_llm_model_path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct JsonAgentRuntimeConfig {
    #[serde(alias = "workspaceRoot")]
    workspace_root: Option<PathBuf>,
    #[serde(alias = "sessionId")]
    session_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    #[serde(alias = "apiBase")]
    api_base: Option<String>,
    #[serde(alias = "apiKey")]
    api_key: Option<String>,
    temperature: Option<f32>,
    #[serde(alias = "maxOutputTokens")]
    max_output_tokens: Option<usize>,
    #[serde(alias = "maxContextTokens")]
    max_context_tokens: Option<usize>,
    #[serde(alias = "maxToolRounds")]
    max_tool_rounds: Option<usize>,
    autonomy: Option<JsonAutonomyConfig>,
    #[serde(alias = "shellPolicy")]
    shell_policy: Option<JsonShellPolicyConfig>,
    #[serde(alias = "mcpServers")]
    mcp_servers: Option<HashMap<String, McpServerConfig>>,
    services: Option<JsonServicesConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct JsonAutonomyConfig {
    enabled: Option<bool>,
    #[serde(alias = "autoStart")]
    auto_start: Option<bool>,
    #[serde(alias = "intervalSecs")]
    interval_secs: Option<u64>,
    #[serde(alias = "idleBackoffSecs")]
    idle_backoff_secs: Option<u64>,
    #[serde(alias = "maxCyclesPerRun")]
    max_cycles_per_run: Option<usize>,
    research: Option<JsonAutonomyResearchConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct JsonAutonomyResearchConfig {
    enabled: Option<bool>,
    #[serde(alias = "absentUserMinutes")]
    absent_user_minutes: Option<u64>,
    #[serde(alias = "maxPendingNotes")]
    max_pending_notes: Option<usize>,
    topics: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct JsonShellPolicyConfig {
    mode: Option<String>,
    #[serde(alias = "allowShell")]
    allow_shell: Option<bool>,
    #[serde(alias = "allowedBins")]
    allowed_bins: Option<Vec<String>>,
    #[serde(alias = "maxTimeoutSecs")]
    max_timeout_secs: Option<u64>,
    #[serde(alias = "maxOutputChars")]
    max_output_chars: Option<usize>,
}

fn resolve_config_path(
    workspace_root: &Path,
    config_path_override: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(config_path) = config_path_override {
        let resolved = if config_path.is_absolute() {
            config_path.to_path_buf()
        } else {
            workspace_root.join(config_path)
        };
        if !resolved.is_file() {
            bail!("agent config file {} does not exist", resolved.display());
        }
        return Ok(Some(resolved));
    }

    let default_path = workspace_root.join(DEFAULT_CONFIG_PATH);
    Ok(default_path.is_file().then_some(default_path))
}

fn load_json_config(path: &Path) -> Result<JsonAgentRuntimeConfig> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read agent config {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse agent config {}", path.display()))
}

fn resolve_config_path_value(base_dir: &Path, value: PathBuf) -> PathBuf {
    if value.is_absolute() {
        value
    } else {
        base_dir.join(value)
    }
}

fn normalize_optional(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn should_refresh_api_base(current_api_base: &str, previous_provider: LlmProvider) -> bool {
    let trimmed = current_api_base.trim();
    trimmed.is_empty() || trimmed == previous_provider.default_api_base()
}

fn should_refresh_api_key(current_api_key: Option<&str>, previous_provider: LlmProvider) -> bool {
    let normalized_current = current_api_key
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let default_key = previous_provider.default_api_key();
    normalized_current.is_none() || normalized_current == default_key.as_deref()
}

fn parse_bool_flag(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use super::{load_json_config, resolve_config_path, AgentRuntimeConfig, LlmProvider};

    #[test]
    fn resolve_config_path_reads_dot_amadeus_config() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().join("workspace");
        let config_dir = workspace_root.join(".amadeus");
        fs::create_dir_all(&config_dir)?;
        fs::write(config_dir.join("config.json"), "{}")?;

        let resolved = resolve_config_path(&workspace_root, None)?;

        assert_eq!(resolved, Some(config_dir.join("config.json")));
        Ok(())
    }

    #[test]
    fn json_config_applies_ollama_settings() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().join("workspace");
        let config_dir = workspace_root.join(".amadeus");
        let config_path = config_dir.join("config.json");
        fs::create_dir_all(&config_dir)?;
        fs::write(
            &config_path,
            r#"{
  "provider": "ollama",
  "model": "glm-5:cloud",
  "temperature": 0.15,
  "shellPolicy": {
    "mode": "allowlist",
    "allowShell": true
  }
}"#,
        )?;

        let mut runtime = AgentRuntimeConfig::with_defaults(workspace_root);
        let config = load_json_config(&config_path)?;
        runtime.apply_json_config(config, &config_dir)?;
        runtime.normalize_provider_defaults();

        assert_eq!(runtime.provider, LlmProvider::Ollama);
        assert_eq!(runtime.model.as_deref(), Some("glm-5:cloud"));
        assert_eq!(runtime.api_base, "http://127.0.0.1:11434");
        assert_eq!(runtime.temperature, 0.15);
        assert!(runtime.shell_policy.allow_shell);
        Ok(())
    }

    #[test]
    fn json_config_applies_autonomy_settings() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().join("workspace");
        let config_dir = workspace_root.join(".amadeus");
        let config_path = config_dir.join("config.json");
        fs::create_dir_all(&config_dir)?;
        fs::write(
            &config_path,
            r#"{
  "autonomy": {
    "enabled": true,
    "autoStart": true,
    "intervalSecs": 45,
    "idleBackoffSecs": 90,
    "maxCyclesPerRun": 3
  }
}"#,
        )?;

        let mut runtime = AgentRuntimeConfig::with_defaults(workspace_root);
        let config = load_json_config(&config_path)?;
        runtime.apply_json_config(config, &config_dir)?;

        assert!(runtime.autonomy.enabled);
        assert!(runtime.autonomy.auto_start);
        assert_eq!(runtime.autonomy.interval_secs, 45);
        assert_eq!(runtime.autonomy.idle_backoff_secs, 90);
        assert_eq!(runtime.autonomy.max_cycles_per_run, 3);
        Ok(())
    }

    #[test]
    fn json_config_applies_context_budget_and_research_settings() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().join("workspace");
        let config_dir = workspace_root.join(".amadeus");
        let config_path = config_dir.join("config.json");
        fs::create_dir_all(&config_dir)?;
        fs::write(
            &config_path,
            r#"{
    "maxContextTokens": 12000,
    "autonomy": {
        "research": {
            "enabled": true,
            "absentUserMinutes": 60,
            "maxPendingNotes": 4,
            "topics": ["continuity", "context compaction"]
        }
    }
}"#,
        )?;

        let mut runtime = AgentRuntimeConfig::with_defaults(workspace_root);
        let config = load_json_config(&config_path)?;
        runtime.apply_json_config(config, &config_dir)?;

        assert_eq!(runtime.max_context_tokens, 12_000);
        assert!(runtime.autonomy.research.enabled);
        assert_eq!(runtime.autonomy.research.absent_user_minutes, 60);
        assert_eq!(runtime.autonomy.research.max_pending_notes, 4);
        assert_eq!(
            runtime.autonomy.research.topics,
            vec!["continuity".to_string(), "context compaction".to_string()]
        );
        Ok(())
    }
}
