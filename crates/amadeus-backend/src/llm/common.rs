use anyhow::{bail, Context, Result};
use reqwest::{
    blocking::{Client, Response},
    header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE},
};
use serde_json::{json, Value};

use crate::{
    config::AgentRuntimeConfig,
    session::{SessionMessage, SessionRole},
    tools::ToolDefinition,
};

#[derive(Debug, Clone)]
pub struct ModelToolCall {
    pub id: String,
    pub name: String,
    pub raw_arguments: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct ModelTurn {
    pub assistant_text: String,
    pub tool_calls: Vec<ModelToolCall>,
}

pub trait TextStreamSink {
    fn on_text_delta(&mut self, delta: &str) -> Result<()>;

    fn on_tool_call_round(&mut self, _tool_calls: &[ModelToolCall]) -> Result<()> {
        Ok(())
    }
}

pub trait ModelClient {
    fn complete(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
    ) -> Result<ModelTurn>;

    fn complete_streaming(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
        stream: &mut dyn TextStreamSink,
    ) -> Result<ModelTurn> {
        let turn = self.complete(system_prompt, messages, tools)?;
        // Emit text even when tool calls are present — this is the narration
        // Kurisu speaks before/between tool uses ("let me check that file…").
        if !turn.assistant_text.is_empty() {
            stream.on_text_delta(&turn.assistant_text)?;
        }
        Ok(turn)
    }
}

#[derive(Clone)]
pub struct LlmClientConfig {
    pub http: Client,
    pub api_base: String,
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: f32,
    pub max_output_tokens: usize,
}

impl LlmClientConfig {
    pub fn from_runtime(config: &AgentRuntimeConfig) -> Result<Self> {
        let model = config
            .model
            .clone()
            .context("no model configured; set --model or AMADEUS_AGENT_MODEL")?;
        let http = Client::builder()
            .build()
            .context("failed to build the HTTP client")?;

        Ok(Self {
            http,
            api_base: config.api_base.trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
            model,
            temperature: config.temperature,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

pub fn openai_auth_headers(api_key: Option<&str>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(api_key) = api_key {
        let value = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .context("failed to encode the API key header")?;
        headers.insert(AUTHORIZATION, value);
    }
    Ok(headers)
}

pub fn ensure_success(response: Response, label: &str) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().unwrap_or_default();
    bail!("{label} failed with {status}: {body}");
}

pub fn openai_chat_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }
            })
        })
        .collect()
}

pub fn openai_response_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect()
}

pub fn anthropic_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect()
}

pub fn gemini_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    if tools.is_empty() {
        return Vec::new();
    }

    vec![json!({
        "functionDeclarations": tools.iter().map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        }).collect::<Vec<_>>()
    })]
}

pub fn build_openai_chat_messages(system_prompt: &str, messages: &[SessionMessage]) -> Vec<Value> {
    let mut request_messages = vec![json!({
        "role": "system",
        "content": system_prompt,
    })];

    for message in messages {
        match message.role {
            SessionRole::User => request_messages.push(json!({
                "role": "user",
                "content": message.content,
            })),
            SessionRole::Assistant => {
                if message.tool_calls.is_empty() {
                    request_messages.push(json!({
                        "role": "assistant",
                        "content": message.content,
                    }));
                } else {
                    let tool_calls = message
                        .tool_calls
                        .iter()
                        .map(|tool_call| {
                            json!({
                                "id": tool_call.id,
                                "type": "function",
                                "function": {
                                    "name": tool_call.name,
                                    "arguments": tool_call.arguments,
                                }
                            })
                        })
                        .collect::<Vec<_>>();

                    let mut payload = serde_json::Map::new();
                    payload.insert("role".to_string(), Value::String("assistant".to_string()));
                    if !message.content.trim().is_empty() {
                        payload.insert(
                            "content".to_string(),
                            Value::String(message.content.clone()),
                        );
                    } else {
                        payload.insert("content".to_string(), Value::Null);
                    }
                    payload.insert("tool_calls".to_string(), Value::Array(tool_calls));
                    request_messages.push(Value::Object(payload));
                }
            }
            SessionRole::Tool => request_messages.push(json!({
                "role": "tool",
                "content": message.content,
                "tool_call_id": message.tool_call_id,
                "name": message.name,
            })),
        }
    }

    request_messages
}

pub fn build_ollama_messages(system_prompt: &str, messages: &[SessionMessage]) -> Vec<Value> {
    let mut request_messages = vec![json!({
        "role": "system",
        "content": system_prompt,
    })];

    for message in messages {
        match message.role {
            SessionRole::User => request_messages.push(json!({
                "role": "user",
                "content": message.content,
            })),
            SessionRole::Assistant => {
                if message.tool_calls.is_empty() {
                    request_messages.push(json!({
                        "role": "assistant",
                        "content": message.content,
                    }));
                } else {
                    let tool_calls = message
                        .tool_calls
                        .iter()
                        .map(|tool_call| {
                            json!({
                                "function": {
                                    "name": tool_call.name,
                                    "arguments": parse_arguments_object(&tool_call.arguments),
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    request_messages.push(json!({
                        "role": "assistant",
                        "content": message.content,
                        "tool_calls": tool_calls,
                    }));
                }
            }
            SessionRole::Tool => request_messages.push(json!({
                "role": "tool",
                "content": message.content,
                "name": message.name,
            })),
        }
    }

    request_messages
}

pub fn parse_arguments(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

pub fn parse_arguments_object(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(object)) => Value::Object(object),
        Ok(value) => json!({ "value": value }),
        Err(_) => json!({ "raw": raw }),
    }
}

pub fn raw_arguments_from_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

pub fn make_tool_call(id: String, name: String, value: Value) -> ModelToolCall {
    ModelToolCall {
        id,
        name,
        raw_arguments: raw_arguments_from_value(&value),
        arguments: value,
    }
}

pub fn append_text(target: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(trimmed);
}
