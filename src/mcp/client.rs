use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::mcp::types::{
    JsonRpcRequest, JsonRpcResponse, McpContent, McpServerConfig, McpToolResult, McpToolSpec,
};

/// A live connection to an MCP server process.
pub struct McpClient {
    server_name: String,
    stdin: ChildStdin,
    reader: Arc<Mutex<BufReader<ChildStdout>>>,
    _child: Child,
    next_id: u64,
}

impl McpClient {
    /// Spawn the MCP server and perform the initialization handshake.
    pub fn connect(name: &str, cfg: &McpServerConfig) -> Result<Self> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn MCP server {name} ({})", cfg.command))?;

        let stdin = child.stdin.take().context("MCP server has no stdin")?;
        let stdout = child.stdout.take().context("MCP server has no stdout")?;
        let reader = Arc::new(Mutex::new(BufReader::new(stdout)));

        let mut client = Self {
            server_name: name.to_string(),
            stdin,
            reader,
            _child: child,
            next_id: 1,
        };

        // Initialize
        client.send_request("initialize", json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "amadeus", "version": "0.1.0" }
        }))?;

        // Send initialized notification
        client.send_notification("notifications/initialized", json!({}))?;

        Ok(client)
    }

    /// List all tools exposed by this MCP server.
    pub fn list_tools(&mut self) -> Result<Vec<McpToolSpec>> {
        let result = self.send_request("tools/list", json!({}))?;
        let tools = result["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value::<McpToolSpec>(v).ok())
            .collect();
        Ok(tools)
    }

    /// Call a tool on the MCP server.
    pub fn call_tool(&mut self, name: &str, input: Value) -> Result<McpToolResult> {
        let result = self.send_request("tools/call", json!({
            "name": name,
            "arguments": input,
        }))?;

        let is_error = result["isError"].as_bool().unwrap_or(false);
        let content = result["content"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value::<McpContent>(v).ok())
            .collect();

        Ok(McpToolResult { content, is_error })
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let mut line = serde_json::to_string(&req).context("failed to serialize JSON-RPC request")?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .with_context(|| format!("failed to write to MCP server {}", self.server_name))?;
        self.stdin.flush().ok();

        // Read response lines, skipping notifications (no id)
        let reader = Arc::clone(&self.reader);
        let mut reader = reader.lock().expect("MCP reader lock poisoned");
        loop {
            let mut response_line = String::new();
            reader
                .read_line(&mut response_line)
                .with_context(|| format!("failed to read from MCP server {}", self.server_name))?;

            if response_line.is_empty() {
                bail!("MCP server {} closed connection", self.server_name);
            }

            let response: JsonRpcResponse =
                serde_json::from_str(response_line.trim())
                    .with_context(|| format!("invalid JSON from MCP server {}: {response_line}", self.server_name))?;

            // Skip notifications (no id)
            if response.id.is_none() {
                continue;
            }

            if let Some(err) = response.error {
                bail!("MCP server {} returned error {}: {}", self.server_name, err.code, err.message);
            }

            return Ok(response.result.unwrap_or(Value::Null));
        }
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&notif).context("failed to serialize notification")?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).ok();
        self.stdin.flush().ok();
        Ok(())
    }
}
