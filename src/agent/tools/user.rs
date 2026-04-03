use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::planning::PlanningState;

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

const QUESTION_TIMEOUT_SECS: u64 = 300; // 5 minutes

pub(crate) struct AskUserQuestionTool {
    pub(crate) planning: PlanningState,
}

impl AgentTool for AskUserQuestionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "AskUserQuestion",
            "Ask the user a clarifying question and wait for their response. Use this when you need input before proceeding.",
            json!({
                "type": "object",
                "required": ["question"],
                "properties": {
                    "question": { "type": "string", "description": "The question to ask the user." },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of suggested answers."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: AskArgs =
            serde_json::from_value(input).context("invalid AskUserQuestion arguments")?;

        let options = args.options.unwrap_or_default();

        // Post the question and block until the UI delivers an answer
        self.planning.post_question(args.question.clone(), options.clone());

        match self.planning.wait_for_answer(QUESTION_TIMEOUT_SECS) {
            Some(answer) => Ok(ToolOutcome::new(
                format!("User answered: {}", answer),
                json!({
                    "question": args.question,
                    "answer": answer,
                }),
            )),
            None => Ok(ToolOutcome::new(
                "Question timed out — no user response",
                json!({
                    "question": args.question,
                    "answer": null,
                    "timed_out": true,
                }),
            )),
        }
    }
}

#[derive(Deserialize)]
struct AskArgs {
    question: String,
    options: Option<Vec<String>>,
}
