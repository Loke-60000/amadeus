use std::{fs, path::PathBuf, thread, time::Duration};

use anyhow::{Context, Result, bail};

use crate::{
    app::AgentApp,
    autonomy::AutonomyActivity,
    config::{AgentRuntimeConfig, LlmProvider, ShellSecurityMode},
    prompt::PromptComposer,
    serve::{ServeOptions, run_serve},
    tools::ToolCatalog,
    workspace::AgentWorkspace,
};

pub fn run_cli(args: &[String]) -> Result<()> {
    match parse_command(args)? {
        AgentCommand::Help => {
            print_help();
            Ok(())
        }
        AgentCommand::Init(runtime) => run_init(runtime),
        AgentCommand::Prompt(runtime) => run_prompt(runtime),
        AgentCommand::Auto(runtime, options) => run_auto(runtime, options),
        AgentCommand::Chat(runtime, prompt) => {
            let mut app = AgentApp::new(runtime)?;
            if let Some(prompt) = prompt {
                let reply = app.run_single_prompt(&prompt)?;
                println!("{reply}");
                Ok(())
            } else {
                app.run_interactive()
            }
        }
        AgentCommand::Serve(runtime, options) => run_serve(runtime, options),
    }
}

enum AgentCommand {
    Chat(AgentRuntimeConfig, Option<String>),
    Auto(AgentRuntimeConfig, AutoOptions),
    Init(AgentRuntimeConfig),
    Prompt(AgentRuntimeConfig),
    Serve(AgentRuntimeConfig, ServeOptions),
    Help,
}

#[derive(Clone, Copy, Debug, Default)]
struct AutoOptions {
    daemon: bool,
    cycles: Option<usize>,
}

