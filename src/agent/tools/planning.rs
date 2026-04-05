use anyhow::Result;
use serde_json::{Value, json};

use crate::agent::planning::{PlanMode, PlanningState};

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

pub(crate) struct EnterPlanModeTool {
    pub(crate) planning: PlanningState,
}

impl AgentTool for EnterPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "EnterPlanMode",
            "Switch into planning mode. In plan mode, read files and explore the codebase before proposing changes. No code is written until ExitPlanMode is called.",
            json!({
                "type": "object",
                "properties": {}
            }),
        )
    }

    fn invoke(&self, _input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        self.planning.set_mode(PlanMode::Active);
        Ok(ToolOutcome::new(
            "Entered plan mode",
            json!({ "mode": "active", "message": "Now in plan mode. Explore the codebase and design your approach. Call ExitPlanMode when ready to present the plan." }),
        ))
    }
}

pub(crate) struct ExitPlanModeTool {
    pub(crate) planning: PlanningState,
}

impl AgentTool for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "ExitPlanMode",
            "Exit planning mode and present the completed plan. Use this after exploring the codebase and designing an implementation approach.",
            json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The implementation plan to present to the user."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        self.planning.set_mode(PlanMode::Off);

        let plan = input["plan"]
            .as_str()
            .unwrap_or("Plan complete. Awaiting approval.");

        Ok(ToolOutcome::new(
            "Exited plan mode",
            json!({
                "mode": "off",
                "plan": plan,
                "message": "Plan presented. Awaiting user approval before implementation."
            }),
        ))
    }
}
