use crate::session::SessionMessage;

pub fn estimate_text_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4)
    }
}

pub fn estimate_message_tokens(message: &SessionMessage) -> usize {
    let mut total = 6 + estimate_text_tokens(&message.content);
    if let Some(name) = &message.name {
        total += estimate_text_tokens(name);
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        total += estimate_text_tokens(tool_call_id);
    }
    for tool_call in &message.tool_calls {
        total += 8;
        total += estimate_text_tokens(&tool_call.name);
        total += estimate_text_tokens(&tool_call.arguments);
        total += estimate_text_tokens(&tool_call.id);
    }
    total
}

pub fn estimate_messages_tokens(messages: &[SessionMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}