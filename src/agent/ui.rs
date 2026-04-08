use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::Duration,
};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::agent::{
    app::AgentApp,
    autonomy::AutonomyActivity,
    backend::{ConversationBackend, TurnRequest, TurnResponse},
    config::AgentRuntimeConfig,
    llm::{build_model_client, ModelClient, TextStreamSink},
    session::{AgentSession, SessionMessage, SessionRole, SessionVisibility},
};

const DEFAULT_UI_SESSION_ID: &str = "desktop-ui";
const MAX_UI_PROMPT_CHARS: usize = 1_200;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUiTurnRequest {
    pub prompt: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// True when the turn originated from speech-to-text input (voice/S2S mode).
    #[serde(default)]
    pub voice_mode: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUiMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUiAutonomyStatus {
    pub enabled: bool,
    pub cycle_count: u64,
    pub idle_streak: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_focus: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_cycle_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUiTurnResponse {
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub reply: String,
    pub messages: Vec<AgentUiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<AgentUiAutonomyStatus>,
}

pub struct AgentUiService {
    base_runtime: RwLock<AgentRuntimeConfig>,
    execution_lock: Arc<Mutex<()>>,
    /// Pre-loaded model client. `None` until the background load completes (or if
    /// the provider is not LlamaCpp, it stays `None` and we build per-turn instead).
    cached_client: Arc<Mutex<Option<Arc<dyn ModelClient + Send + Sync>>>>,
    _autonomy_worker: Option<AutonomyWorker>,
}

impl AgentUiService {
    pub fn new(mut base_runtime: AgentRuntimeConfig) -> Self {
        base_runtime.normalize_provider_defaults();
        let execution_lock = Arc::new(Mutex::new(()));
        let autonomy_worker = spawn_autonomy_worker(&base_runtime, execution_lock.clone());

        // Pre-load the model client in a background thread so the first user turn
        // doesn't pay the full GGUF load cost.  For cloud providers this is a no-op
        // (construction is cheap); for LlamaCpp it amortises the ~4 GB disk load.
        let cached_client: Arc<Mutex<Option<Arc<dyn ModelClient + Send + Sync>>>> =
            Arc::new(Mutex::new(None));
        let cache_ref = Arc::clone(&cached_client);
        let config_for_preload = base_runtime.clone();
        thread::Builder::new()
            .name("amadeus-llm-preload".to_string())
            .spawn(move || match build_model_client(&config_for_preload) {
                Ok(client) => {
                    let arc_client: Arc<dyn ModelClient + Send + Sync> = Arc::from(client);
                    if let Ok(mut guard) = cache_ref.lock() {
                        *guard = Some(arc_client);
                    }
                }
                Err(e) => {
                    eprintln!("[amadeus] model pre-load failed: {e:#}");
                }
            })
            .ok();

        Self {
            base_runtime: RwLock::new(base_runtime),
            execution_lock,
            cached_client,
            _autonomy_worker: autonomy_worker,
        }
    }

    /// Returns `true` once the model is ready for inference without a blocking load stall.
    /// This is true if either:
    ///   a) the cached client is built (normal path), or
    ///   b) the persistent GGUF handle is already in VRAM (survives reload_config()).
    pub fn is_model_ready(&self) -> bool {
        if self.cached_client.lock().map(|g| g.is_some()).unwrap_or(true) {
            return true;
        }
        // The persistent handle stays loaded across config reloads — building a wrapper is instant.
        crate::agent::llm::llama_cpp::is_handle_loaded()
    }

    /// Reload the base runtime config from disk so subsequent turns use the updated settings.
    /// Call this after writing to config.json (e.g. from a `/settings` command).
    pub fn reload_config(&self) {
        let workspace_root = {
            let rt = self.base_runtime.read().unwrap();
            rt.workspace_root.clone()
        };
        match AgentRuntimeConfig::load(Some(workspace_root), None) {
            Ok(new_config) => {
                // If the user switched away from local LLM, free the model from VRAM.
                if new_config.provider != crate::agent::config::LlmProvider::LlamaCpp {
                    crate::agent::llm::llama_cpp::release_persistent_handle();
                }
                *self.base_runtime.write().unwrap() = new_config;
                // Invalidate the cached client so the next turn re-loads with the new config.
                if let Ok(mut guard) = self.cached_client.lock() {
                    *guard = None;
                }
            }
            Err(e) => {
                eprintln!(
                    "[amadeus] warning: failed to reload config after settings change: {e:#}"
                );
            }
        }
    }

    pub fn run_turn(&self, request: AgentUiTurnRequest) -> Result<AgentUiTurnResponse> {
        let _guard = self
            .execution_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the agent runtime"))?;

        let prompt = validate_prompt(&request.prompt)?;
        let runtime = self.runtime_for_session(request.session_id);
        let client = self.take_or_build_client(&runtime)?;
        let mut app = AgentApp::with_client(runtime.clone(), client)?;
        let reply = app.run_single_prompt(&prompt)?;

        Ok(AgentUiTurnResponse {
            session_id: runtime.session_id.clone(),
            provider: runtime.provider.to_string(),
            model: runtime
                .model
                .clone()
                .unwrap_or_else(|| "(unset)".to_string()),
            reply,
            messages: project_visible_messages(app.session()),
            autonomy: project_autonomy_status(&runtime, app.session()),
        })
    }

    pub fn run_turn_streaming(
        &self,
        request: AgentUiTurnRequest,
        stream: &mut dyn TextStreamSink,
    ) -> Result<AgentUiTurnResponse> {
        let _guard = self
            .execution_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the agent runtime"))?;

        let prompt = validate_prompt(&request.prompt)?;
        let mut runtime = self.runtime_for_session(request.session_id);
        runtime.voice_mode = request.voice_mode;
        let client = self.take_or_build_client(&runtime)?;
        let mut app = AgentApp::with_client(runtime.clone(), client)?;
        let reply = app.run_single_prompt_streaming(&prompt, stream)?;

        Ok(AgentUiTurnResponse {
            session_id: runtime.session_id.clone(),
            provider: runtime.provider.to_string(),
            model: runtime
                .model
                .clone()
                .unwrap_or_else(|| "(unset)".to_string()),
            reply,
            messages: project_visible_messages(app.session()),
            autonomy: project_autonomy_status(&runtime, app.session()),
        })
    }

    fn runtime_for_session(&self, session_id: Option<String>) -> AgentRuntimeConfig {
        let mut runtime = self.base_runtime.read().unwrap().clone();
        runtime.session_id = normalize_session_id(session_id.as_deref());
        runtime
    }

    /// Return the cached pre-loaded client if available; otherwise build a fresh one.
    /// After this call the cache slot is empty — the client is owned by the caller.
    /// The next turn will re-populate the cache via the pre-load thread if needed,
    /// or build synchronously again if the thread hasn't finished yet.
    fn take_or_build_client(
        &self,
        runtime: &AgentRuntimeConfig,
    ) -> Result<Arc<dyn ModelClient + Send + Sync>> {
        // Try to take the pre-loaded client.
        if let Ok(mut guard) = self.cached_client.lock() {
            if let Some(client) = guard.take() {
                // Kick off a new pre-load for the next turn while the current turn runs.
                let cache_ref = Arc::clone(&self.cached_client);
                let config_for_preload = self.base_runtime.read().unwrap().clone();
                thread::Builder::new()
                    .name("amadeus-llm-preload".to_string())
                    .spawn(move || match build_model_client(&config_for_preload) {
                        Ok(next_client) => {
                            let arc_client: Arc<dyn ModelClient + Send + Sync> =
                                Arc::from(next_client);
                            if let Ok(mut g) = cache_ref.lock() {
                                *g = Some(arc_client);
                            }
                        }
                        Err(e) => {
                            eprintln!("[amadeus] model pre-load failed: {e:#}");
                        }
                    })
                    .ok();
                return Ok(client);
            }
        }
        // Cache miss — build synchronously.
        let client = Arc::from(build_model_client(runtime)?);
        Ok(client)
    }
}

fn validate_prompt(prompt: &str) -> Result<String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        bail!("prompt cannot be empty")
    }

    let char_count = trimmed.chars().count();
    if char_count > MAX_UI_PROMPT_CHARS {
        bail!(
            "prompt exceeds the UI limit of {MAX_UI_PROMPT_CHARS} characters ({char_count} provided)"
        )
    }

    Ok(trimmed.to_string())
}

