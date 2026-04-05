use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::llm::ModelToolCall;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionVisibility {
    #[default]
    Public,
    Internal,
}

impl SessionVisibility {
    fn is_public(&self) -> bool {
        matches!(self, Self::Public)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAutonomyDrives {
    pub curiosity: f32,
    pub maintenance: f32,
    pub follow_through: f32,
    pub caution: f32,
}

impl Default for SessionAutonomyDrives {
    fn default() -> Self {
        Self {
            curiosity: 0.62,
            maintenance: 0.55,
            follow_through: 0.72,
            caution: 0.38,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAutonomyChemistry {
    pub excitement: f32,
    pub satisfaction: f32,
    pub frustration: f32,
    pub loneliness: f32,
    pub fatigue: f32,
}

impl Default for SessionAutonomyChemistry {
    fn default() -> Self {
        Self {
            excitement: 0.44,
            satisfaction: 0.42,
            frustration: 0.18,
            loneliness: 0.28,
            fatigue: 0.14,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAutonomyInitiativeKind {
    #[default]
    Research,
    Maintenance,
    Continuity,
    Review,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAutonomySubagent {
    #[default]
    Scientist,
    Engineer,
    Archivist,
    Skeptic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAutonomyInterest {
    pub topic: String,
    pub rationale: String,
    pub source: String,
    #[serde(default)]
    pub kind: SessionAutonomyInitiativeKind,
    #[serde(default)]
    pub subagent: SessionAutonomySubagent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_selected_ms: Option<u64>,
    #[serde(default)]
    pub selection_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAutonomyUserNote {
    pub topic: String,
    pub note: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionContextState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_summary: Option<String>,
    #[serde(default)]
    pub compacted_message_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_compaction_ms: Option<u64>,
    #[serde(default)]
    pub last_estimated_tokens: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAutonomyState {
    #[serde(default)]
    pub cycle_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cycle_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    #[serde(default)]
    pub idle_streak: u32,
    #[serde(default)]
    pub recent_failure_count: u32,
    #[serde(default)]
    pub drives: SessionAutonomyDrives,
    #[serde(default)]
    pub chemistry: SessionAutonomyChemistry,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interests: Vec<SessionAutonomyInterest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_user_notes: Vec<SessionAutonomyUserNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_initiative_topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_initiative_kind: Option<SessionAutonomyInitiativeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_subagent: Option<SessionAutonomySubagent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_research_topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_research_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: SessionRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "SessionVisibility::is_public")]
    pub visibility: SessionVisibility,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<SessionToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub autonomy: SessionAutonomyState,
    #[serde(default)]
    pub context: SessionContextState,
    pub messages: Vec<SessionMessage>,
}

#[allow(dead_code)]
impl AgentSession {
    pub fn new(id: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            id: id.into(),
            created_at_ms: now,
            updated_at_ms: now,
            autonomy: SessionAutonomyState::default(),
            context: SessionContextState::default(),
            messages: Vec::new(),
        }
    }

    pub fn push_user_message(&mut self, content: impl Into<String>) {
        self.push_user_message_with_visibility(content, SessionVisibility::Public);
    }

    pub fn push_internal_user_message(&mut self, content: impl Into<String>) {
        self.push_user_message_with_visibility(content, SessionVisibility::Internal);
    }

    pub fn push_user_message_with_visibility(
        &mut self,
        content: impl Into<String>,
        visibility: SessionVisibility,
    ) {
        let content = content.into();
        self.messages.push(SessionMessage {
            role: SessionRole::User,
            content: content.clone(),
            visibility,
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
        if visibility == SessionVisibility::Public {
            self.autonomy.last_user_message_ms = Some(now_ms());
            self.autonomy.last_user_message = Some(content);
        }
        self.touch();
    }

    pub fn push_assistant_message(&mut self, content: impl Into<String>) {
        self.push_assistant_message_with_visibility(content, SessionVisibility::Public);
    }

    pub fn push_internal_assistant_message(&mut self, content: impl Into<String>) {
        self.push_assistant_message_with_visibility(content, SessionVisibility::Internal);
    }

    pub fn push_assistant_message_with_visibility(
        &mut self,
        content: impl Into<String>,
        visibility: SessionVisibility,
    ) {
        self.messages.push(SessionMessage {
            role: SessionRole::Assistant,
            content: content.into(),
            visibility,
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
        self.touch();
    }

    pub fn push_assistant_tool_calls(
        &mut self,
        content: impl Into<String>,
        tool_calls: &[ModelToolCall],
    ) {
        self.push_assistant_tool_calls_with_visibility(
            content,
            tool_calls,
            SessionVisibility::Public,
        );
    }

    pub fn push_assistant_tool_calls_with_visibility(
        &mut self,
        content: impl Into<String>,
        tool_calls: &[ModelToolCall],
        visibility: SessionVisibility,
    ) {
        self.messages.push(SessionMessage {
            role: SessionRole::Assistant,
            content: content.into(),
            visibility,
            name: None,
            tool_call_id: None,
            tool_calls: tool_calls
                .iter()
                .map(|call| SessionToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.raw_arguments.clone(),
                })
                .collect(),
        });
        self.touch();
    }

    pub fn push_tool_message(
        &mut self,
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) {
        self.push_tool_message_with_visibility(
            tool_call_id,
            name,
            content,
            SessionVisibility::Public,
        );
    }

    pub fn push_tool_message_with_visibility(
        &mut self,
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        visibility: SessionVisibility,
    ) {
        self.messages.push(SessionMessage {
            role: SessionRole::Tool,
            content: content.into(),
            visibility,
            name: Some(name.into()),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
        });
        self.touch();
    }

    pub fn last_public_user_message(&self) -> Option<&str> {
        self.messages.iter().rev().find_map(|message| {
            (message.role == SessionRole::User && message.visibility == SessionVisibility::Public)
                .then_some(message.content.as_str())
        })
    }

    pub fn last_public_assistant_message(&self) -> Option<&str> {
        self.messages.iter().rev().find_map(|message| {
            (message.role == SessionRole::Assistant
                && message.visibility == SessionVisibility::Public
                && message.tool_calls.is_empty()
                && !message.content.trim().is_empty())
            .then_some(message.content.as_str())
        })
    }

    pub fn build_pending_user_notes_prompt(&self) -> Option<String> {
        if self.autonomy.pending_user_notes.is_empty() {
            return None;
        }

        let mut lines = vec![
            "Before answering the next visible user message, briefly surface these autonomous findings if they are still relevant:".to_string(),
        ];
        for note in self.autonomy.pending_user_notes.iter().take(4) {
            lines.push(format!(
                "- {}: {}",
                compact_excerpt_with_limit(&note.topic, 80),
                compact_excerpt_with_limit(&note.note, 180)
            ));
        }
        lines.push(
            "Then handle the user's actual request normally instead of turning the reply into a research dump."
                .to_string(),
        );
        Some(lines.join("\n"))
    }

    pub fn clear_pending_user_notes(&mut self) {
        if self.autonomy.pending_user_notes.is_empty() {
            return;
        }
        self.autonomy.pending_user_notes.clear();
        self.touch();
    }

    fn touch(&mut self) {
        self.updated_at_ms = now_ms();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn compact_excerpt_with_limit(content: &str, limit: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = normalized.chars().take(limit).collect::<String>();
    if normalized.chars().count() > limit {
        excerpt.push_str("...");
    }
    excerpt
}

#[cfg(test)]
mod tests {
    use super::{AgentSession, SessionAutonomyUserNote, SessionVisibility};

    #[test]
    fn pending_user_notes_build_a_delivery_prompt() {
        let mut session = AgentSession::new("main");
        session.autonomy.pending_user_notes = vec![SessionAutonomyUserNote {
            topic: "context compaction".to_string(),
            note: "The agent should summarize older turns once the context budget is exceeded."
                .to_string(),
            created_at_ms: 1,
        }];

        let prompt = session
            .build_pending_user_notes_prompt()
            .expect("notes should produce a prompt");
        assert!(prompt.contains("context compaction"));

        session.clear_pending_user_notes();
        assert!(session.autonomy.pending_user_notes.is_empty());
    }

    #[test]
    fn internal_messages_do_not_replace_last_public_user_prompt() {
        let mut session = AgentSession::new("main");
        session.push_user_message("public prompt");
        session.push_internal_user_message("internal autonomy prompt");

        assert_eq!(session.last_public_user_message(), Some("public prompt"));
        assert_eq!(session.autonomy.last_user_message.as_deref(), Some("public prompt"));
        assert_eq!(session.messages[1].visibility, SessionVisibility::Internal);
    }
}