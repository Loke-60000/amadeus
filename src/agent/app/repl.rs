use std::io::{self, Write};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::agent::config::AgentRuntimeConfig;
use crate::agent::llm::build_model_client;
use crate::agent::session::SessionVisibility;
use crate::agent::settings_command::{settings_help, SettingsCommand};

use super::AgentApp;

impl AgentApp {
    pub fn run_interactive(&mut self) -> Result<()> {
        println!(
            "Amadeus agent core\nworkspace: {}\nsession: {}\nprovider: {}\nmodel: {}\n",
            self.workspace.boundary.root().display(),
            self.session.id,
            self.config.provider,
            self.config.model.as_deref().unwrap_or("(unset)")
        );
        println!("Commands: /help, /status, /prompt, /settings, /quit\n");

        let stdin = io::stdin();
        loop {
            print!("you> ");
            io::stdout().flush().ok();

            let mut input = String::new();
            let read = stdin
                .read_line(&mut input)
                .context("failed to read stdin")?;
            if read == 0 {
                println!();
                return Ok(());
            }

            let input = input.trim();
            if input.is_empty() {
                continue;
            }

            match input {
                "/quit" | "/exit" => return Ok(()),
                "/help" => {
                    println!(
                        "Commands:\n  /help     show this help\n  /status   show runtime status\n  /prompt   print the composed system prompt\n  /settings manage service toggles\n  /quit     exit the agent"
                    );
                }
                "/status" => self.print_status(),
                "/prompt" => println!("{}\n", self.system_prompt()?),
                "/settings" | "/settings help" => println!("{}\n", settings_help()),
                _ if input.starts_with("/settings ") => {
                    let args = input["/settings ".len()..].trim();
                    match SettingsCommand::parse(args) {
                        Ok(cmd) => match cmd.apply(&self.config.workspace_root) {
                            Ok(msg) => {
                                // Reload config from disk and rebuild the model client so
                                // provider/model/api-base/context changes take effect immediately.
                                let workspace_root = self.config.workspace_root.clone();
                                match AgentRuntimeConfig::load(Some(workspace_root), None) {
                                    Ok(new_config) => match build_model_client(&new_config) {
                                        Ok(new_model) => {
                                            self.config = new_config;
                                            self.model = Arc::from(new_model);
                                            println!("{msg}\n");
                                        }
                                        Err(e) => {
                                            println!("{msg}\nWarning: could not rebuild model client: {e:#}\n");
                                        }
                                    },
                                    Err(e) => {
                                        println!(
                                            "{msg}\nWarning: could not reload config: {e:#}\n"
                                        );
                                    }
                                }
                            }
                            Err(err) => println!("error: {err}\n"),
                        },
                        Err(err) => println!("error: {err}\n"),
                    }
                }
                _ => {
                    let reply =
                        self.run_turn_internal(input, SessionVisibility::Public, None, true)?;
                    println!("\namadeus> {reply}\n");
                }
            }
        }
    }
}
