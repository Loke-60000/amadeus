mod budget;
mod summary;

use crate::session::{AgentSession, SessionMessage, SessionRole, SessionVisibility};

use budget::{estimate_messages_tokens, estimate_text_tokens};
pub(crate) use summary::tool_message_counts_as_failure;
use summary::build_compacted_summary;

const SUMMARY_RESERVE_RATIO_NUMERATOR: usize = 1;
const SUMMARY_RESERVE_RATIO_DENOMINATOR: usize = 4;
const MIN_SUMMARY_RESERVE_TOKENS: usize = 160;
const DEFAULT_MESSAGE_CHAR_CAP: usize = 2_400;
const DEFAULT_TOOL_RESULT_CHAR_CAP: usize = 1_600;
const DEFAULT_TOOL_ARGUMENT_CHAR_CAP: usize = 720;
const SUMMARY_MESSAGE_PREFIX: &str = "Compacted session summary:";

#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct PreparedContext {
    pub messages: Vec<SessionMessage>,
    pub estimated_tokens: usize,
    pub compacted: bool,
}

pub fn prepare_model_context(
    session: &mut AgentSession,
    system_prompt: &str,
    max_context_tokens: usize,
) -> PreparedContext {
    let system_tokens = estimate_text_tokens(system_prompt);
    let full_estimate = system_tokens + estimate_messages_tokens(&session.messages);
    session.context.last_estimated_tokens = full_estimate;

    if full_estimate <= max_context_tokens {
        session.context.compacted_summary = None;
        session.context.compacted_message_count = 0;
        session.context.last_compaction_ms = None;
        return PreparedContext {
            messages: session.messages.clone(),
            estimated_tokens: full_estimate,
            compacted: false,
        };
    }

    let turn_ranges = split_turn_ranges(&session.messages);
    if turn_ranges.is_empty() {
        return PreparedContext {
            messages: Vec::new(),
            estimated_tokens: system_tokens,
            compacted: false,
        };
    }

    let summary_reserve_tokens = ((max_context_tokens * SUMMARY_RESERVE_RATIO_NUMERATOR)
        / SUMMARY_RESERVE_RATIO_DENOMINATOR)
        .max(MIN_SUMMARY_RESERVE_TOKENS)
        .min(max_context_tokens.saturating_sub(system_tokens));
    let recent_budget = max_context_tokens
        .saturating_sub(system_tokens)
        .saturating_sub(summary_reserve_tokens);

    let mut keep_start = turn_ranges.len() - 1;
    let mut recent_messages = session.messages[turn_ranges[keep_start].0..turn_ranges[keep_start].1]
        .to_vec();
    let mut recent_tokens = estimate_messages_tokens(&recent_messages);

    while keep_start > 0 {
        let previous_range = turn_ranges[keep_start - 1];
        let previous_turn = &session.messages[previous_range.0..previous_range.1];
        let previous_tokens = estimate_messages_tokens(previous_turn);
        if recent_tokens + previous_tokens > recent_budget.max(previous_tokens) {
            break;
        }

        keep_start -= 1;
        let mut merged = previous_turn.to_vec();
        merged.extend(recent_messages);
        recent_messages = merged;
        recent_tokens += previous_tokens;
    }

    let omitted_messages = if keep_start > 0 {
        session.messages[..turn_ranges[keep_start].0].to_vec()
    } else {
        Vec::new()
    };

    let summary_text = if omitted_messages.is_empty() {
        None
    } else {
        build_compacted_summary(&omitted_messages, summary_reserve_tokens.saturating_mul(4))
    };

    let mut prepared_messages = Vec::new();
    if let Some(summary_text) = summary_text.clone() {
        prepared_messages.push(SessionMessage {
            role: SessionRole::Assistant,
            content: format!("{SUMMARY_MESSAGE_PREFIX}\n{summary_text}"),
            visibility: SessionVisibility::Internal,
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
    }
    prepared_messages.extend(recent_messages);

    fit_messages_to_budget(&mut prepared_messages, system_tokens, max_context_tokens);

    let final_estimate = system_tokens + estimate_messages_tokens(&prepared_messages);
    session.context.compacted_summary = summary_text;
    session.context.compacted_message_count = omitted_messages.len();
    session.context.last_compaction_ms = Some(now_ms());
    session.context.last_estimated_tokens = final_estimate;

    PreparedContext {
        messages: prepared_messages,
        estimated_tokens: final_estimate,
        compacted: true,
    }
}

fn split_turn_ranges(messages: &[SessionMessage]) -> Vec<(usize, usize)> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut starts = vec![0usize];
    for (index, message) in messages.iter().enumerate().skip(1) {
        if message.role == SessionRole::User {
            starts.push(index);
        }
    }

    let mut ranges = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(messages.len());
        ranges.push((*start, end));
    }
    ranges
}

