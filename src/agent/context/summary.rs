use crate::agent::session::{SessionMessage, SessionRole, SessionVisibility};

const ITEM_LIMIT: usize = 4;
const EXCERPT_LIMIT: usize = 180;

pub fn build_compacted_summary(messages: &[SessionMessage], max_chars: usize) -> Option<String> {
    if messages.is_empty() {
        return None;
    }

    let mut earlier_user_requests = Vec::new();
    let mut earlier_assistant_outcomes = Vec::new();
    let mut internal_autonomy = Vec::new();
    let mut tool_failures = Vec::new();

    for message in messages {
        match message.role {
            SessionRole::User => {
                if earlier_user_requests.len() < ITEM_LIMIT {
                    earlier_user_requests.push(compact_excerpt(&message.content));
                }
            }
            SessionRole::Assistant => {
                if message.visibility == SessionVisibility::Internal {
                    if internal_autonomy.len() < ITEM_LIMIT && !message.content.trim().is_empty() {
                        internal_autonomy.push(compact_excerpt(&message.content));
                    }
                } else if message.tool_calls.is_empty()
                    && !message.content.trim().is_empty()
                    && earlier_assistant_outcomes.len() < ITEM_LIMIT
                {
                    earlier_assistant_outcomes.push(compact_excerpt(&message.content));
                }
            }
            SessionRole::Tool => {
                if tool_failures.len() < ITEM_LIMIT && tool_message_counts_as_failure(message) {
                    tool_failures.push(compact_excerpt(&message.content));
                }
            }
        }
    }

    let mut lines = vec![format!("{} earlier messages were compacted.", messages.len())];
    if !earlier_user_requests.is_empty() {
        lines.push("Earlier user requests:".to_string());
        for item in earlier_user_requests {
            lines.push(format!("- {item}"));
        }
    }
    if !earlier_assistant_outcomes.is_empty() {
        lines.push("Earlier assistant outcomes:".to_string());
        for item in earlier_assistant_outcomes {
            lines.push(format!("- {item}"));
        }
    }
    if !internal_autonomy.is_empty() {
        lines.push("Earlier internal autonomy context:".to_string());
        for item in internal_autonomy {
            lines.push(format!("- {item}"));
        }
    }
    if !tool_failures.is_empty() {
        lines.push("Recent tool/runtime failures inside compacted history:".to_string());
        for item in tool_failures {
            lines.push(format!("- {item}"));
        }
    }

    let mut summary = lines.join("\n");
    if summary.chars().count() > max_chars {
        summary = compact_excerpt_with_limit(&summary, max_chars);
    }
    Some(summary)
}

fn compact_excerpt(content: &str) -> String {
    compact_excerpt_with_limit(content, EXCERPT_LIMIT)
}

fn compact_excerpt_with_limit(content: &str, limit: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = normalized.chars().take(limit).collect::<String>();
    if normalized.chars().count() > limit {
        excerpt.push_str("...");
    }
    excerpt
}

pub(crate) fn tool_message_counts_as_failure(message: &SessionMessage) -> bool {
    let parsed = serde_json::from_str::<serde_json::Value>(&message.content).ok();
    let error = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(serde_json::Value::as_str);

    if let Some(error) = error
        && is_expected_tool_rejection(message.name.as_deref(), error)
    {
        return false;
    }

    match parsed
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(serde_json::Value::as_bool)
    {
        Some(ok) => !ok,
        None => message.content.to_ascii_lowercase().contains("error"),
    }
}

fn is_expected_tool_rejection(tool_name: Option<&str>, error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();

    if normalized.contains("private to the agent runtime")
        || normalized.contains("escapes the workspace root")
        || normalized.contains("is not a directory")
        || normalized.contains("is not a file")
    {
        return true;
    }

    if matches!(tool_name, Some("run_command"))
        && (normalized.contains("command denied by allowlist policy")
            || normalized.contains("command denied by the operator")
            || normalized.contains("shell execution is disabled")
            || normalized.contains("not in the allowlist"))
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use crate::agent::session::{SessionMessage, SessionRole, SessionVisibility};

    use super::{build_compacted_summary, tool_message_counts_as_failure};

    #[test]
    fn expected_tool_rejections_do_not_count_as_failures() {
        let message = SessionMessage {
            role: SessionRole::Tool,
            content: r#"{"ok":false,"error":".amadeus/sessions/main.json is not a directory"}"#
                .to_string(),
            visibility: SessionVisibility::Internal,
            name: Some("list_dir".to_string()),
            tool_call_id: Some("call-1".to_string()),
            tool_calls: Vec::new(),
        };

        assert!(!tool_message_counts_as_failure(&message));
    }

    #[test]
    fn compacted_summary_omits_benign_tool_rejections() {
        let summary = build_compacted_summary(
            &[SessionMessage {
                role: SessionRole::Tool,
                content: r#"{"ok":false,"error":".amadeus/sessions/main.json is private to the agent runtime"}"#
                    .to_string(),
                visibility: SessionVisibility::Internal,
                name: Some("read_file".to_string()),
                tool_call_id: Some("call-2".to_string()),
                tool_calls: Vec::new(),
            }],
            800,
        )
        .expect("summary");

        assert!(!summary.contains("Recent tool/runtime failures inside compacted history"));
    }
}