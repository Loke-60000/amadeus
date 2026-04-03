use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::agent::config::default_sessions_dir;

use super::AgentSession;

#[derive(Debug, Clone)]
pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn new(workspace_root: &Path) -> Result<Self> {
        let sessions_dir = default_sessions_dir(workspace_root);
        fs::create_dir_all(&sessions_dir).with_context(|| {
            format!(
                "failed to create session directory {}",
                sessions_dir.display()
            )
        })?;
        Ok(Self { sessions_dir })
    }

    pub fn load_or_create(&self, session_id: &str) -> Result<AgentSession> {
        let path = self.session_path(session_id);
        if path.exists() {
            return load_session_file(&path);
        }

        Ok(AgentSession::new(session_id))
    }

    pub fn save(&self, session: &AgentSession) -> Result<()> {
        let path = self.session_path(&session.id);
        let payload = serde_json::to_string_pretty(session)
            .with_context(|| format!("failed to serialize session {}", session.id))?;
        fs::write(&path, payload)
            .with_context(|| format!("failed to write session file {}", path.display()))
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("{}.json", sanitize_session_id(session_id)))
    }
}

fn load_session_file(path: &Path) -> Result<AgentSession> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read session file {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse session file {}", path.display()))
}

fn sanitize_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::agent::session::AgentSession;

    use super::SessionStore;

    #[test]
    fn load_or_create_reads_dot_amadeus_session_files() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().join("workspace");
        let sessions_dir = workspace_root.join(".amadeus").join("sessions");
        fs::create_dir_all(&sessions_dir)?;
        fs::write(
            sessions_dir.join("desktop-ui.json"),
            r#"{
  "id": "desktop-ui",
  "created_at_ms": 1,
  "updated_at_ms": 2,
  "messages": []
}"#,
        )?;

        let store = SessionStore::new(&workspace_root)?;
        let session = store.load_or_create("desktop-ui")?;

        assert_eq!(session.id, "desktop-ui");
        assert_eq!(session.created_at_ms, 1);
        Ok(())
    }

    #[test]
    fn creates_a_new_session_when_missing() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().join("workspace");
        let store = SessionStore::new(&workspace_root)?;

        let session = store.load_or_create("main")?;

        assert_eq!(session.id, AgentSession::new("main").id);
        Ok(())
    }
}