use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::{
    config::AgentRuntimeConfig,
    llm::common::{
        anthropic_tools, append_text, ensure_success, make_tool_call, parse_arguments_object,
        LlmClientConfig, ModelClient, ModelTurn,
    },
    session::{SessionMessage, SessionRole},
    tools::ToolDefinition,
};

const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const X_API_KEY: &str = "x-api-key";

#[derive(Clone)]
pub struct AnthropicClient {
    config: LlmClientConfig,
}

impl AnthropicClient {
    pub fn new(config: &AgentRuntimeConfig) -> Result<Self> {
        Ok(Self {
            config: LlmClientConfig::from_runtime(config)?,
        })
    }
}

impl ModelClient for AnthropicClient {
    fn complete(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
    ) -> Result<ModelTurn> {
        let url = format!("{}/messages", self.config.api_base);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static(ANTHROPIC_VERSION_HEADER),
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        if let Some(api_key) = &self.config.api_key {
            headers.insert(
                HeaderName::from_static(X_API_KEY),
                HeaderValue::from_str(api_key).context("failed to encode the Anthropic API key")?,
            );
        }

        let mut payload = serde_json::Map::new();
        payload.insert(
            "model".to_string(),
            Value::String(self.config.model.clone()),
        );
        payload.insert(
            "system".to_string(),
            Value::String(system_prompt.to_string()),
        );
        payload.insert(
            "messages".to_string(),
            Value::Array(build_anthropic_messages(messages)),
        );
        payload.insert("temperature".to_string(), json!(self.config.temperature));
        payload.insert(
            "max_tokens".to_string(),
            json!(self.config.max_output_tokens),
        );
        if !tools.is_empty() {
            payload.insert("tools".to_string(), Value::Array(anthropic_tools(tools)));
        }

        let response = self
            .config
            .http
            .post(url)
            .headers(headers)
            .json(&Value::Object(payload))
            .send()
            .context("Anthropic request failed")?;
        let parsed: Value = ensure_success(response, "Anthropic request")?
            .json()
            .context("failed to parse the Anthropic response")?;

        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();
        if let Some(content) = parsed.get("content").and_then(Value::as_array) {
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            append_text(&mut assistant_text, text);
                        }
                    }
                    Some("tool_use") => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("anthropic-tool-call")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let arguments = block
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default()));
                        tool_calls.push(make_tool_call(id, name, arguments));
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

fn build_anthropic_messages(messages: &[SessionMessage]) -> Vec<Value> {
    let mut converted = Vec::new();

    for message in messages {
        match message.role {
            SessionRole::User => converted.push(json!({
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": message.content,
                    }
                ]
            })),
            SessionRole::Assistant => {
                let mut content = Vec::new();
                if !message.content.trim().is_empty() {
                    content.push(json!({
                        "type": "text",
                        "text": message.content,
                    }));
                }
                for tool_call in &message.tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": tool_call.id,
                        "name": tool_call.name,
                        "input": parse_arguments_object(&tool_call.arguments),
                    }));
                }
                converted.push(json!({
                    "role": "assistant",
                    "content": content,
                }));
            }
            SessionRole::Tool => converted.push(json!({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": message.tool_call_id,
                        "content": message.content,
                    }
                ]
            })),
        }
    }

    converted
}
