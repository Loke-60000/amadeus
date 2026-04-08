use anyhow::{bail, Result};
use serde_json::json;

use crate::{
    context::prepare_model_context, llm::TextStreamSink, session::SessionVisibility,
};

use super::AgentApp;

impl AgentApp {
    pub(super) fn run_turn_internal(
        &mut self,
        user_input: &str,
        visibility: SessionVisibility,
        mut stream: Option<&mut dyn TextStreamSink>,
        reload_workspace: bool,
    ) -> Result<String> {
        if reload_workspace {
            self.workspace.reload()?;
        }
        let pending_user_notes_prompt = if visibility == SessionVisibility::Public {
            self.session.build_pending_user_notes_prompt()
        } else {
            None
        };
        if let Some(note_prompt) = &pending_user_notes_prompt {
            self.session.push_internal_user_message(note_prompt.clone());
        }
        self.session
            .push_user_message_with_visibility(user_input, visibility);

        let mut final_reply = None;
        for _ in 0..self.config.max_tool_rounds {
            let system_prompt = self.system_prompt()?;
            let prepared_context = prepare_model_context(
                &mut self.session,
                &system_prompt,
                self.config.max_context_tokens,
            );
            let turn = if let Some(stream_sink) = stream.as_deref_mut() {
                self.model.complete_streaming(
                    &system_prompt,
                    &prepared_context.messages,
                    &self.tools.definitions(),
                    stream_sink,
                )?
            } else {
                self.model.complete(
                    &system_prompt,
                    &prepared_context.messages,
                    &self.tools.definitions(),
                )?
            };

            if turn.tool_calls.is_empty() {
                let should_backfill_stream = turn.assistant_text.trim().is_empty();
                let assistant_text = if turn.assistant_text.trim().is_empty() {
                    "I do not have a final reply yet.".to_string()
                } else {
                    turn.assistant_text.trim().to_string()
                };
                if should_backfill_stream {
                    if let Some(stream_sink) = stream.as_deref_mut() {
                        stream_sink.on_text_delta(&assistant_text)?;
                    }
                }
                self.session
                    .push_assistant_message_with_visibility(assistant_text.clone(), visibility);
                if visibility == SessionVisibility::Public && pending_user_notes_prompt.is_some() {
                    self.session.clear_pending_user_notes();
                }
                self.session_store.save(&self.session)?;
                final_reply = Some(assistant_text);
                break;
            }

            if let Some(stream_sink) = stream.as_deref_mut() {
                stream_sink.on_tool_call_round(&turn.tool_calls)?;
            }

            self.session.push_assistant_tool_calls_with_visibility(
                turn.assistant_text,
                &turn.tool_calls,
                visibility,
            );

            for tool_call in turn.tool_calls {
                println!("[tool] {}", tool_call.name);
                let tool_result = match self.tools.invoke(&tool_call.name, tool_call.arguments) {
                    Ok(outcome) => {
                        println!("[tool:{}] {}", tool_call.name, outcome.summary);
                        outcome.to_tool_message()
                    }
                    Err(error) => {
                        println!("[tool:{}] error: {}", tool_call.name, error);
                        serde_json::to_string_pretty(&json!({
                            "ok": false,
                            "error": error.to_string(),
                        }))
                        .unwrap_or_else(|_| {
                            format!("{{\"ok\":false,\"error\":{:?}}}", error.to_string())
                        })
                    }
                };
                self.session.push_tool_message_with_visibility(
                    tool_call.id,
                    tool_call.name,
                    tool_result,
                    visibility,
                );
            }

            self.session_store.save(&self.session)?;
        }

        if let Some(reply) = final_reply {
            return Ok(reply);
        }

        bail!(
            "the model kept requesting tools for {} rounds without producing a final reply",
            self.config.max_tool_rounds
        )
    }
}
