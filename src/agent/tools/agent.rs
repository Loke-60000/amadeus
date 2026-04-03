use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    thread,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::agent::task::{TaskRegistry, TaskStatus, TaskType};

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

const MAX_AGENT_OUTPUT_CHARS: usize = 100_000;

pub(crate) struct AgentSpawnTool {
    pub(crate) task_registry: TaskRegistry,
    pub(crate) workspace_root: PathBuf,
}

impl AgentTool for AgentSpawnTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Agent",
            "Launch a subagent to handle a complex, multi-step task autonomously. The subagent gets its own context window and full tool access. Use run_in_background for long-running tasks.",
            json!({
                "type": "object",
                "required": ["description", "prompt"],
                "properties": {
                    "description": { "type": "string", "description": "Short description of what this agent will do (3-5 words)." },
                    "prompt": { "type": "string", "description": "The task description and context for the subagent." },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "If true, start the agent as a background task and return immediately with a task_id. If false (default), wait for completion."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: AgentArgs =
            serde_json::from_value(input).context("invalid Agent arguments")?;

        let exe = std::env::current_exe()
            .context("failed to locate the current executable for subagent spawn")?;

        let session_id = format!("subagent-{}", &Uuid::new_v4().to_string()[..8]);
        let workspace = self.workspace_root.to_string_lossy().to_string();

        let run_in_background = args.run_in_background.unwrap_or(false);

        if run_in_background {
            let task = self.task_registry.create(TaskType::LocalAgent, &args.description);
            let task_id = task.id.clone();
            let registry = self.task_registry.clone();
            let prompt = args.prompt.clone();

            thread::spawn(move || {
                registry.update_status(&task_id, TaskStatus::Running);

                let mut child = match Command::new(&exe)
                    .args([
                        "agent", "chat",
                        "--prompt", &prompt,
                        "--session", &session_id,
                        "--workspace", &workspace,
                        "--security", "full",
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                {
                    Ok(child) => child,
                    Err(e) => {
                        registry.append_output(&task_id, &format!("spawn failed: {e}"));
                        registry.update_status(&task_id, TaskStatus::Failed);
                        return;
                    }
                };

                registry.set_pid(&task_id, child.id());

                if let Some(stdout) = child.stdout.take() {
                    let r2 = registry.clone();
                    let id2 = task_id.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(stdout).lines().flatten() {
                            r2.append_output(&id2, &format!("{line}\n"));
                        }
                    });
                }

                if let Some(stderr) = child.stderr.take() {
                    let r3 = registry.clone();
                    let id3 = task_id.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(stderr).lines().flatten() {
                            r3.append_output(&id3, &format!("[stderr] {line}\n"));
                        }
                    });
                }

                match child.wait() {
                    Ok(status) if status.success() => {
                        registry.update_status(&task_id, TaskStatus::Completed)
                    }
                    _ => registry.update_status(&task_id, TaskStatus::Failed),
                }
            });

            Ok(ToolOutcome::new(
                format!("Started background agent: {}", args.description),
                json!({
                    "task_id": task.id,
                    "description": args.description,
                    "run_in_background": true,
                    "message": "Agent started. Use TaskOutput to check progress.",
                }),
            ))
        } else {
            // Foreground: wait for completion
            let output = Command::new(&exe)
                .args([
                    "agent", "chat",
                    "--prompt", &args.prompt,
                    "--session", &session_id,
                    "--workspace", &workspace,
                    "--security", "full",
                ])
                .output()
                .context("failed to run subagent")?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            let mut combined = stdout;
            if !stderr.is_empty() {
                combined.push_str("\n[stderr]\n");
                combined.push_str(&stderr);
            }

            // Truncate to avoid flooding context
            if combined.chars().count() > MAX_AGENT_OUTPUT_CHARS {
                let truncated: String = combined.chars().take(MAX_AGENT_OUTPUT_CHARS).collect();
                combined = format!("{truncated}\n\n[output truncated]");
            }

            Ok(ToolOutcome::new(
                format!("Agent completed: {}", args.description),
                json!({
                    "description": args.description,
                    "exit_code": output.status.code(),
                    "output": combined,
                }),
            ))
        }
    }
}

#[derive(Deserialize)]
struct AgentArgs {
    description: String,
    prompt: String,
    run_in_background: Option<bool>,
}