fn parse_command(args: &[String]) -> Result<AgentCommand> {
    let bootstrap = scan_bootstrap_options(args)?;
    let mut runtime = AgentRuntimeConfig::load(bootstrap.workspace_root, bootstrap.config_path)?;
    let mut prompt = None;
    let mut subcommand = "chat";
    let mut index = 0usize;
    let mut api_base_explicit = false;
    let mut api_key_explicit = false;
    let mut auto_options = AutoOptions::default();
    let mut serve_bind: Option<String> = None;

    if let Some(first) = args.first() {
        if !first.starts_with('-') {
            subcommand = first.as_str();
            index = 1;
        }
    }

    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        match flag {
            "--help" | "-h" => return Ok(AgentCommand::Help),
            "--workspace" => {
                runtime.workspace_root = PathBuf::from(next_value(args, &mut index, flag)?);
            }
            "--config" => {
                let _ = next_value(args, &mut index, flag)?;
            }
            "--session" => {
                runtime.session_id = next_value(args, &mut index, flag)?.to_string();
            }
            "--model" => {
                runtime.model = Some(next_value(args, &mut index, flag)?.to_string());
            }
            "--provider" => {
                let previous_provider = runtime.provider;
                runtime.provider = LlmProvider::parse(next_value(args, &mut index, flag)?)?;
                if !api_base_explicit
                    && should_refresh_api_base(&runtime.api_base, previous_provider)
                {
                    runtime.api_base = runtime.provider.default_api_base().to_string();
                }
                if !api_key_explicit
                    && should_refresh_api_key(runtime.api_key.as_deref(), previous_provider)
                {
                    runtime.api_key = runtime.provider.default_api_key();
                }
            }
            "--api-base" => {
                runtime.api_base = next_value(args, &mut index, flag)?.to_string();
                api_base_explicit = true;
            }
            "--api-key" => {
                runtime.api_key = Some(next_value(args, &mut index, flag)?.to_string());
                api_key_explicit = true;
            }
            "--prompt" => {
                prompt = Some(next_value(args, &mut index, flag)?.to_string());
            }
            "--security" => {
                runtime.shell_policy.mode =
                    ShellSecurityMode::parse(next_value(args, &mut index, flag)?)?;
            }
            "--allow-bin" => {
                runtime
                    .shell_policy
                    .allowed_bins
                    .insert(next_value(args, &mut index, flag)?.to_ascii_lowercase());
            }
            "--allow-shell" => {
                runtime.shell_policy.allow_shell = true;
            }
            "--temperature" => {
                runtime.temperature = next_value(args, &mut index, flag)?
                    .parse::<f32>()
                    .with_context(|| format!("invalid float for {flag}"))?;
            }
            "--max-output-tokens" => {
                runtime.max_output_tokens = next_value(args, &mut index, flag)?
                    .parse::<usize>()
                    .with_context(|| format!("invalid integer for {flag}"))?
                    .max(1);
            }
            "--max-context-tokens" => {
                runtime.max_context_tokens = next_value(args, &mut index, flag)?
                    .parse::<usize>()
                    .with_context(|| format!("invalid integer for {flag}"))?
                    .max(512);
            }
            "--max-tool-rounds" => {
                runtime.max_tool_rounds = next_value(args, &mut index, flag)?
                    .parse::<usize>()
                    .with_context(|| format!("invalid integer for {flag}"))?
                    .max(1);
            }
            "--daemon" => {
                auto_options.daemon = true;
            }
            "--cycles" => {
                auto_options.cycles = Some(
                    next_value(args, &mut index, flag)?
                        .parse::<usize>()
                        .with_context(|| format!("invalid integer for {flag}"))?
                        .max(1),
                );
            }
            "--bind" => {
                serve_bind = Some(next_value(args, &mut index, flag)?.to_string());
            }
            other => bail!("unknown option {other:?}; run `cargo run -- agent --help`"),
        }
    }

    runtime.normalize_provider_defaults();

    match subcommand {
        "chat" => Ok(AgentCommand::Chat(runtime, prompt)),
        "auto" | "autonomous" => {
            if prompt.is_some() {
                bail!("--prompt is only valid with the chat subcommand");
            }
            runtime.autonomy.enabled = true;
            Ok(AgentCommand::Auto(runtime, auto_options))
        }
        "init" => Ok(AgentCommand::Init(runtime)),
        "prompt" => Ok(AgentCommand::Prompt(runtime)),
        "serve" => Ok(AgentCommand::Serve(
            runtime,
            ServeOptions {
                bind: serve_bind.unwrap_or_else(|| ServeOptions::default().bind),
            },
        )),
        "help" => Ok(AgentCommand::Help),
        other => bail!("unknown agent subcommand {other:?}"),
    }
}

#[derive(Default)]
struct BootstrapOptions {
    workspace_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
}

fn scan_bootstrap_options(args: &[String]) -> Result<BootstrapOptions> {
    let mut options = BootstrapOptions::default();
    let mut index = 0usize;

    if let Some(first) = args.first() {
        if !first.starts_with('-') {
            index = 1;
        }
    }

    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        match flag {
            "--workspace" => {
                options.workspace_root = Some(PathBuf::from(next_value(args, &mut index, flag)?));
            }
            "--config" => {
                options.config_path = Some(PathBuf::from(next_value(args, &mut index, flag)?));
            }
            _ if option_takes_value(flag) => {
                let _ = next_value(args, &mut index, flag)?;
            }
            _ => {}
        }
    }

    Ok(options)
}

fn option_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--workspace"
            | "--config"
            | "--session"
            | "--model"
            | "--provider"
            | "--api-base"
            | "--api-key"
            | "--prompt"
            | "--security"
            | "--allow-bin"
            | "--temperature"
            | "--max-output-tokens"
            | "--max-context-tokens"
            | "--max-tool-rounds"
            | "--cycles"
            | "--bind"
    )
}

fn should_refresh_api_base(current_api_base: &str, previous_provider: LlmProvider) -> bool {
    let trimmed = current_api_base.trim();
    trimmed.is_empty() || trimmed == previous_provider.default_api_base()
}

