//! HTTP server that exposes the agent as a remote backend.
//!
//! Start with:
//!   cargo run -- agent serve [--bind 0.0.0.0:8080]
//!
//! The desktop client (native viewer) then points to it with:
//!   AMADEUS_EXTERNAL_AGENT_URL=http://<host>:8080 cargo run
//!
//! Endpoints:
//!
//!   POST /api/agent/turn
//!     Body:    { "prompt": "...", "sessionId": "...", "voiceMode": false }
//!     Returns: { "reply": "..." }
//!
//!   GET  /api/agent/health
//!     Returns: { "status": "ok", "model_ready": true }
//!
//! An optional bearer-token gate is activated when AMADEUS_SERVE_KEY is set.

use std::{
    io::Read,
    net::SocketAddr,
    sync::Arc,
};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    backend::{TurnRequest, TurnResponse},
    ConversationBackend,
    ui::AgentUiService,
    config::AgentRuntimeConfig,
};

const ENV_SERVE_KEY: &str = "AMADEUS_SERVE_KEY";
const DEFAULT_BIND: &str = "127.0.0.1:8765";

pub struct ServeOptions {
    pub bind: String,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self { bind: DEFAULT_BIND.to_string() }
    }
}

/// Build an `AgentUiService` from `runtime` and serve it over HTTP until interrupted.
pub fn run_serve(runtime: AgentRuntimeConfig, options: ServeOptions) -> Result<()> {
    let addr: SocketAddr = options
        .bind
        .parse()
        .with_context(|| format!("invalid bind address: {}", options.bind))?;

    let service: Arc<dyn ConversationBackend> = Arc::new(AgentUiService::new(runtime));
    let api_key = std::env::var(ENV_SERVE_KEY).ok();

    let server = tiny_http::Server::http(addr)
        .map_err(|e| anyhow::anyhow!("failed to bind {addr}: {e}"))?;

    eprintln!("[amadeus serve] listening on http://{addr}");
    if api_key.is_some() {
        eprintln!("[amadeus serve] bearer-token auth enabled (AMADEUS_SERVE_KEY)");
    }

    for request in server.incoming_requests() {
        let method = request.method().as_str().to_ascii_uppercase();
        let url = request.url().to_string();

        // Trim query strings for routing.
        let path: &str = url.split('?').next().unwrap_or(&url);

        let response = handle_request(request, &method, path, &service, api_key.as_deref());
        if let Err(e) = response {
            eprintln!("[amadeus serve] handler error: {e:#}");
        }
    }

    Ok(())
}

fn handle_request(
    mut request: tiny_http::Request,
    method: &str,
    path: &str,
    service: &Arc<dyn ConversationBackend>,
    api_key: Option<&str>,
) -> Result<()> {
    // Optional bearer-token check.
    if let Some(expected) = api_key {
        let provided = request
            .headers()
            .iter()
            .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("Authorization"))
            .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
            .map(str::to_string);

        if provided.as_deref() != Some(expected) {
            return respond_text(request, 401, r#"{"error":"unauthorized"}"#);
        }
    }

    match (method, path) {
        ("GET", "/api/agent/health") => {
            let ready = service.is_ready();
            let body = format!(r#"{{"status":"ok","model_ready":{ready}}}"#);
            respond_text(request, 200, &body)
        }

        ("POST", "/api/agent/turn") => {
            let mut body = String::new();
            request
                .as_reader()
                .read_to_string(&mut body)
                .context("failed to read request body")?;

            let json: Value = serde_json::from_str(&body)
                .map_err(|e| anyhow::anyhow!("invalid JSON body: {e}"))?;

            let prompt = json
                .get("prompt")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing `prompt` field"))?
                .to_string();

            let session_id = json
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let voice_mode = json
                .get("voiceMode")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let reply_body = match service.run_turn(TurnRequest { prompt, session_id, voice_mode }) {
                Ok(TurnResponse { reply }) => serde_json::json!({ "reply": reply }).to_string(),
                Err(e) => {
                    let msg = format!("{e:#}");
                    return respond_text(request, 500, &serde_json::json!({"error": msg}).to_string());
                }
            };

            respond_text(request, 200, &reply_body)
        }

        _ => respond_text(request, 404, r#"{"error":"not found"}"#),
    }
}

fn respond_text(request: tiny_http::Request, code: u16, body: &str) -> Result<()> {
    let response = tiny_http::Response::from_string(body)
        .with_status_code(code)
        .with_header(
            tiny_http::Header::from_bytes(b"Content-Type", b"application/json").unwrap(),
        );
    request.respond(response).context("failed to send HTTP response")?;
    Ok(())
}
