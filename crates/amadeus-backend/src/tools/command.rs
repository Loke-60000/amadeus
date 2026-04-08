use std::{
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::ShellSecurityMode;

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

pub(crate) struct BashTool;

impl AgentTool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Bash",
            "Run a shell command in the workspace. Supports direct execution or bash -lc for shell syntax. Security policy applies.",
            json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": { "type": "string", "description": "Executable name or shell snippet." },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Arguments when not using shell mode." },
                    "cwd": { "type": "string", "description": "Optional workspace-relative working directory." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 600, "description": "Timeout in seconds (default 120)." },
                    "use_shell": { "type": "boolean", "description": "Run through bash -lc when shell syntax is required." },
                    "description": { "type": "string", "description": "Human-readable description of what this command does." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: BashArgs =
            serde_json::from_value(input).context("invalid Bash arguments")?;
        let cwd = ctx.resolve_tool_dir(args.cwd.as_deref())?;
        let use_shell = args.use_shell.unwrap_or(false);
        let timeout_secs = args
            .timeout_secs
            .unwrap_or(ctx.shell_policy.max_timeout_secs)
            .clamp(1, ctx.shell_policy.max_timeout_secs.max(1));
        let display = render_command_display(&args.command, &args.args, use_shell);

        ensure_command_stays_in_workspace_domain(ctx, &args.command, &args.args, use_shell, &cwd)?;
        ensure_command_allowed(ctx, &display, &args.command, &args.args, use_shell, &cwd)?;
        let result = execute_command(
            &args.command,
            &args.args,
            use_shell,
            &cwd,
            timeout_secs,
            ctx.shell_policy.max_output_chars,
        )?;

        Ok(ToolOutcome::new(
            format!("Executed {display}"),
            json!({
                "command": display,
                "cwd": ctx.display_relative(&cwd),
                "exit_code": result.exit_code,
                "timed_out": result.timed_out,
                "duration_ms": result.duration_ms,
                "stdout": result.stdout,
                "stderr": result.stderr,
            }),
        ))
    }
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
    use_shell: Option<bool>,
    #[allow(dead_code)]
    description: Option<String>,
}

struct CommandExecutionResult {
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u128,
    stdout: String,
    stderr: String,
}

fn ensure_command_stays_in_workspace_domain(
    ctx: &ToolContext,
    command: &str,
    args: &[String],
    use_shell: bool,
    cwd: &Path,
) -> Result<()> {
    if use_shell {
        if let Some(private_path) = private_runtime_path_from_shell(command, ctx, cwd) {
            bail!(
                "{} is private to the agent runtime",
                ctx.display_relative(&private_path)
            );
        }
        return Ok(());
    }

    for token in std::iter::once(command).chain(args.iter().map(String::as_str)) {
        if let Some(private_path) = private_runtime_path_from_token(token, ctx, cwd) {
            bail!(
                "{} is private to the agent runtime",
                ctx.display_relative(&private_path)
            );
        }
    }

    Ok(())
}

fn ensure_command_allowed(
    ctx: &ToolContext,
    display: &str,
    command: &str,
    args: &[String],
    use_shell: bool,
    cwd: &Path,
) -> Result<()> {
    if let Ok(approvals) = ctx.approved_commands.lock() {
        if approvals.contains(display) {
            return Ok(());
        }
    }

    let danger = command_requires_approval(command, args, use_shell);
    let basename = command_basename(command);
    let allowlisted = ctx.shell_policy.allowed_bins.contains(&basename);

    if !use_shell && allowlisted && !danger {
        return Ok(());
    }

    match ctx.shell_policy.mode {
        ShellSecurityMode::Full => {
            if use_shell && !ctx.shell_policy.allow_shell {
                bail!("shell execution is disabled by policy");
            }
            Ok(())
        }
        ShellSecurityMode::Allowlist => {
            if use_shell {
                bail!("shell execution is disabled in allowlist mode");
            }
            if danger {
                bail!("command denied by allowlist policy because it is considered dangerous");
            }
            bail!(
                "command {display} is not in the allowlist for {}",
                cwd.display()
            )
        }
        ShellSecurityMode::Ask => request_user_approval(ctx, display, cwd, use_shell),
    }
}

fn request_user_approval(
    ctx: &ToolContext,
    display: &str,
    cwd: &Path,
    use_shell: bool,
) -> Result<()> {
    if use_shell && !ctx.shell_policy.allow_shell {
        bail!("shell execution is disabled by policy");
    }

    eprintln!();
    eprintln!("[security] command approval required");
    eprintln!("  cwd : {}", ctx.boundary.display_relative(cwd));
    eprintln!("  cmd : {display}");
    eprint!("Allow once [y], allow always [a], deny [n]: ");
    std::io::stderr().flush().ok();

    let mut decision = String::new();
    std::io::stdin()
        .read_line(&mut decision)
        .context("failed to read the approval response")?;
    match decision.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        "a" | "always" => {
            if let Ok(mut approvals) = ctx.approved_commands.lock() {
                approvals.insert(display.to_string());
            }
            Ok(())
        }
        _ => bail!("command denied by the operator"),
    }
}

