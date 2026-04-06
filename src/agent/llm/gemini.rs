use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::agent::{
    config::AgentRuntimeConfig,
    llm::common::{
        append_text, ensure_success, gemini_tools, make_tool_call, parse_arguments_object,
        LlmClientConfig, ModelClient, ModelTurn,
    },
    session::{SessionMessage, SessionRole},
    tools::ToolDefinition,
};

const X_GOOG_API_KEY: &str = "x-goog-api-key";

#[derive(Clone)]
pub struct GeminiClient {
    config: LlmClientConfig,
}

impl GeminiClient {
    pub fn new(config: &AgentRuntimeConfig) -> Result<Self> {
        Ok(Self {
            config: LlmClientConfig::from_runtime(config)?,
        })
    }
}

impl ModelClient for GeminiClient {
    fn complete(
        &self,
        system_prompt: &str,
        messages: &[SessionMessage],
        tools: &[ToolDefinition],
    ) -> Result<ModelTurn> {
        let model_path = if self.config.model.starts_with("models/") {
            self.config.model.clone()
        } else {
            format!("models/{}", self.config.model)
        };
        let url = format!("{}/{}:generateContent", self.config.api_base, model_path);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(api_key) = &self.config.api_key {
            headers.insert(
                HeaderName::from_static(X_GOOG_API_KEY),
                HeaderValue::from_str(api_key).context("failed to encode the Gemini API key")?,
            );
        }

        let mut payload = serde_json::Map::new();
        payload.insert(
            "systemInstruction".to_string(),
            json!({
                "parts": [
                    {
                        "text": system_prompt,
                    }
                ]
            }),
        );
        payload.insert(
            "contents".to_string(),
            Value::Array(build_gemini_contents(messages)),
        );
        payload.insert(
            "generationConfig".to_string(),
            json!({
                "temperature": self.config.temperature,
                "maxOutputTokens": self.config.max_output_tokens,
            }),
        );
        if !tools.is_empty() {
            payload.insert("tools".to_string(), Value::Array(gemini_tools(tools)));
            payload.insert(
                "toolConfig".to_string(),
                json!({
                    "functionCallingConfig": {
                        "mode": "AUTO",
                    }
                }),
            );
        }

        let response = self
            .config
            .http
            .post(url)
            .headers(headers)
            .json(&Value::Object(payload))
            .send()
            .context("Gemini request failed")?;
        let parsed: Value = ensure_success(response, "Gemini request")?
            .json()
            .context("failed to parse the Gemini response")?;

        let candidate = parsed
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .context("Gemini response did not contain any candidates")?;
        let parts = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .context("Gemini candidate did not contain any parts")?;

        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                append_text(&mut assistant_text, text);
            }
            if let Some(function_call) = part.get("functionCall") {
                let name = function_call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let arguments = function_call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                tool_calls.push(make_tool_call(
                    format!("gemini-call-{}", index + 1),
                    name,
                    arguments,
                ));
            }
        }

        Ok(ModelTurn {
            assistant_text,
            tool_calls,
        })
    }
}

fn build_gemini_contents(messages: &[SessionMessage]) -> Vec<Value> {
    let mut contents = Vec::new();
    for message in messages {
        match message.role {
            SessionRole::User => contents.push(json!({
                "role": "user",
                "parts": [
                    {
                        "text": message.content,
                    }
                ]
            })),
            SessionRole::Assistant => {
                let mut parts = Vec::new();
                if !message.content.trim().is_empty() {
                    parts.push(json!({
                        "text": message.content,
                    }));
                }
                for tool_call in &message.tool_calls {
                    parts.push(json!({
                        "functionCall": {
                            "name": tool_call.name,
                            "args": parse_arguments_object(&tool_call.arguments),
                        }
                    }));
                }
                contents.push(json!({
                    "role": "model",
                    "parts": parts,
                }));
            }
            SessionRole::Tool => contents.push(json!({
                "role": "user",
                "parts": [
                    {
                        "functionResponse": {
                            "name": message.name.clone().unwrap_or_else(|| "tool".to_string()),
                            "response": {
                                "output": message.content,
                            }
                        }
                    }
                ]
            })),
        }
    }
    contents
}
