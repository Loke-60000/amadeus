use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::{
    config::AgentRuntimeConfig,
    llm::common::{
        append_text, ensure_success, make_tool_call, openai_auth_headers, openai_response_tools,
        parse_arguments, LlmClientConfig, ModelClient, ModelTurn,
    },
    session::{SessionMessage, SessionRole},
    tools::ToolDefinition,
};

#[derive(Clone)]
pub struct OpenAiResponsesClient {
    config: LlmClientConfig,
}

impl OpenAiResponsesClient {
    pub fn new(config: &AgentRuntimeConfig) -> Result<Self> {
        Ok(Self {
            config: LlmClientConfig::from_runtime(config)?,
        })
    }
}

impl ModelClient for OpenAiResponsesClient {
    fn complete(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
    ) -> Result<ModelTurn> {
        let url = format!("{}/responses", self.config.api_base);
        let headers = openai_auth_headers(self.config.api_key.as_deref())?;

        let mut payload = serde_json::Map::new();
        payload.insert(
            "model".to_string(),
            Value::String(self.config.model.clone()),
        );
        payload.insert(
            "instructions".to_string(),
            Value::String(system_prompt.to_string()),
        );
        payload.insert(
            "input".to_string(),
            Value::Array(build_responses_input(messages)),
        );
        payload.insert("temperature".to_string(), json!(self.config.temperature));
        payload.insert(
            "max_output_tokens".to_string(),
            json!(self.config.max_output_tokens),
        );
        if !tools.is_empty() {
            payload.insert(
                "tools".to_string(),
                Value::Array(openai_response_tools(tools)),
            );
            payload.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }

        let response = self
            .config
            .http
            .post(url)
            .headers(headers)
            .json(&Value::Object(payload))
            .send()
            .context("OpenAI responses request failed")?;
        let parsed: Value = ensure_success(response, "OpenAI responses request")?
            .json()
            .context("failed to parse the OpenAI responses response")?;

        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();

        if let Some(items) = parsed.get("output").and_then(Value::as_array) {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        if let Some(content) = item.get("content").and_then(Value::as_array) {
                            for block in content {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    append_text(&mut assistant_text, text);
                                }
                            }
                        }
                    }
                    Some("function_call") => {
                        let id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("openai-response-call")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .map(parse_arguments)
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        tool_calls.push(make_tool_call(id, name, arguments));
                    }
                    Some("output_text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            append_text(&mut assistant_text, text);
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(ModelTurn {
            assistant_text,
            tool_calls,
        })
    }
}

fn build_responses_input(messages: &[SessionMessage]) -> Vec<Value> {
    let mut input = Vec::new();

    for message in messages {
        match message.role {
            SessionRole::User => input.push(json!({
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": message.content,
                    }
                ]
            })),
            SessionRole::Assistant => {
                if !message.content.trim().is_empty() {
                    input.push(json!({
                        "role": "assistant",
                        "content": [
                            {
                                "type": "output_text",
                                "text": message.content,
                            }
                        ]
                    }));
                }
                for tool_call in &message.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": tool_call.id,
                        "name": tool_call.name,
                        "arguments": tool_call.arguments,
                    }));
                }
            }
            SessionRole::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id,
                "output": message.content,
            })),
        }
    }

    input
}