fn truncate_messages_for_budget(messages: &mut [SessionMessage]) {
    truncate_messages_with_cap(
        messages,
        DEFAULT_MESSAGE_CHAR_CAP,
        DEFAULT_TOOL_RESULT_CHAR_CAP,
        DEFAULT_TOOL_ARGUMENT_CHAR_CAP,
    );
}

fn truncate_messages_with_cap(
    messages: &mut [SessionMessage],
    message_char_cap: usize,
    tool_result_char_cap: usize,
    tool_argument_char_cap: usize,
) {
    for message in messages {
        let char_cap = if message.role == SessionRole::Tool {
            tool_result_char_cap
        } else if message.content.starts_with(SUMMARY_MESSAGE_PREFIX) {
            tool_result_char_cap
        } else {
            message_char_cap
        };
        truncate_string(&mut message.content, char_cap);
        for tool_call in &mut message.tool_calls {
            truncate_string(&mut tool_call.arguments, tool_argument_char_cap);
        }
    }
}

fn fit_messages_to_budget(
    messages: &mut Vec<SessionMessage>,
    system_tokens: usize,
    max_context_tokens: usize,
) {
    truncate_messages_for_budget(messages);

    while system_tokens + estimate_messages_tokens(messages) > max_context_tokens && messages.len() > 1 {
        let drop_index = if has_summary_message(messages) && messages.len() > 2 {
            1
        } else {
            0
        };
        messages.remove(drop_index);
    }

    let available_chars = max_context_tokens
        .saturating_sub(system_tokens)
        .saturating_mul(4)
        .max(96);
    let mut message_char_cap = available_chars / messages.len().max(1);
    let mut tool_result_char_cap = message_char_cap;
    let mut tool_argument_char_cap = (message_char_cap / 3).max(64);

    while system_tokens + estimate_messages_tokens(messages) > max_context_tokens {
        truncate_messages_with_cap(
            messages,
            message_char_cap.max(96),
            tool_result_char_cap.max(96),
            tool_argument_char_cap.max(48),
        );

        if system_tokens + estimate_messages_tokens(messages) <= max_context_tokens {
            break;
        }

        if message_char_cap <= 96 && tool_result_char_cap <= 96 && tool_argument_char_cap <= 48 {
            break;
        }

        message_char_cap = (message_char_cap / 2).max(96);
        tool_result_char_cap = (tool_result_char_cap / 2).max(96);
        tool_argument_char_cap = (tool_argument_char_cap / 2).max(48);
    }
}

fn truncate_string(value: &mut String, max_chars: usize) {
    if value.chars().count() <= max_chars {
        return;
    }

    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut truncated = normalized.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    *value = truncated;
}

fn has_summary_message(messages: &[SessionMessage]) -> bool {
    messages.first().is_some_and(|message| {
        message.role == SessionRole::Assistant
            && message.visibility == SessionVisibility::Internal
            && message.content.starts_with(SUMMARY_MESSAGE_PREFIX)
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crate::session::{AgentSession, SessionMessage, SessionRole, SessionVisibility};

    use super::prepare_model_context;

    #[test]
    fn context_manager_compacts_older_turns() {
        let mut session = AgentSession::new("main");
        session.messages = vec![
            SessionMessage {
                role: SessionRole::User,
                content: "First request ".repeat(120),
                visibility: SessionVisibility::Public,
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
            SessionMessage {
                role: SessionRole::Assistant,
                content: "First answer ".repeat(120),
                visibility: SessionVisibility::Public,
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
            SessionMessage {
                role: SessionRole::User,
                content: "Second request ".repeat(120),
                visibility: SessionVisibility::Public,
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
            SessionMessage {
                role: SessionRole::Assistant,
                content: "Second answer ".repeat(120),
                visibility: SessionVisibility::Public,
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
        ];

        let prepared = prepare_model_context(
            &mut session,
            "system prompt",
            320,
        );

        assert!(prepared.compacted);
        assert!(prepared.estimated_tokens <= 320);
        assert!(session.context.compacted_summary.is_some());
        assert!(!prepared.messages.is_empty());
    }
}