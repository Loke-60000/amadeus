use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::agent::{
    boundary::WorkspaceBoundary,
    config::ShellPolicyConfig,
    planning::PlanningState,
    skills::Skill,
    task::TaskRegistry,
};

use super::{
    agent::AgentSpawnTool,
    command::BashTool,
    filesystem::{EditTool, GlobTool, GrepTool, LsTool, ReadTool, WriteTool},
    planning::{EnterPlanModeTool, ExitPlanModeTool},
    skill::SkillTool,
    task::{
        TaskCreateTool, TaskGetTool, TaskListTool, TaskOutputTool, TaskStopTool, TaskUpdateTool,
    },
    user::AskUserQuestionTool,
    web::{WebFetchTool, WebSearchTool},
};

#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolOutcome {
    pub summary: String,
    pub payload: Value,
}

impl ToolOutcome {
    pub fn new(summary: impl Into<String>, payload: Value) -> Self {
        Self {
            summary: summary.into(),
            payload,
        }
    }

    pub fn to_tool_message(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "summary": self.summary,
            "data": self.payload,
        }))
        .unwrap_or_else(|_| format!("{{\"ok\":true,\"summary\":{:?}}}", self.summary))
    }
}

pub(crate) trait AgentTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome>;
}

#[derive(Clone)]
pub(crate) struct ToolContext {
    pub(crate) boundary: WorkspaceBoundary,
    pub(crate) shell_policy: ShellPolicyConfig,
    pub(crate) approved_commands: Arc<Mutex<BTreeSet<String>>>,
}

impl ToolContext {
    pub(crate) fn new(boundary: WorkspaceBoundary, shell_policy: ShellPolicyConfig) -> Self {
        Self {
            boundary,
            shell_policy,
            approved_commands: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    pub(crate) fn resolve_tool_dir(&self, raw: Option<&str>) -> Result<PathBuf> {
        let path = self.boundary.resolve_dir(raw)?;
        self.ensure_tool_visible(&path)?;
        Ok(path)
    }

    pub(crate) fn resolve_tool_existing(&self, raw: &str) -> Result<PathBuf> {
        let path = self.boundary.resolve_existing(raw)?;
        self.ensure_tool_visible(&path)?;
        Ok(path)
    }

    pub(crate) fn resolve_tool_output(&self, raw: &str) -> Result<PathBuf> {
        let path = self.boundary.resolve_output(raw)?;
        self.ensure_tool_visible(&path)?;
        Ok(path)
    }

    pub(crate) fn display_relative(&self, path: &Path) -> String {
        self.boundary.display_relative(path)
    }

    pub(crate) fn ensure_tool_visible(&self, path: &Path) -> Result<()> {
        if self.boundary.is_agent_private_path(path) {
            bail!(
                "{} is private to the agent runtime",
                self.boundary.display_relative(path)
            );
        }

        Ok(())
    }
}

pub struct ToolCatalog {
    tools: Vec<Box<dyn AgentTool>>,
    by_name: BTreeMap<String, usize>,
    context: ToolContext,
    task_registry: TaskRegistry,
    planning: PlanningState,
}

impl ToolCatalog {
    pub fn new(
        boundary: WorkspaceBoundary,
        shell_policy: ShellPolicyConfig,
        search_api_key: Option<String>,
        skills: Vec<Skill>,
    ) -> Self {
        let workspace_root = boundary.root().to_path_buf();
        let context = ToolContext::new(boundary, shell_policy);
        let task_registry = TaskRegistry::new();
        let planning = PlanningState::new();
        let tools: Vec<Box<dyn AgentTool>> = vec![
            Box::new(LsTool),
            Box::new(GlobTool),
            Box::new(ReadTool),
            Box::new(GrepTool),
            Box::new(WriteTool),
            Box::new(EditTool),
            Box::new(BashTool),
            Box::new(WebFetchTool),
            Box::new(WebSearchTool::new(search_api_key)),
            Box::new(TaskCreateTool { registry: task_registry.clone() }),
            Box::new(TaskListTool { registry: task_registry.clone() }),
            Box::new(TaskGetTool { registry: task_registry.clone() }),
            Box::new(TaskUpdateTool { registry: task_registry.clone() }),
            Box::new(TaskStopTool { registry: task_registry.clone() }),
            Box::new(TaskOutputTool { registry: task_registry.clone() }),
            Box::new(EnterPlanModeTool { planning: planning.clone() }),
            Box::new(ExitPlanModeTool { planning: planning.clone() }),
            Box::new(AskUserQuestionTool { planning: planning.clone() }),
            Box::new(AgentSpawnTool {
                task_registry: task_registry.clone(),
                workspace_root,
            }),
            Box::new(SkillTool { skills }),
        ];
        let by_name = tools
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.definition().name.clone(), index))
            .collect();

        Self {
            tools,
            by_name,
            context,
            task_registry,
            planning,
        }
    }

    pub fn task_registry(&self) -> &TaskRegistry {
        &self.task_registry
    }

    pub fn planning(&self) -> &PlanningState {
        &self.planning
    }

    /// Register an additional tool at runtime (used by MCP, skills, etc.)
    pub fn register(&mut self, tool: Box<dyn AgentTool>) {
        let name = tool.definition().name.clone();
        let index = self.tools.len();
        self.tools.push(tool);
        self.by_name.insert(name, index);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    pub fn invoke(&self, name: &str, input: Value) -> Result<ToolOutcome> {
        let Some(index) = self.by_name.get(name) else {
            bail!("unknown tool {name:?}");
        };
        self.tools[*index].invoke(input, &self.context)
    }

    pub fn context(&self) -> &ToolContext {
        &self.context
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::agent::{boundary::WorkspaceBoundary, config::ShellPolicyConfig};

    use super::ToolContext;

    #[test]
    fn tool_context_blocks_agent_private_runtime_paths() -> Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path().join("workspace");
        let private_file = workspace.join(".amadeus").join("sessions").join("main.json");
        fs::create_dir_all(private_file.parent().unwrap())?;
        fs::write(&private_file, "{}")?;

        let boundary = WorkspaceBoundary::new(workspace)?;
        let ctx = ToolContext::new(boundary, ShellPolicyConfig::default());
        let error = ctx
            .resolve_tool_existing(".amadeus/sessions/main.json")
            .unwrap_err();

        assert!(error.to_string().contains("private to the agent runtime"));
        Ok(())
    }
}
