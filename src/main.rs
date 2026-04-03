mod agent;
mod core;
mod live2d;
mod mcp;
mod tts;

use std::path::PathBuf;

use core::configure_linux_runtime;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NativeViewerLaunchOptions {
    logs_terminal: bool,
}

#[derive(Debug)]
enum RootCommand {
    Agent(Vec<String>),
    LogsWindow(PathBuf),
    Native(NativeViewerLaunchOptions),
    Help,
}

fn main() {
    configure_linux_runtime();

    match parse_root_command(std::env::args().skip(1)) {
        Ok(RootCommand::Agent(remaining)) => {
            if let Err(error) = agent::run_cli(&remaining) {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Ok(RootCommand::LogsWindow(log_file)) => {
            if let Err(error) = core::log_window::run_log_viewer(log_file) {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Ok(RootCommand::Native(options)) => {
            let result = if options.logs_terminal {
                core::native::run_native_viewer_with_logs_terminal(true)
            } else {
                core::native::run_native_viewer()
            };
            if let Err(error) = result {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Ok(RootCommand::Help) => {
            print_root_help();
        }
        Err(error) => {
            eprintln!("{error}\n");
            print_root_help();
            std::process::exit(2);
        }
    }
}

fn parse_root_command<I>(args: I) -> Result<RootCommand, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    match args.next() {
        Some(arg) if arg == "agent" => Ok(RootCommand::Agent(args.collect())),
        Some(arg) if arg == "logs-window" => parse_logs_window_args(args),
        Some(arg) if arg == "native" => parse_native_viewer_args(args),
        Some(arg) if arg == "--help" || arg == "-h" => Ok(RootCommand::Help),
        Some(arg) if is_native_viewer_option(&arg) => parse_native_viewer_args(
            std::iter::once(arg).chain(args),
        ),
        Some(other) => Err(format!("unknown subcommand or option: {other}")),
        None => Ok(RootCommand::Native(NativeViewerLaunchOptions::default())),
    }
}

fn parse_logs_window_args<I>(args: I) -> Result<RootCommand, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mut log_file = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--log-file" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for logs-window --log-file".to_string())?;
                log_file = Some(PathBuf::from(value));
            }
            "--help" | "-h" => return Ok(RootCommand::Help),
            other => return Err(format!("unknown logs-window option: {other}")),
        }
    }

    let log_file =
        log_file.ok_or_else(|| "missing required logs-window --log-file path".to_string())?;
    Ok(RootCommand::LogsWindow(log_file))
}

fn parse_native_viewer_args<I>(args: I) -> Result<RootCommand, String>
where
    I: IntoIterator<Item = String>,
{
    let mut options = NativeViewerLaunchOptions::default();

    for arg in args {
        match arg.as_str() {
            "--logs-terminal" => options.logs_terminal = true,
            "--help" | "-h" => return Ok(RootCommand::Help),
            other => return Err(format!("unknown native option: {other}")),
        }
    }

    Ok(RootCommand::Native(options))
}

fn is_native_viewer_option(arg: &str) -> bool {
    matches!(arg, "--logs-terminal")
}

fn print_root_help() {
    eprintln!(
        "Usage:\n  cargo run\n  cargo run -- --logs-terminal\n  cargo run -- native [--logs-terminal]\n  cargo run -- agent [chat|init|prompt] [options]\n\nNotes:\n  The desktop app now runs the native Cubism renderer only. Bare `cargo run` and `cargo run -- native` are equivalent.\n  `--logs-terminal` opens a second Rust log window named Amadeus-logs for the current run.\n\nExamples:\n  cargo run\n  cargo run -- --logs-terminal\n  cargo run -- native --logs-terminal\n  cargo run -- agent init\n  cargo run -- agent chat --model gpt-4.1-mini\n  cargo run -- agent prompt"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        NativeViewerLaunchOptions, RootCommand, parse_root_command,
    };

    #[test]
    fn bare_logs_terminal_flag_launches_native_viewer() {
        let command = parse_root_command(["--logs-terminal".to_string()]).unwrap();
        match command {
            RootCommand::Native(options) => {
                assert_eq!(
                    options,
                    NativeViewerLaunchOptions {
                        logs_terminal: true,
                    }
                );
            }
            _ => panic!("expected native launch command"),
        }
    }

    #[test]
    fn native_subcommand_accepts_logs_terminal_flag() {
        let command = parse_root_command([
            "native".to_string(),
            "--logs-terminal".to_string(),
        ])
        .unwrap();
        match command {
            RootCommand::Native(options) => assert!(options.logs_terminal),
            _ => panic!("expected native launch command"),
        }
    }

    #[test]
    fn unknown_native_option_is_rejected() {
        let error = parse_root_command(["native".to_string(), "--wat".to_string()])
            .expect_err("unknown option should fail");
        assert!(error.contains("unknown native option"));
    }

    #[test]
    fn logs_window_subcommand_requires_log_file() {
        let error = parse_root_command(["logs-window".to_string()])
            .expect_err("missing log file should fail");
        assert!(error.contains("missing required logs-window --log-file path"));
    }
}
