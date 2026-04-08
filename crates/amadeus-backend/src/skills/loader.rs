use std::{fs, path::Path};

use crate::skills::Skill;

/// Parse a skill from a markdown file with optional YAML-like frontmatter.
///
/// Format:
/// ```markdown
/// ---
/// name: commit
/// description: Create a well-formatted git commit
/// trigger: /commit
/// ---
/// Body of the skill prompt...
/// ```
pub(crate) fn parse_skill_file(path: &Path, content: &str) -> Option<Skill> {
    let (frontmatter, body) = split_frontmatter(content);

    let name = frontmatter
        .as_ref()
        .and_then(|fm| parse_fm_field(fm, "name"))
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string))?;

    let description = frontmatter
        .as_ref()
        .and_then(|fm| parse_fm_field(fm, "description"))
        .unwrap_or_else(|| format!("Skill: {name}"));

    let trigger = frontmatter
        .as_ref()
        .and_then(|fm| parse_fm_field(fm, "trigger"));

    Some(Skill {
        name,
        description,
        trigger,
        prompt_template: body.trim().to_string(),
        path: path.to_path_buf(),
    })
}

pub(crate) fn load_skills_dir(dir: &Path) -> Vec<Skill> {
    if !dir.is_dir() {
        return Vec::new();
    }

    let mut skills: Vec<Skill> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry.path().extension().and_then(|e| e.to_str()) == Some("md")
        })
        .filter_map(|entry| {
            let path = entry.path();
            let content = fs::read_to_string(&path).ok()?;
            parse_skill_file(&path, &content)
        })
        .collect();

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return (None, content.to_string());
    }

    let after_open = content.trim_start_matches("---").trim_start_matches('\n');
    if let Some(close) = after_open.find("---") {
        let fm = after_open[..close].to_string();
        let body = after_open[close..].trim_start_matches("---").to_string();
        (Some(fm), body)
    } else {
        (None, content.to_string())
    }
}

fn parse_fm_field(frontmatter: &str, field: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&format!("{field}:")) {
            let value = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}
