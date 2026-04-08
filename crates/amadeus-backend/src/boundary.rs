use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::config::AMADEUS_DIR_NAME;

#[derive(Clone, Debug)]
pub struct WorkspaceBoundary {
    root: PathBuf,
}

impl WorkspaceBoundary {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create workspace root at {}", root.display()))?;
        let root = fs::canonicalize(&root)
            .with_context(|| format!("failed to canonicalize workspace root {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_dir(&self, raw: Option<&str>) -> Result<PathBuf> {
        let raw = raw.unwrap_or(".").trim();
        if raw.is_empty() || raw == "." {
            return Ok(self.root.clone());
        }

        let candidate = self.join_candidate(raw);
        let resolved = fs::canonicalize(&candidate)
            .with_context(|| format!("failed to resolve directory {}", candidate.display()))?;
        self.ensure_inside(&resolved)?;
        if !resolved.is_dir() {
            bail!("{} is not a directory", self.display_relative(&resolved));
        }
        Ok(resolved)
    }

    pub fn resolve_existing(&self, raw: &str) -> Result<PathBuf> {
        let candidate = self.join_candidate(raw);
        let resolved = fs::canonicalize(&candidate)
            .with_context(|| format!("failed to resolve {}", candidate.display()))?;
        self.ensure_inside(&resolved)?;
        Ok(resolved)
    }

    pub fn resolve_output(&self, raw: &str) -> Result<PathBuf> {
        let candidate = self.join_candidate(raw);
        let parent = nearest_existing_parent(&candidate)?;
        let resolved_parent = fs::canonicalize(&parent).with_context(|| {
            format!("failed to canonicalize output parent {}", parent.display())
        })?;
        self.ensure_inside(&resolved_parent)?;

        let relative_tail = candidate
            .strip_prefix(&parent)
            .unwrap_or(candidate.as_path());
        Ok(resolved_parent.join(relative_tail))
    }

    pub fn display_relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|relative| {
                let rendered = relative.to_string_lossy().replace('\\', "/");
                if rendered.is_empty() {
                    ".".to_string()
                } else {
                    rendered
                }
            })
            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
    }

    pub fn relative_path<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.root).ok()
    }

    pub fn is_agent_private_path(&self, path: &Path) -> bool {
        let normalized = normalize_lexically(path);
        self.relative_path(&normalized)
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| component.as_os_str() == AMADEUS_DIR_NAME)
    }

    fn join_candidate(&self, raw: &str) -> PathBuf {
        let path = Path::new(raw);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }

    fn ensure_inside(&self, candidate: &Path) -> Result<()> {
        let inside = candidate == self.root || candidate.starts_with(&self.root);
        if inside {
            return Ok(());
        }

        bail!(
            "path {} escapes the workspace root {}",
            candidate.display(),
            self.root.display()
        )
    }
}

fn nearest_existing_parent(candidate: &Path) -> Result<PathBuf> {
    let mut current = candidate.to_path_buf();
    loop {
        if current.exists() {
            return Ok(current);
        }

        let Some(parent) = current.parent() else {
            bail!("{} has no existing parent directory", candidate.display());
        };
        current = parent.to_path_buf();
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }

    normalized
}

#[allow(dead_code)]
fn has_parent_escape(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use super::WorkspaceBoundary;

    #[test]
    fn rejects_paths_that_escape_the_workspace() -> Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside.txt");
        fs::create_dir_all(&workspace)?;
        fs::write(&outside, "hi")?;

        let boundary = WorkspaceBoundary::new(workspace)?;
        let error = boundary
            .resolve_existing(outside.to_string_lossy().as_ref())
            .unwrap_err();
        assert!(error.to_string().contains("escapes the workspace"));
        Ok(())
    }

    #[test]
    fn detects_agent_private_runtime_paths() -> Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path().join("workspace");
        let private_file = workspace.join(".amadeus").join("sessions").join("main.json");
        fs::create_dir_all(private_file.parent().unwrap())?;
        fs::write(&private_file, "{}")?;

        let boundary = WorkspaceBoundary::new(workspace)?;

        assert!(boundary.is_agent_private_path(&private_file));
        assert!(boundary.is_agent_private_path(&boundary.root().join("src/../.amadeus/workspace/SOUL.md")));
        assert!(!boundary.is_agent_private_path(&boundary.root().join("src/main.rs")));
        Ok(())
    }
}
