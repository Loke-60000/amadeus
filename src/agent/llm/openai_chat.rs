use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::agent::{
    config::AgentRuntimeConfig,
    llm::common::{
        append_text, build_openai_chat_messages, ensure_success, openai_auth_headers,
        openai_chat_tools, parse_arguments, LlmClientConfig, ModelClient, ModelToolCall, ModelTurn,
        TextStreamSink,
    },
    session::SessionMessage,
    tools::ToolDefinition,
};

#[derive(Clone)]
pub struct OpenAiChatClient {
    config: LlmClientConfig,
}

impl OpenAiChatClient {
    pub fn new(config: &AgentRuntimeConfig) -> Result<Self> {
        Ok(Self {
            config: LlmClientConfig::from_runtime(config)?,
        })
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    raw_arguments: String,
}

impl ModelClient for OpenAiChatClient {
    fn complete(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
    ) -> Result<ModelTurn> {
        let url = format!("{}/chat/completions", self.config.api_base);
        let headers = openai_auth_headers(self.config.api_key.as_deref())?;

        let mut payload = serde_json::Map::new();
        payload.insert(
            "model".to_string(),
            Value::String(self.config.model.clone()),
        );
        payload.insert(
            "messages".to_string(),
            Value::Array(build_openai_chat_messages(system_prompt, messages)),
        );
        payload.insert("temperature".to_string(), json!(self.config.temperature));
        payload.insert("stream".to_string(), Value::Bool(false));
        if !tools.is_empty() {
            payload.insert("tools".to_string(), Value::Array(openai_chat_tools(tools)));
            payload.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }

        let response = self
            .config
            .http
            .post(url)
            .headers(headers)
            .json(&Value::Object(payload))
            .send()
            .context("OpenAI chat completion request failed")?;
        let parsed: Value = ensure_success(response, "OpenAI chat completion request")?
            .json()
            .context("failed to parse the OpenAI chat completion response")?;

        let choice = parsed
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .context("OpenAI chat completion response did not contain any choices")?;
        let message = choice
            .get("message")
            .context("OpenAI chat completion response did not contain a message")?;

        let mut assistant_text = String::new();
        match message.get("content") {
            Some(Value::String(text)) => append_text(&mut assistant_text, text),
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        append_text(&mut assistant_text, text);
                    } else if let Some(text) = part.get("value").and_then(Value::as_str) {
                        append_text(&mut assistant_text, text);
                    }
                }
            }
            _ => {}
        }

        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|tool_calls| {
                tool_calls
                    .iter()
                    .filter_map(|tool_call| {
                        let id = tool_call.get("id")?.as_str()?.to_string();
                        let function = tool_call.get("function")?;
                        let name = function.get("name")?.as_str()?.to_string();
                        let raw_arguments = function
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string();
                        Some(ModelToolCall {
                            id,
                            name,
                            arguments: parse_arguments(&raw_arguments),
                            raw_arguments,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(ModelTurn {
            assistant_text,
            tool_calls,
        })
    }

    fn complete_streaming(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
        stream: &mut dyn TextStreamSink,
    ) -> Result<ModelTurn> {
        let url = format!("{}/chat/completions", self.config.api_base);
        let headers = openai_auth_headers(self.config.api_key.as_deref())?;

        let mut payload = serde_json::Map::new();
        payload.insert(
            "model".to_string(),
            Value::String(self.config.model.clone()),
        );
        payload.insert(
            "messages".to_string(),
            Value::Array(build_openai_chat_messages(system_prompt, messages)),
        );
        payload.insert("temperature".to_string(), json!(self.config.temperature));
        payload.insert("stream".to_string(), Value::Bool(true));
        if !tools.is_empty() {
            payload.insert("tools".to_string(), Value::Array(openai_chat_tools(tools)));
            payload.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }

        let response = self
            .config
            .http
            .post(url)
            .headers(headers)
            .json(&Value::Object(payload))
            .send()
            .context("OpenAI chat streaming request failed")?;
        let response = ensure_success(response, "OpenAI chat streaming request")?;

        let mut assistant_text = String::new();
        let mut partial_tool_calls: BTreeMap<usize, PartialToolCall> = BTreeMap::new();
        let reader = BufReader::new(response);
        for line in reader.lines() {
            let line = line.context("failed to read the streamed OpenAI chat response")?;
            let trimmed = line.trim();
            if trimmed.is_empty() || !trimmed.starts_with("data:") {
                continue;
            }

            let data = trimmed.trim_start_matches("data:").trim();
            if data == "[DONE]" {
                break;
            }

            let parsed: Value = serde_json::from_str(data)
                .context("failed to parse a streamed OpenAI chat event")?;
            let Some(choice) = parsed
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
            else {
                continue;
            };
            let Some(delta) = choice.get("delta") else {
                continue;
            };

            match delta.get("content") {
                Some(Value::String(text)) if !text.is_empty() => {
                    assistant_text.push_str(text);
                    stream.on_text_delta(text)?;
                }
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                assistant_text.push_str(text);
                                stream.on_text_delta(text)?;
                            }
                        }
                    }
                }
                _ => {}
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for (fallback_index, tool_call) in tool_calls.iter().enumerate() {
                    let index = tool_call
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize)
                        .unwrap_or(fallback_index);
                    let partial = partial_tool_calls.entry(index).or_default();

                    if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                        partial.id = id.to_string();
                    }

                    if let Some(function) = tool_call.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            partial.name.push_str(name);
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                            partial.raw_arguments.push_str(arguments);
                        }
                    }
                }
            }
        }

        let tool_calls = partial_tool_calls
            .into_iter()
            .map(|(index, partial)| {
                let raw_arguments = if partial.raw_arguments.is_empty() {
                    "{}".to_string()
                } else {
                    partial.raw_arguments
                };
                ModelToolCall {
                    id: if partial.id.is_empty() {
                        format!("openai-chat-call-{}", index + 1)
                    } else {
                        partial.id
                    },
                    name: if partial.name.is_empty() {
                        "tool".to_string()
                    } else {
                        partial.name
                    },
                    arguments: parse_arguments(&raw_arguments),
                    raw_arguments,
                }
            })
            .collect::<Vec<_>>();

        Ok(ModelTurn {
            assistant_text,
            tool_calls,
        })
    }
}
