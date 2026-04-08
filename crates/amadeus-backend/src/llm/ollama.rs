use std::io::{BufRead, BufReader};

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::{
    config::AgentRuntimeConfig,
    llm::common::{
        append_text, build_ollama_messages, ensure_success, make_tool_call, openai_chat_tools,
        LlmClientConfig, ModelClient, ModelTurn, TextStreamSink,
    },
    session::SessionMessage,
    tools::ToolDefinition,
};

#[derive(Clone)]
pub struct OllamaClient {
    config: LlmClientConfig,
}

impl OllamaClient {
    pub fn new(config: &AgentRuntimeConfig) -> Result<Self> {
        Ok(Self {
            config: LlmClientConfig::from_runtime(config)?,
        })
    }
}

impl ModelClient for OllamaClient {
    fn complete(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
    ) -> Result<ModelTurn> {
        let url = format!("{}/api/chat", self.config.api_base);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let mut payload = serde_json::Map::new();
        payload.insert(
            "model".to_string(),
            Value::String(self.config.model.clone()),
        );
        payload.insert(
            "messages".to_string(),
            Value::Array(build_ollama_messages(system_prompt, messages)),
        );
        payload.insert("stream".to_string(), Value::Bool(false));
        payload.insert(
            "options".to_string(),
            json!({
                "temperature": self.config.temperature,
            }),
        );
        if !tools.is_empty() {
            payload.insert("tools".to_string(), Value::Array(openai_chat_tools(tools)));
        }

        let response = self
            .config
            .http
            .post(url)
            .headers(headers)
            .json(&Value::Object(payload))
            .send()
            .context("Ollama request failed")?;
        let parsed: Value = ensure_success(response, "Ollama request")?
            .json()
            .context("failed to parse the Ollama response")?;

        let message = parsed
            .get("message")
            .context("Ollama response did not contain a message")?;
        let mut assistant_text = String::new();
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            append_text(&mut assistant_text, text);
        }

        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .enumerate()
                    .filter_map(|(index, tool_call)| {
                        let function = tool_call.get("function")?;
                        let name = function.get("name")?.as_str()?.to_string();
                        let arguments = function
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        Some(make_tool_call(
                            format!("ollama-call-{}", index + 1),
                            name,
                            arguments,
                        ))
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
        let url = format!("{}/api/chat", self.config.api_base);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let mut payload = serde_json::Map::new();
        payload.insert(
            "model".to_string(),
            Value::String(self.config.model.clone()),
        );
        payload.insert(
            "messages".to_string(),
            Value::Array(build_ollama_messages(system_prompt, messages)),
        );
        payload.insert("stream".to_string(), Value::Bool(true));
        payload.insert(
            "options".to_string(),
            json!({
                "temperature": self.config.temperature,
            }),
        );
        if !tools.is_empty() {
            payload.insert("tools".to_string(), Value::Array(openai_chat_tools(tools)));
        }

        let response = self
            .config
            .http
            .post(url)
            .headers(headers)
            .json(&Value::Object(payload))
            .send()
            .context("Ollama streaming request failed")?;
        let response = ensure_success(response, "Ollama streaming request")?;

        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();
        let reader = BufReader::new(response);
        for line in reader.lines() {
            let line = line.context("failed to read the streamed Ollama response")?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parsed: Value = serde_json::from_str(trimmed)
                .context("failed to parse a streamed Ollama response chunk")?;
            let Some(message) = parsed.get("message") else {
                continue;
            };

            if let Some(text) = message.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    assistant_text.push_str(text);
                    stream.on_text_delta(text)?;
                }
            }

            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                let next_calls = calls
                    .iter()
                    .enumerate()
                    .filter_map(|(index, tool_call)| {
                        let function = tool_call.get("function")?;
                        let name = function.get("name")?.as_str()?.to_string();
                        let arguments = function
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        Some(make_tool_call(
                            format!("ollama-call-{}", index + 1),
                            name,
                            arguments,
                        ))
                    })
                    .collect::<Vec<_>>();
                if !next_calls.is_empty() {
                    tool_calls = next_calls;
                }
            }
        }

        Ok(ModelTurn {
            assistant_text,
            tool_calls,
        })
    }
}
