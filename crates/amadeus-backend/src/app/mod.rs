mod repl;
mod turn;

use anyhow::Result;

use std::sync::{Arc, Mutex};

use crate::{
    autonomy::{build_cycle_plan, finalize_cycle, AutonomyCycleReport},
    config::AgentRuntimeConfig,
    llm::{build_model_client, ModelClient, TextStreamSink},
    mcp::{client::McpClient, McpAgentTool},
    prompt::PromptComposer,
    session::{AgentSession, SessionStore},
    tools::ToolCatalog,
    workspace::AgentWorkspace,
};

pub struct AgentApp {
    config: AgentRuntimeConfig,
    workspace: AgentWorkspace,
    session_store: SessionStore,
    session: AgentSession,
    model: Arc<dyn ModelClient + Send + Sync>,
    tools: ToolCatalog,
}

impl AgentApp {
    pub fn new(config: AgentRuntimeConfig) -> Result<Self> {
        let model = Arc::from(build_model_client(&config)?);
        Self::with_client(config, model)
    }

    /// Construct an `AgentApp` with a pre-loaded model client (avoids reloading from disk).
    pub fn with_client(
        config: AgentRuntimeConfig,
        model: Arc<dyn ModelClient + Send + Sync>,
    ) -> Result<Self> {
        let workspace = AgentWorkspace::load(config.workspace_root.clone())?;
        let session_store = SessionStore::new(workspace.boundary.root())?;
        let session = session_store.load_or_create(&config.session_id)?;
        let mut tools = ToolCatalog::new(
            workspace.boundary.clone(),
            config.shell_policy.clone(),
            config.search_api_key.clone(),
            workspace.skills.clone(),
        );

        // Spawn MCP servers and register their tools.
        for (name, cfg) in &config.mcp_servers {
            match McpClient::connect(name, cfg) {
                Err(e) => {
                    eprintln!("[mcp] failed to start server {name}: {e:#}");
                }
                Ok(mut client) => match client.list_tools() {
                    Err(e) => {
                        eprintln!("[mcp] failed to list tools for {name}: {e:#}");
                    }
                    Ok(specs) => {
                        let client = Arc::new(Mutex::new(client));
                        for spec in specs {
                            tools.register(Box::new(McpAgentTool {
                                server_name: name.clone(),
                                spec,
                                client: Arc::clone(&client),
                            }));
                        }
                    }
                },
            }
        }

        Ok(Self {
            config,
            workspace,
            session_store,
            session,
            model,
            tools,
        })
    }

    pub fn run_single_prompt(&mut self, prompt: &str) -> Result<String> {
        self.run_turn_internal(
            prompt,
            crate::session::SessionVisibility::Public,
            None,
            true,
        )
    }

    pub fn run_single_prompt_streaming(
        &mut self,
        prompt: &str,
        stream: &mut dyn TextStreamSink,
    ) -> Result<String> {
        self.run_turn_internal(
            prompt,
            crate::session::SessionVisibility::Public,
            Some(stream),
            true,
        )
    }

    pub fn run_autonomy_cycle(&mut self) -> Result<AutonomyCycleReport> {
        self.workspace.reload()?;
        let plan = build_cycle_plan(&self.session, &self.workspace, &self.config.autonomy);
        let reply = self.run_turn_internal(
            &plan.prompt,
            crate::session::SessionVisibility::Internal,
            None,
            false,
        )?;
        let report = finalize_cycle(&mut self.session, &self.config.autonomy, &plan, &reply);
        self.session_store.save(&self.session)?;
        Ok(report)
    }

    pub fn session(&self) -> &AgentSession {
        &self.session
    }

    fn system_prompt(&self) -> Result<String> {
        Ok(PromptComposer::compose(
            &self.workspace,
            &self.tools.definitions(),
            &self.config,
        ))
    }

    fn print_status(&self) {
        println!("session : {}", self.session.id);
        println!("messages: {}", self.session.messages.len());
        println!("provider: {}", self.config.provider);
        println!(
            "model   : {}",
            self.config.model.as_deref().unwrap_or("(unset)")
        );
        println!("api base: {}", self.config.api_base);
        println!("security: {}", self.config.shell_policy.mode);
        println!(
            "workspace bootstrap files: {}",
            self.workspace.bootstrap_files.len()
        );
        println!();
    }
}
