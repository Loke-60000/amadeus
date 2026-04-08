//! Pluggable conversation backend trait.
//!
//! The desktop client (native overlay + TTS + STT) is the **client layer**.
//! The **backend** is whatever provides the actual LLM reasoning:
//!
//! - [`super::ui::AgentUiService`] — local in-process Amadeus agent (default)
//! - [`ExternalAgentClient`] — remote agent reachable over HTTP (set
//!   `AMADEUS_EXTERNAL_AGENT_URL` to enable)
//!
//! The native viewer holds a `Arc<dyn ConversationBackend>` and never cares which
//! implementation is underneath.

use anyhow::Result;

use crate::llm::TextStreamSink;

/// A single conversation turn request.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub prompt: String,
    pub session_id: Option<String>,
    pub voice_mode: bool,
}

/// The minimal reply returned by any backend.
#[derive(Debug, Clone)]
pub struct TurnResponse {
    pub reply: String,
}

/// A pluggable conversation backend.
///
/// Anything that can accept a text prompt and return a text reply can implement
/// this trait.  The two key implementations are the local [`super::ui::AgentUiService`]
/// and the HTTP-based [`ExternalAgentClient`].
pub trait ConversationBackend: Send + Sync {
    /// Returns `true` once the backend is ready to handle turns without a
    /// blocking initialisation stall.  External clients always return `true`.
    fn is_ready(&self) -> bool;

    /// Reload configuration from disk.  No-op for external clients.
    fn reload_config(&self);

    /// Run a turn to completion and return the full reply.
    fn run_turn(&self, request: TurnRequest) -> Result<TurnResponse>;

    /// Run a turn, calling `sink.on_text_delta` for each incremental chunk.
    /// Returns the complete reply once streaming is done.
    fn run_turn_streaming(
        &self,
        request: TurnRequest,
        sink: &mut dyn TextStreamSink,
    ) -> Result<TurnResponse>;
}

// ── ExternalAgentClient ────────────────────────────────────────────────────────

const ENV_EXTERNAL_AGENT_URL: &str = "AMADEUS_EXTERNAL_AGENT_URL";
const ENV_EXTERNAL_AGENT_KEY: &str = "AMADEUS_EXTERNAL_AGENT_KEY";

/// HTTP client that forwards conversation turns to a remote agent server.
///
/// The remote server is expected to speak the Amadeus turn protocol:
///
/// ```text
/// POST {base_url}/api/agent/turn
/// Content-Type: application/json
/// Authorization: Bearer <api_key>   (optional)
///
/// { "prompt": "...", "sessionId": "...", "voiceMode": false }
///
/// 200 OK
/// { "reply": "..." }
/// ```
///
/// Streaming (`run_turn_streaming`) currently delivers the full reply as a
/// single delta once the response arrives.  A future revision can adopt SSE.
pub struct ExternalAgentClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::blocking::Client,
}

impl ExternalAgentClient {
    /// Build a client from `AMADEUS_EXTERNAL_AGENT_URL` and (optionally)
    /// `AMADEUS_EXTERNAL_AGENT_KEY`.  Returns `None` when the URL env-var is
    /// not set.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var(ENV_EXTERNAL_AGENT_URL).ok()?;
        let base_url = base_url.trim_end_matches('/').to_string();
        let api_key = std::env::var(ENV_EXTERNAL_AGENT_KEY).ok();
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .ok()?;
        Some(Self { base_url, api_key, http })
    }

    fn post_turn(&self, request: &TurnRequest) -> Result<String> {
        let url = format!("{}/api/agent/turn", self.base_url);

        let body = serde_json::json!({
            "prompt":    request.prompt,
            "sessionId": request.session_id,
            "voiceMode": request.voice_mode,
        });

        let mut builder = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        let resp = builder
            .send()
            .map_err(|e| anyhow::anyhow!("external agent request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            anyhow::bail!("external agent returned {status}: {text}");
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| anyhow::anyhow!("external agent response is not valid JSON: {e}"))?;

        let reply = json
            .get("reply")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("external agent response missing `reply` field"))?
            .to_string();

        Ok(reply)
    }
}

impl ConversationBackend for ExternalAgentClient {
    fn is_ready(&self) -> bool {
        true
    }

    fn reload_config(&self) {
        // No local config to reload for an external agent.
    }

    fn run_turn(&self, request: TurnRequest) -> Result<TurnResponse> {
        let reply = self.post_turn(&request)?;
        Ok(TurnResponse { reply })
    }

    fn run_turn_streaming(
        &self,
        request: TurnRequest,
        sink: &mut dyn TextStreamSink,
    ) -> Result<TurnResponse> {
        let reply = self.post_turn(&request)?;
        sink.on_text_delta(&reply)?;
        Ok(TurnResponse { reply })
    }
}
