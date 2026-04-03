use crate::agent::{
    config::AutonomyConfig,
    session::{AgentSession, SessionAutonomyUserNote},
};

const TOPIC_EXCERPT_LIMIT: usize = 96;
const NOTE_EXCERPT_LIMIT: usize = 220;

pub fn research_topic_hints(session: &AgentSession, config: &AutonomyConfig) -> Vec<String> {
    configured_or_fallback_topics(session, config)
}

pub fn extract_user_notes(
    reply: &str,
    fallback_topic: Option<&str>,
    max_notes: usize,
) -> Vec<SessionAutonomyUserNote> {
    let mut notes = Vec::new();

    for line in reply.lines() {
        let Some(raw_note) = line.trim().strip_prefix("USER_NOTE:") else {
            continue;
        };

        let (topic, note) = raw_note
            .split_once("::")
            .map(|(topic, note)| (topic.trim(), note.trim()))
            .unwrap_or_else(|| (fallback_topic.unwrap_or("autonomy research"), raw_note.trim()));

        if topic.is_empty() || note.is_empty() {
            continue;
        }

        notes.push(SessionAutonomyUserNote {
            topic: compact_excerpt(topic, TOPIC_EXCERPT_LIMIT),
            note: compact_excerpt(note, NOTE_EXCERPT_LIMIT),
            created_at_ms: now_ms(),
        });

        if notes.len() >= max_notes.max(1) {
            break;
        }
    }

    notes
}

pub fn retain_pending_user_notes(
    existing: &mut Vec<SessionAutonomyUserNote>,
    new_notes: Vec<SessionAutonomyUserNote>,
    max_notes: usize,
) {
    for note in new_notes {
        let duplicate = existing.iter().any(|current| {
            current.topic.eq_ignore_ascii_case(&note.topic) && current.note == note.note
        });
        if !duplicate {
            existing.push(note);
        }
    }

    let keep = max_notes.max(1);
    if existing.len() > keep {
        let remove = existing.len() - keep;
        existing.drain(0..remove);
    }
}

fn configured_or_fallback_topics(session: &AgentSession, config: &AutonomyConfig) -> Vec<String> {
    if !config.research.topics.is_empty() {
        return config.research.topics.clone();
    }

    let mut topics = Vec::new();
    if let Some(last_user) = session.last_public_user_message() {
        topics.push(format!(
            "the user's recent ongoing interests: {}",
            compact_excerpt(last_user, TOPIC_EXCERPT_LIMIT)
        ));
    }
    topics.push("agent continuity, self-maintenance, and long-horizon autonomy in this workspace".to_string());
    topics.push("context compaction, token budgeting, and memory discipline for this agent".to_string());
    topics
}

fn compact_excerpt(content: &str, limit: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = normalized.chars().take(limit).collect::<String>();
    if normalized.chars().count() > limit {
        excerpt.push_str("...");
    }
    excerpt
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}