fn execute_command(
    command: &str,
    args: &[String],
    use_shell: bool,
    cwd: &Path,
    timeout_secs: u64,
    max_output_chars: usize,
) -> Result<CommandExecutionResult> {
    let start = Instant::now();
    let mut process = if use_shell {
        let shell = preferred_shell();
        let mut command_builder = Command::new(shell);
        command_builder.arg("-lc").arg(command);
        command_builder
    } else {
        let mut command_builder = Command::new(command);
        command_builder.args(args);
        command_builder
    };
    process
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = process
        .spawn()
        .with_context(|| format!("failed to spawn command {} in {}", command, cwd.display()))?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture command stderr")?;

    let stdout_thread = thread::spawn(move || read_stream(stdout));
    let stderr_thread = thread::spawn(move || read_stream(stderr));

    let timeout = Duration::from_secs(timeout_secs.max(1));
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .context("failed while waiting for the command")?
        {
            break status;
        }

        if start.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child
                .wait()
                .context("failed to collect the timed-out child status")?;
        }

        thread::sleep(Duration::from_millis(50));
    };

    let stdout = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader thread panicked"))??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader thread panicked"))??;

    Ok(CommandExecutionResult {
        exit_code: status.code(),
        timed_out,
        duration_ms: start.elapsed().as_millis(),
        stdout: truncate_output(&stdout, max_output_chars),
        stderr: truncate_output(&stderr, max_output_chars),
    })
}

fn read_stream(stream: impl Read) -> Result<String> {
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .context("failed to read process output")?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn truncate_output(text: &str, max_output_chars: usize) -> String {
    let mut collected = String::new();
    for ch in text.chars().take(max_output_chars) {
        collected.push(ch);
    }
    if text.chars().count() > max_output_chars {
        collected.push_str("\n[output truncated]");
    }
    collected
}

fn preferred_shell() -> PathBuf {
    std::env::var("SHELL")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| {
            let bash = PathBuf::from("/bin/bash");
            bash.exists().then_some(bash)
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn command_requires_approval(command: &str, args: &[String], use_shell: bool) -> bool {
    if use_shell {
        return true;
    }

    let basename = command_basename(command);
    if matches!(
        basename.as_str(),
        "rm" | "mkfs" | "shutdown" | "reboot" | "poweroff"
    ) {
        return true;
    }

    let rendered = render_command_display(command, args, false).to_ascii_lowercase();
    [
        "git reset --hard",
        "git clean -fd",
        "dd if=",
        ":(){",
        "chmod -r 777 /",
    ]
    .iter()
    .any(|pattern| rendered.contains(pattern))
}

fn command_basename(command: &str) -> String {
    Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase()
}

fn render_command_display(command: &str, args: &[String], use_shell: bool) -> String {
    if use_shell {
        format!("bash -lc {}", quote_token(command))
    } else {
        std::iter::once(command.to_string())
            .chain(args.iter().map(|value| quote_token(value)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn quote_token(token: &str) -> String {
    if token.is_empty() {
        return "''".to_string();
    }
    if token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        token.to_string()
    } else {
        format!("'{}'", token.replace('\'', "'\\''"))
    }
}

fn private_runtime_path_from_shell(command: &str, ctx: &ToolContext, cwd: &Path) -> Option<PathBuf> {
    if !command.contains(".amadeus") {
        return None;
    }

    private_runtime_path_from_token(command, ctx, cwd)
        .or_else(|| lexical_workspace_path(ctx, cwd, ".amadeus"))
}

fn private_runtime_path_from_token(token: &str, ctx: &ToolContext, cwd: &Path) -> Option<PathBuf> {
    if !token.contains(".amadeus") {
        return None;
    }

    command_token_candidates(token)
        .into_iter()
        .find_map(|candidate| lexical_workspace_path(ctx, cwd, candidate))
}

fn command_token_candidates(token: &str) -> Vec<&str> {
    let mut candidates = vec![token];
    if let Some((_, value)) = token.split_once('=') {
        candidates.push(value);
    }
    candidates
}

fn lexical_workspace_path(ctx: &ToolContext, cwd: &Path, raw: &str) -> Option<PathBuf> {
    if !raw.contains(".amadeus") || raw.contains("://") {
        return None;
    }

    let candidate = Path::new(raw);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    };
    let normalized = normalize_lexically(&joined);

    (normalized == ctx.boundary.root() || normalized.starts_with(ctx.boundary.root()))
        .then_some(normalized)
        .filter(|path| ctx.boundary.is_agent_private_path(path))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }

    normalized
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::{
        boundary::WorkspaceBoundary,
        config::ShellPolicyConfig,
        tools::catalog::ToolContext,
    };

    use super::ensure_command_stays_in_workspace_domain;

    #[test]
    fn commands_cannot_reference_agent_private_paths() -> Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(workspace.join("src"))?;
        let ctx = ToolContext::new(
            WorkspaceBoundary::new(workspace)?,
            ShellPolicyConfig::default(),
        );
        let cwd = ctx.resolve_tool_dir(None)?;
        let error = ensure_command_stays_in_workspace_domain(
            &ctx,
            "cat",
            &[".amadeus/sessions/main.json".to_string()],
            false,
            &cwd,
        )
        .unwrap_err();

        assert!(error.to_string().contains("private to the agent runtime"));
        Ok(())
    }

    #[test]
    fn shell_commands_cannot_reference_agent_private_paths() -> Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(workspace.join("src"))?;
        let ctx = ToolContext::new(
            WorkspaceBoundary::new(workspace)?,
            ShellPolicyConfig::default(),
        );
        let cwd = ctx.resolve_tool_dir(None)?;
        let error = ensure_command_stays_in_workspace_domain(
            &ctx,
            "cat .amadeus/workspace/SOUL.md",
            &[],
            true,
            &cwd,
        )
        .unwrap_err();

        assert!(error.to_string().contains("private to the agent runtime"));
        Ok(())
    }
}