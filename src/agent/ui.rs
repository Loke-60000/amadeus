use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    agent::{
        autonomy::AutonomyActivity,
        app::AgentApp,
        config::AgentRuntimeConfig,
        llm::TextStreamSink,
        session::{AgentSession, SessionMessage, SessionRole, SessionVisibility},
    },
};

const DEFAULT_UI_SESSION_ID: &str = "desktop-ui";
const MAX_UI_PROMPT_CHARS: usize = 1_200;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUiTurnRequest {
    pub prompt: String,
    #[serde(default)]
    pub session_id: Option<String>,
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
    base_runtime: AgentRuntimeConfig,
    execution_lock: Arc<Mutex<()>>,
    _autonomy_worker: Option<AutonomyWorker>,
}

impl AgentUiService {
    pub fn new(mut base_runtime: AgentRuntimeConfig) -> Self {
        base_runtime.normalize_provider_defaults();
        let execution_lock = Arc::new(Mutex::new(()));
        let autonomy_worker = spawn_autonomy_worker(&base_runtime, execution_lock.clone());
        Self {
            base_runtime,
            execution_lock,
            _autonomy_worker: autonomy_worker,
        }
    }

    pub fn run_turn(&self, request: AgentUiTurnRequest) -> Result<AgentUiTurnResponse> {
        let _guard = self
            .execution_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the agent runtime"))?;

        let prompt = validate_prompt(&request.prompt)?;
        let runtime = self.runtime_for_session(request.session_id);
        let mut app = AgentApp::new(runtime.clone())?;
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
        let runtime = self.runtime_for_session(request.session_id);
        let mut app = AgentApp::new(runtime.clone())?;
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
        let mut runtime = self.base_runtime.clone();
        runtime.session_id = normalize_session_id(session_id.as_deref());
        runtime
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

                let result = AgentApp::new(runtime.clone()).and_then(|mut app| app.run_autonomy_cycle());
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