fn should_refresh_api_key(current_api_key: Option<&str>, previous_provider: LlmProvider) -> bool {
    let normalized_current = current_api_key
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let default_key = previous_provider.default_api_key();
    normalized_current.is_none() || normalized_current == default_key.as_deref()
}

fn run_init(runtime: AgentRuntimeConfig) -> Result<()> {
    fs::create_dir_all(&runtime.workspace_root).with_context(|| {
        format!(
            "failed to create workspace directory {}",
            runtime.workspace_root.display()
        )
    })?;
    let workspace = AgentWorkspace::load(runtime.workspace_root)?;
    let created = workspace.ensure_templates()?;

    if created.is_empty() {
        println!(
            "Bootstrap markdown files already exist in {}",
            workspace.bootstrap_root().display()
        );
    } else {
        println!("Created {} bootstrap files:", created.len());
        for path in created {
            println!("- {}", path.display());
        }
    }

    Ok(())
}

fn run_prompt(runtime: AgentRuntimeConfig) -> Result<()> {
    let workspace = AgentWorkspace::load(runtime.workspace_root.clone())?;
    let tools = ToolCatalog::new(workspace.boundary.clone(), runtime.shell_policy.clone(), runtime.search_api_key.clone(), workspace.skills.clone());
    let prompt = PromptComposer::compose(&workspace, &tools.definitions(), &runtime);
    println!("{prompt}");
    Ok(())
}

fn run_auto(mut runtime: AgentRuntimeConfig, options: AutoOptions) -> Result<()> {
    runtime.autonomy.enabled = true;
    let mut app = AgentApp::new(runtime.clone())?;

    if options.daemon {
        println!(
            "Autonomous mode running every {}s (idle backoff {}s)",
            runtime.autonomy.interval_secs, runtime.autonomy.idle_backoff_secs
        );
        loop {
            let report = app.run_autonomy_cycle()?;
            print_autonomy_report(&report);
            thread::sleep(Duration::from_secs(report.next_interval_secs));
        }
    }

    let cycles = options
        .cycles
        .unwrap_or(runtime.autonomy.max_cycles_per_run)
        .max(1);
    for cycle in 0..cycles {
        let report = app.run_autonomy_cycle()?;
        print_autonomy_report(&report);
        if cycle + 1 < cycles {
            thread::sleep(Duration::from_secs(report.next_interval_secs));
        }
    }
    Ok(())
}

fn print_autonomy_report(report: &crate::autonomy::AutonomyCycleReport) {
    let activity = match report.activity {
        AutonomyActivity::Acted => "acted",
        AutonomyActivity::Idle => "idle",
    };
    println!("[autonomy] {activity}: {}", report.focus);
    println!("[autonomy] summary: {}", report.summary);
}

fn next_value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str> {
    let value = args
        .get(*index)
        .map(String::as_str)
        .with_context(|| format!("missing value for {flag}"))?;
    *index += 1;
    Ok(value)
}

