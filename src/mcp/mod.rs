pub(crate) mod client;
pub(crate) mod types;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    agent::tools::{AgentTool, ToolContext, ToolDefinition, ToolOutcome},
    mcp::{client::McpClient, types::McpToolSpec},
};

/// An AgentTool that wraps a single MCP tool.
pub(crate) struct McpAgentTool {
    pub server_name: String,
    pub spec: McpToolSpec,
    pub client: Arc<Mutex<McpClient>>,
}

impl AgentTool for McpAgentTool {
    fn definition(&self) -> ToolDefinition {
        let description = self.spec.description.as_deref()
            .unwrap_or("MCP tool")
            .to_string();

        let qualified = format!("mcp__{}__{}", self.server_name, self.spec.name);

        ToolDefinition::new(
            qualified,
            description,
            self.spec.input_schema.clone().unwrap_or_else(|| json!({ "type": "object" })),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let mut client = self.client.lock().expect("MCP client lock poisoned");
        let result = client.call_tool(&self.spec.name, input)?;

        let text: String = result.content
            .iter()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_error {
            anyhow::bail!("MCP tool {} returned error: {}", self.spec.name, text);
        }

        Ok(ToolOutcome::new(
            format!("MCP {}::{} completed", self.server_name, self.spec.name),
            json!({ "output": text }),
        ))
    }
}
