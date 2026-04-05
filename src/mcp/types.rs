use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Configuration for a single MCP server.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// A tool exposed by an MCP server.
#[derive(Clone, Debug, Deserialize)]
pub struct McpToolSpec {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

/// Result from calling an MCP tool.
#[derive(Debug)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    pub is_error: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpContent {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub kind: String,
    pub text: Option<String>,
}

impl McpContent {
    pub fn to_string_lossy(&self) -> String {
        self.text.clone().unwrap_or_default()
    }
}

// ── JSON-RPC types ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcResponse {
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct JsonRpcError {
    pub code: i64,
    pub message: String,
}
