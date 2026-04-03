mod anthropic;
mod common;
mod gemini;
mod ollama;
mod openai_chat;
mod openai_responses;

use anyhow::Result;

use crate::agent::config::{AgentRuntimeConfig, LlmProvider};

pub use common::{ModelClient, ModelToolCall, TextStreamSink};

pub fn build_model_client(config: &AgentRuntimeConfig) -> Result<Box<dyn ModelClient>> {
    match config.provider {
        LlmProvider::OpenAiChat => Ok(Box::new(openai_chat::OpenAiChatClient::new(config)?)),
        LlmProvider::OpenAiResponses => Ok(Box::new(openai_responses::OpenAiResponsesClient::new(
            config,
        )?)),
        LlmProvider::Anthropic => Ok(Box::new(anthropic::AnthropicClient::new(config)?)),
        LlmProvider::Gemini => Ok(Box::new(gemini::GeminiClient::new(config)?)),
        LlmProvider::Ollama => Ok(Box::new(ollama::OllamaClient::new(config)?)),
    }
}