fn normalize_session_id(raw: Option<&str>) -> String {
    let raw = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_UI_SESSION_ID);

    raw.chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect()
}

fn project_visible_messages(session: &AgentSession) -> Vec<AgentUiMessage> {
    session
        .messages
        .iter()
        .filter_map(project_message)
        .collect()
}

fn project_message(message: &SessionMessage) -> Option<AgentUiMessage> {
    if message.visibility != SessionVisibility::Public {
        return None;
    }

    match message.role {
        SessionRole::User => Some(AgentUiMessage {
            role: "user".to_string(),
            content: message.content.clone(),
        }),
        SessionRole::Assistant
            if message.tool_calls.is_empty() && !message.content.trim().is_empty() =>
        {
            Some(AgentUiMessage {
                role: "assistant".to_string(),
                content: message.content.clone(),
            })
        }
        SessionRole::Assistant | SessionRole::Tool => None,
    }
}

fn project_autonomy_status(
    runtime: &AgentRuntimeConfig,
    session: &AgentSession,
) -> Option<AgentUiAutonomyStatus> {
    runtime.autonomy.enabled.then(|| AgentUiAutonomyStatus {
        enabled: true,
        cycle_count: session.autonomy.cycle_count,
        idle_streak: session.autonomy.idle_streak,
        current_focus: session.autonomy.current_focus.clone(),
        pending_goal: session.autonomy.pending_goal.clone(),
        last_outcome: session.autonomy.last_outcome.clone(),
        last_cycle_ms: session.autonomy.last_cycle_ms,
    })
}

