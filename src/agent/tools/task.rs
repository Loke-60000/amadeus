use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    thread,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::task::{TaskRegistry, TaskStatus, TaskType};

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

// ── TaskCreate ────────────────────────────────────────────────────────────────

pub(crate) struct TaskCreateTool {
    pub(crate) registry: TaskRegistry,
}

impl AgentTool for TaskCreateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskCreate",
            "Create and start a background task. For local_bash tasks, the command runs immediately in the background.",
            json!({
                "type": "object",
                "required": ["type", "label"],
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": ["local_bash"],
                        "description": "Task type. Use local_bash to run a shell command in the background."
                    },
                    "label": { "type": "string", "description": "Human-readable description of the task." },
                    "command": { "type": "string", "description": "Shell command to run (required for local_bash)." },
                    "cwd": { "type": "string", "description": "Optional working directory." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TaskCreateArgs =
            serde_json::from_value(input).context("invalid TaskCreate arguments")?;

        match args.task_type.as_str() {
            "local_bash" => {
                let command = args.command.context("command is required for local_bash tasks")?;
                let cwd = ctx.resolve_tool_dir(args.cwd.as_deref())?;

                let task = self.registry.create(TaskType::LocalBash, &args.label);
                let task_id = task.id.clone();
                let registry = self.registry.clone();
                let output_arc = task.output.clone();

                registry.update_status(&task_id, TaskStatus::Running);

                thread::spawn(move || {
                    let mut child = match Command::new("bash")
                        .arg("-lc")
                        .arg(&command)
                        .current_dir(&cwd)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                    {
                        Ok(child) => child,
                        Err(e) => {
                            if let Ok(mut out) = output_arc.lock() {
                                *out = format!("failed to spawn: {e}");
                            }
                            registry.update_status(&task_id, TaskStatus::Failed);
                            return;
                        }
                    };

                    registry.set_pid(&task_id, child.id());

                    // Stream stdout
                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        let registry2 = registry.clone();
                        let id2 = task_id.clone();
                        thread::spawn(move || {
                            for line in reader.lines().flatten() {
                                registry2.append_output(&id2, &format!("{line}\n"));
                            }
                        });
                    }

                    // Stream stderr
                    if let Some(stderr) = child.stderr.take() {
                        let reader = BufReader::new(stderr);
                        let registry3 = registry.clone();
                        let id3 = task_id.clone();
                        thread::spawn(move || {
                            for line in reader.lines().flatten() {
                                registry3.append_output(&id3, &format!("[stderr] {line}\n"));
                            }
                        });
                    }

                    match child.wait() {
                        Ok(status) if status.success() => {
                            registry.update_status(&task_id, TaskStatus::Completed)
                        }
                        Ok(_) => registry.update_status(&task_id, TaskStatus::Failed),
                        Err(_) => registry.update_status(&task_id, TaskStatus::Failed),
                    }
                });

                Ok(ToolOutcome::new(
                    format!("Started task {} ({})", task.id, args.label),
                    json!({
                        "task_id": task.id,
                        "type": "local_bash",
                        "label": args.label,
                        "status": "running",
                    }),
                ))
            }
            other => bail!("unsupported task type {other:?}"),
        }
    }
}

// ── TaskList ──────────────────────────────────────────────────────────────────

pub(crate) struct TaskListTool {
    pub(crate) registry: TaskRegistry,
}

