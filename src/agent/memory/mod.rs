use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

/// A single file loaded from the memory directory.
#[derive(Clone, Debug)]
pub struct MemoryFile {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
}

/// Manages AMADEUS.md and the .amadeus/memory/ directory.
#[derive(Clone, Debug)]
pub struct MemorySystem {
    workspace_root: PathBuf,
    memory_dir: PathBuf,
}

impl MemorySystem {
    pub fn new(workspace_root: PathBuf) -> Self {
        let memory_dir = workspace_root.join(".amadeus").join("memory");
        Self { workspace_root, memory_dir }
    }

    /// Reads AMADEUS.md from the workspace root (project-level instructions).
    pub fn load_amadeus_md(&self) -> Option<String> {
        let path = self.workspace_root.join("AMADEUS.md");
        fs::read_to_string(&path).ok()
    }

    /// Reads all .md files from .amadeus/memory/.
    pub fn load_memory_files(&self) -> Vec<MemoryFile> {
        if !self.memory_dir.is_dir() {
            return Vec::new();
        }

        let mut files: Vec<MemoryFile> = fs::read_dir(&self.memory_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry.path().extension().and_then(|ext| ext.to_str()) == Some("md")
            })
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_string_lossy().into_owned();
                let content = fs::read_to_string(&path).ok()?;
                Some(MemoryFile { name, path, content })
            })
            .collect();

        files.sort_by(|a, b| a.name.cmp(&b.name));
        files
    }

    /// Write or overwrite a named memory file in .amadeus/memory/.
    pub fn write_memory(&self, name: &str, content: &str) -> Result<()> {
        fs::create_dir_all(&self.memory_dir)
            .with_context(|| format!("failed to create {}", self.memory_dir.display()))?;

        let filename = if name.ends_with(".md") {
            name.to_string()
        } else {
            format!("{name}.md")
        };

        let path = self.memory_dir.join(&filename);
        fs::write(&path, content)
            .with_context(|| format!("failed to write memory file {}", path.display()))
    }

    /// List memory file names.
    pub fn list_memory_files(&self) -> Vec<String> {
        self.load_memory_files()
            .into_iter()
            .map(|f| f.name)
            .collect()
    }

    pub fn memory_dir(&self) -> &Path {
        &self.memory_dir
    }
}