struct AutonomyWorker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for AutonomyWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_autonomy_worker(
    base_runtime: &AgentRuntimeConfig,
    execution_lock: Arc<Mutex<()>>,
) -> Option<AutonomyWorker> {
    if !base_runtime.autonomy.enabled || !base_runtime.autonomy.auto_start {
        return None;
    }
    if base_runtime.model.is_none() {
        return None;
    }

    let runtime = base_runtime.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_signal = stop.clone();
    let handle = thread::spawn(move || {
        if !sleep_interruptibly(&stop_signal, runtime.autonomy.initial_delay_secs()) {
            return;
        }

        while !stop_signal.load(Ordering::Relaxed) {
            let next_sleep = {
                let _guard = match execution_lock.lock() {
                    Ok(guard) => guard,
                    Err(_) => return,
                };

                let result =
                    AgentApp::new(runtime.clone()).and_then(|mut app| app.run_autonomy_cycle());
                match result {
                    Ok(report) => {
                        let activity = match report.activity {
                            AutonomyActivity::Acted => "acted",
                            AutonomyActivity::Idle => "idle",
                        };
                        eprintln!(
                            "[autonomy] {activity}: {} ({})",
                            report.focus, report.summary
                        );
                        report.next_interval_secs
                    }
                    Err(error) => {
                        eprintln!("[autonomy] cycle failed: {error:#}");
                        runtime
                            .autonomy
                            .idle_backoff_secs
                            .max(runtime.autonomy.interval_secs)
                    }
                }
            };

            if !sleep_interruptibly(&stop_signal, next_sleep) {
                return;
            }
        }
    });

    Some(AutonomyWorker {
        stop,
        handle: Some(handle),
    })
}

fn sleep_interruptibly(stop: &AtomicBool, seconds: u64) -> bool {
    for _ in 0..seconds.max(1) {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_secs(1));
    }
    !stop.load(Ordering::Relaxed)
}

// ── ConversationBackend impl ──────────────────────────────────────────────────

impl ConversationBackend for AgentUiService {
    fn is_ready(&self) -> bool {
        AgentUiService::is_model_ready(self)
    }

    fn reload_config(&self) {
        AgentUiService::reload_config(self);
    }

    fn run_turn(&self, request: TurnRequest) -> anyhow::Result<TurnResponse> {
        let resp = AgentUiService::run_turn(
            self,
            AgentUiTurnRequest {
                prompt: request.prompt,
                session_id: request.session_id,
                voice_mode: request.voice_mode,
            },
        )?;
        Ok(TurnResponse { reply: resp.reply })
    }

    fn run_turn_streaming(
        &self,
        request: TurnRequest,
        sink: &mut dyn TextStreamSink,
    ) -> anyhow::Result<TurnResponse> {
        let resp = AgentUiService::run_turn_streaming(
            self,
            AgentUiTurnRequest {
                prompt: request.prompt,
                session_id: request.session_id,
                voice_mode: request.voice_mode,
            },
            sink,
        )?;
        Ok(TurnResponse { reply: resp.reply })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::{SessionMessage, SessionToolCall};

    #[test]
    fn normalizes_session_ids_for_ui_use() {
        assert_eq!(normalize_session_id(None), "desktop-ui");
        assert_eq!(normalize_session_id(Some(" ui main ")), "ui-main");
        assert_eq!(normalize_session_id(Some("alpha_beta-01")), "alpha_beta-01");
    }

    #[test]
    fn projects_only_visible_conversation_messages() {
        let session = AgentSession {
            id: "desktop-ui".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            messages: vec![
                SessionMessage {
                    role: SessionRole::User,
                    content: "hello".to_string(),
                    visibility: SessionVisibility::Public,
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                },
                SessionMessage {
                    role: SessionRole::Assistant,
                    content: String::new(),
                    visibility: SessionVisibility::Public,
                    name: None,
                    tool_call_id: None,
                    tool_calls: vec![SessionToolCall {
                        id: "call-1".to_string(),
                        name: "run_shell".to_string(),
                        arguments: "{}".to_string(),
                    }],
                },
                SessionMessage {
                    role: SessionRole::Tool,
                    content: "tool result".to_string(),
                    visibility: SessionVisibility::Public,
                    name: Some("run_shell".to_string()),
                    tool_call_id: Some("call-1".to_string()),
                    tool_calls: Vec::new(),
                },
                SessionMessage {
                    role: SessionRole::Assistant,
                    content: "final answer".to_string(),
                    visibility: SessionVisibility::Public,
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                },
            ],
            autonomy: Default::default(),
            context: Default::default(),
        };

        let projected = project_visible_messages(&session);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].role, "user");
        assert_eq!(projected[1].role, "assistant");
        assert_eq!(projected[1].content, "final answer");
    }

    #[test]
    fn hides_internal_messages_from_ui_projection() {
        let session = AgentSession {
            id: "desktop-ui".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            autonomy: Default::default(),
            context: Default::default(),
            messages: vec![SessionMessage {
                role: SessionRole::User,
                content: "internal".to_string(),
                visibility: SessionVisibility::Internal,
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            }],
        };

        assert!(project_visible_messages(&session).is_empty());
    }
}
