use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

const MAX_FETCH_CHARS: usize = 50_000;
const FETCH_TIMEOUT_SECS: u64 = 30;
const MAX_SEARCH_RESULTS: usize = 10;

// ── WebFetch ──────────────────────────────────────────────────────────────────

pub(crate) struct WebFetchTool;

impl AgentTool for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "WebFetch",
            "Fetch a URL and return its content as plain text. HTML is converted to readable text.",
            json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "description": "The URL to fetch." },
                    "max_length": {
                        "type": "integer",
                        "minimum": 1000,
                        "maximum": 100000,
                        "description": "Maximum characters to return (default 50000)."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: WebFetchArgs =
            serde_json::from_value(input).context("invalid WebFetch arguments")?;

        let max_length = args.max_length.unwrap_or(MAX_FETCH_CHARS).clamp(1000, 100_000);

        let client = Client::builder()
            .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
            .user_agent("amadeus/1.0 (research agent)")
            .build()
            .context("failed to build HTTP client")?;

        let response = client
            .get(&args.url)
            .send()
            .with_context(|| format!("failed to fetch {}", args.url))?;

        let status = response.status();
        if !status.is_success() {
            bail!("HTTP {} when fetching {}", status, args.url);
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        let body = response
            .text()
            .with_context(|| format!("failed to read response body from {}", args.url))?;

        let text = if content_type.contains("html") {
            html_to_text(&body)
        } else {
            body
        };

        let truncated = truncate_chars(&text, max_length);
        let was_truncated = text.chars().count() > max_length;

        Ok(ToolOutcome::new(
            format!("Fetched {} ({} chars{})", args.url, truncated.chars().count(), if was_truncated { ", truncated" } else { "" }),
            json!({
                "url": args.url,
                "content": truncated,
                "truncated": was_truncated,
            }),
        ))
    }
}

// ── WebSearch ─────────────────────────────────────────────────────────────────

pub(crate) struct WebSearchTool {
    pub(crate) api_key: Option<String>,
}

impl WebSearchTool {
    pub(crate) fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }
}

impl AgentTool for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "WebSearch",
            "Search the web and return a list of results with titles, URLs, and snippets.",
            json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "The search query." },
                    "num_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Number of results to return (default 5)."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: WebSearchArgs =
            serde_json::from_value(input).context("invalid WebSearch arguments")?;

        let num_results = args.num_results.unwrap_or(5).clamp(1, MAX_SEARCH_RESULTS);

        let results = if let Some(api_key) = &self.api_key {
            brave_search(&args.query, num_results, api_key)?
        } else {
            ddg_search(&args.query, num_results)?
        };

        Ok(ToolOutcome::new(
            format!("Found {} results for {:?}", results.len(), args.query),
            json!({
                "query": args.query,
                "results": results,
            }),
        ))
    }
}

// ── Argument structs ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WebFetchArgs {
    url: String,
    max_length: Option<usize>,
}

#[derive(Deserialize)]
struct WebSearchArgs {
    query: String,
    num_results: Option<usize>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 120)
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push_str("\n\n[content truncated]");
    }
    out
}

/// Brave Search API (requires API key, env: AMADEUS_SEARCH_API_KEY)
fn brave_search(query: &str, num_results: usize, api_key: &str) -> Result<Vec<Value>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("failed to build search client")?;

    let response = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("Accept", "application/json")
        .header("Accept-Encoding", "gzip")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", &num_results.to_string())])
        .send()
        .context("brave search request failed")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!("Brave Search returned {status}: {body}");
    }

    let data: Value = response.json().context("failed to parse brave search response")?;

    let results = data["web"]["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(num_results)
        .map(|item| {
            json!({
                "title": item["title"],
                "url": item["url"],
                "snippet": item["description"],
            })
        })
        .collect();

    Ok(results)
}

/// DuckDuckGo Instant Answer API (no key needed, limited but free)
fn ddg_search(query: &str, num_results: usize) -> Result<Vec<Value>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("amadeus/1.0")
        .build()
        .context("failed to build search client")?;

    let response = client
        .get("https://api.duckduckgo.com/")
        .query(&[
            ("q", query),
            ("format", "json"),
            ("no_redirect", "1"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ])
        .send()
        .context("DuckDuckGo search request failed")?;

    let status = response.status();
    if !status.is_success() {
        bail!("DuckDuckGo returned {status}");
    }

    let data: Value = response.json().context("failed to parse DDG response")?;

    let mut results: Vec<Value> = Vec::new();

    // Instant answer
    if let Some(text) = data["AbstractText"].as_str().filter(|s| !s.is_empty()) {
        results.push(json!({
            "title": data["Heading"].as_str().unwrap_or("DuckDuckGo Instant Answer"),
            "url": data["AbstractURL"].as_str().unwrap_or(""),
            "snippet": text,
        }));
    }

    // Related topics
    if let Some(topics) = data["RelatedTopics"].as_array() {
        for topic in topics.iter().take(num_results.saturating_sub(results.len())) {
            if let Some(text) = topic["Text"].as_str().filter(|s| !s.is_empty()) {
                results.push(json!({
                    "title": text.split(" - ").next().unwrap_or(text),
                    "url": topic["FirstURL"].as_str().unwrap_or(""),
                    "snippet": text,
                }));
            }
        }
    }

    if results.is_empty() {
        // Return a helpful message rather than an empty list
        results.push(json!({
            "title": "No instant results",
            "url": format!("https://duckduckgo.com/?q={}", urlencoding::encode(query)),
            "snippet": "DuckDuckGo has no instant answer. Try WebFetch on the URL above or set AMADEUS_SEARCH_API_KEY for Brave Search.",
        }));
    }

    Ok(results)
}

fn urlencoding_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

// Inline simple URL encoding to avoid extra dependency
mod urlencoding {
    pub fn encode(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
                ' ' => "+".to_string(),
                _ => format!("%{:02X}", c as u32),
            })
            .collect()
    }
}
