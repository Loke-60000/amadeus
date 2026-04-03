use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::skills::Skill;

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

pub(crate) struct SkillTool {
    pub(crate) skills: Vec<Skill>,
}

impl AgentTool for SkillTool {
    fn definition(&self) -> ToolDefinition {
        let skill_list: Vec<Value> = self.skills.iter().map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "trigger": s.trigger,
            })
        }).collect();

        ToolDefinition::new(
            "Skill",
            "Invoke a named skill. Skills are predefined prompts that expand into specific actions. Call with no skill_name to list available skills.",
            json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "The name of the skill to invoke (e.g. 'commit', 'simplify', 'remember')."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: SkillArgs =
            serde_json::from_value(input).context("invalid Skill arguments")?;

        let Some(name) = &args.skill_name else {
            // List mode
            let list: Vec<Value> = self.skills.iter().map(|s| json!({
                "name": s.name,
                "description": s.description,
                "trigger": s.trigger,
            })).collect();
            return Ok(ToolOutcome::new(
                format!("Available skills: {}", self.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")),
                json!({ "skills": list }),
            ));
        };

        let skill = self.skills.iter()
            .find(|s| s.name == *name || s.trigger.as_deref() == Some(name))
            .ok_or_else(|| anyhow::anyhow!("skill {name:?} not found; available: {}", self.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")))?;

        Ok(ToolOutcome::new(
            format!("Invoking skill: {}", skill.name),
            json!({
                "skill": skill.name,
                "description": skill.description,
                "prompt": skill.prompt_template,
                "instruction": "Execute the above prompt now as your next action.",
            }),
        ))
    }
}

#[derive(Deserialize)]
struct SkillArgs {
    skill_name: Option<String>,
}