impl AgentTool for TaskListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskList",
            "List all background tasks and their current status.",
            json!({
                "type": "object",
                "properties": {
                    "status_filter": {
                        "type": "string",
                        "enum": ["pending", "running", "completed", "failed", "killed"],
                        "description": "Optional filter by status."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TaskListArgs =
            serde_json::from_value(input).context("invalid TaskList arguments")?;

        let tasks = self.registry.list();
        let filtered: Vec<Value> = tasks
            .iter()
            .filter(|task| {
                if let Some(filter) = &args.status_filter {
                    task.status.to_string() == *filter
                } else {
                    true
                }
            })
            .map(|task| json!({
                "task_id": task.id,
                "type": task.kind.to_string(),
                "label": task.label,
                "status": task.status.to_string(),
                "created_at": task.created_at,
            }))
            .collect();

        Ok(ToolOutcome::new(
            format!("Found {} task(s)", filtered.len()),
            json!({ "tasks": filtered }),
        ))
    }
}

// ── TaskGet ───────────────────────────────────────────────────────────────────

pub(crate) struct TaskGetTool {
    pub(crate) registry: TaskRegistry,
}

impl AgentTool for TaskGetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskGet",
            "Get details about a specific background task.",
            json!({
                "type": "object",
                "required": ["task_id"],
                "properties": {
                    "task_id": { "type": "string", "description": "The task ID to look up." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TaskIdArgs =
            serde_json::from_value(input).context("invalid TaskGet arguments")?;

        let Some(task) = self.registry.get(&args.task_id) else {
            bail!("task {} not found", args.task_id);
        };

        Ok(ToolOutcome::new(
            format!("Task {} is {}", task.id, task.status),
            json!({
                "task_id": task.id,
                "type": task.kind.to_string(),
                "label": task.label,
                "status": task.status.to_string(),
                "created_at": task.created_at,
                "is_terminal": task.status.is_terminal(),
            }),
        ))
    }
}

// ── TaskUpdate ────────────────────────────────────────────────────────────────

pub(crate) struct TaskUpdateTool {
    pub(crate) registry: TaskRegistry,
}

impl AgentTool for TaskUpdateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskUpdate",
            "Update the label of a task.",
            json!({
                "type": "object",
                "required": ["task_id", "label"],
                "properties": {
                    "task_id": { "type": "string" },
                    "label": { "type": "string", "description": "New human-readable label." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TaskUpdateArgs =
            serde_json::from_value(input).context("invalid TaskUpdate arguments")?;

        let Some(_task) = self.registry.get(&args.task_id) else {
            bail!("task {} not found", args.task_id);
        };

        self.registry.rename(&args.task_id, &args.label);

        Ok(ToolOutcome::new(
            format!("Updated task {}", args.task_id),
            json!({ "task_id": args.task_id, "label": args.label }),
        ))
    }
}

// ── TaskStop ──────────────────────────────────────────────────────────────────

pub(crate) struct TaskStopTool {
    pub(crate) registry: TaskRegistry,
}

impl AgentTool for TaskStopTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskStop",
            "Stop (kill) a running background task.",
            json!({
                "type": "object",
                "required": ["task_id"],
                "properties": {
                    "task_id": { "type": "string", "description": "The task ID to stop." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TaskIdArgs =
            serde_json::from_value(input).context("invalid TaskStop arguments")?;

        let Some(task) = self.registry.get(&args.task_id) else {
            bail!("task {} not found", args.task_id);
        };

        if task.status.is_terminal() {
            bail!("task {} is already in terminal state ({})", args.task_id, task.status);
        }

        let killed = self.registry.kill(&args.task_id);
        self.registry.update_status(&args.task_id, TaskStatus::Killed);

        Ok(ToolOutcome::new(
            format!("Stopped task {}", args.task_id),
            json!({ "task_id": args.task_id, "signal_sent": killed }),
        ))
    }
}

// ── TaskOutput ────────────────────────────────────────────────────────────────

pub(crate) struct TaskOutputTool {
    pub(crate) registry: TaskRegistry,
}

impl AgentTool for TaskOutputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskOutput",
            "Get the output (stdout/stderr) of a background task.",
            json!({
                "type": "object",
                "required": ["task_id"],
                "properties": {
                    "task_id": { "type": "string", "description": "The task ID." },
                    "tail_lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000,
                        "description": "Return only the last N lines (default: all)."
                    }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: TaskOutputArgs =
            serde_json::from_value(input).context("invalid TaskOutput arguments")?;

        let Some(task) = self.registry.get(&args.task_id) else {
            bail!("task {} not found", args.task_id);
        };

        let full_output = task.snapshot_output();
        let output = if let Some(tail) = args.tail_lines {
            full_output
                .lines()
                .rev()
                .take(tail)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            full_output.clone()
        };

        Ok(ToolOutcome::new(
            format!("Output of task {} ({} chars)", args.task_id, output.len()),
            json!({
                "task_id": args.task_id,
                "status": task.status.to_string(),
                "output": output,
            }),
        ))
    }
}

// ── Argument structs ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TaskCreateArgs {
    #[serde(rename = "type")]
    task_type: String,
    label: String,
    command: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct TaskListArgs {
    status_filter: Option<String>,
}

#[derive(Deserialize)]
struct TaskIdArgs {
    task_id: String,
}

#[derive(Deserialize)]
struct TaskUpdateArgs {
    task_id: String,
    label: String,
}

#[derive(Deserialize)]
struct TaskOutputArgs {
    task_id: String,
    tail_lines: Option<usize>,
}
