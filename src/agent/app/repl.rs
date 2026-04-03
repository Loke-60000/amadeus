use std::io::{self, Write};

use anyhow::{Context, Result};

use crate::agent::session::SessionVisibility;

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
        println!("Commands: /help, /status, /prompt, /quit\n");

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
                        "Commands:\n  /help   show this help\n  /status show runtime status\n  /prompt print the composed system prompt\n  /quit   exit the agent"
                    );
                }
                "/status" => self.print_status(),
                "/prompt" => println!("{}\n", self.system_prompt()?),
                _ => {
                    let reply = self.run_turn_internal(input, SessionVisibility::Public, None, true)?;
                    println!("\namadeus> {reply}\n");
                }
            }
        }
    }
}