fn print_help() {
    println!(
        "Amadeus agent core\n\nUsage:\n  cargo run -- agent [chat] [options]\n  cargo run -- agent auto [options]\n  cargo run -- agent init [options]\n  cargo run -- agent prompt [options]\n  cargo run -- agent serve [--bind ADDR]\n\nOptions:\n  --workspace PATH         Workspace root (defaults to the current directory)\n  --config PATH            Load a JSON agent config (auto-loads .amadeus/config.json)\n  --session ID             Session id for transcript persistence (chat/auto)\n  --provider NAME          openai-chat | openai-responses | anthropic | gemini | ollama\n  --model MODEL            Provider-specific model id\n  --api-base URL           Override the provider base URL\n  --api-key VALUE          Optional API key for the selected provider\n  --prompt TEXT            Run one turn non-interactively and exit (chat only)\n  --security MODE          ask | allowlist | full\n  --allow-bin NAME         Add a baseline-approved executable\n  --allow-shell            Permit shell snippets when policy allows them\n  --temperature FLOAT      Sampling temperature (default 0.2)\n  --max-output-tokens N    Provider output token cap (default 2048)\n  --max-context-tokens N   Approximate session-context budget before compaction (default 16000)\n  --max-tool-rounds N      Tool-call loop cap (default 8)\n  --daemon                 Keep autonomous mode running until interrupted\n  --cycles N               Run N autonomous cycles before exiting (auto only)\n  --bind ADDR              Address to listen on for `serve` (default 127.0.0.1:8765)\n\nEnvironment:\n  AMADEUS_AGENT_PROVIDER\n  AMADEUS_AGENT_MODEL\n  AMADEUS_AGENT_API_BASE\n  AMADEUS_AGENT_API_KEY\n  AMADEUS_AGENT_MAX_OUTPUT_TOKENS\n  AMADEUS_AGENT_MAX_CONTEXT_TOKENS\n  AMADEUS_AGENT_SECURITY\n  AMADEUS_AGENT_ALLOW_SHELL\n  AMADEUS_AGENT_ALLOWED_BINS\n  AMADEUS_AGENT_AUTONOMY\n  AMADEUS_AGENT_AUTONOMY_AUTO_START\n  AMADEUS_AGENT_AUTONOMY_INTERVAL\n  AMADEUS_AGENT_AUTONOMY_IDLE_BACKOFF\n  AMADEUS_AGENT_AUTONOMY_RESEARCH\n  AMADEUS_AGENT_AUTONOMY_RESEARCH_ABSENT_USER_MINS\n  AMADEUS_AGENT_AUTONOMY_RESEARCH_MAX_PENDING_NOTES\n  AMADEUS_AGENT_AUTONOMY_RESEARCH_TOPICS\n  AMADEUS_SERVE_KEY        Bearer token required by the `serve` endpoint (optional)\n  AMADEUS_EXTERNAL_AGENT_URL  Point the native viewer at a remote `serve` instance\n  AMADEUS_EXTERNAL_AGENT_KEY  Bearer token sent to the remote agent server\n\nExamples:\n  cargo run -- agent init\n  cargo run -- agent chat --config .amadeus/config.json\n  cargo run -- agent auto --cycles 2\n  cargo run -- agent auto --daemon\n  cargo run -- agent serve --bind 0.0.0.0:8765\n  AMADEUS_EXTERNAL_AGENT_URL=http://host:8765 cargo run\n  cargo run -- agent chat --provider openai-chat --model gpt-4.1-mini\n  cargo run -- agent chat --provider openai-responses --model gpt-4.1\n  cargo run -- agent chat --provider anthropic --model claude-sonnet-4-20250514\n  cargo run -- agent chat --provider gemini --model gemini-2.5-pro\n  cargo run -- agent chat --provider ollama --model qwen2.5-coder --api-base http://localhost:11434\n  cargo run -- agent prompt --workspace ."
    );
}

#[cfg(test)]
mod tests {
    use super::{AgentCommand, parse_command};

    #[test]
    fn auto_subcommand_enables_autonomy() {
        let command = parse_command(&["auto".to_string()]).expect("auto command should parse");
        match command {
            AgentCommand::Auto(runtime, options) => {
                assert!(runtime.autonomy.enabled);
                assert!(!options.daemon);
                assert!(options.cycles.is_none());
            }
            _ => panic!("expected auto command"),
        }
    }

    #[test]
    fn auto_subcommand_accepts_daemon_and_cycles() {
        let command = parse_command(&[
            "auto".to_string(),
            "--daemon".to_string(),
            "--cycles".to_string(),
            "3".to_string(),
        ])
        .expect("auto command should parse with flags");

        match command {
            AgentCommand::Auto(_, options) => {
                assert!(options.daemon);
                assert_eq!(options.cycles, Some(3));
            }
            _ => panic!("expected auto command"),
        }
    }
}
