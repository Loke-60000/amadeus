use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet};
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use walkdir::{DirEntry, WalkDir};

use super::catalog::{AgentTool, ToolContext, ToolDefinition, ToolOutcome};

pub(crate) struct LsTool;

impl AgentTool for LsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "LS",
            "List files and directories inside a workspace path.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative directory path. Defaults to workspace root." },
                    "recursive": { "type": "boolean", "description": "Whether to recurse into subdirectories." },
                    "max_entries": { "type": "integer", "minimum": 1, "maximum": 500 }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: LsArgs =
            serde_json::from_value(input).context("invalid LS arguments")?;
        let recursive = args.recursive.unwrap_or(false);
        let max_entries = args.max_entries.unwrap_or(200).clamp(1, 500);
        let directory = ctx.resolve_tool_dir(args.path.as_deref())?;

        let entries = if recursive {
            let mut entries = Vec::new();
            for entry in WalkDir::new(&directory)
                .min_depth(1)
                .into_iter()
                .filter_entry(|entry| !is_ignored_dir(entry))
                .flatten()
            {
                if entries.len() >= max_entries {
                    break;
                }
                let relative = ctx.display_relative(entry.path());
                entries.push(json!({
                    "path": relative,
                    "kind": entry_kind(&entry),
                }));
            }
            entries
        } else {
            let mut entries = fs::read_dir(&directory)
                .with_context(|| format!("failed to list {}", directory.display()))?
                .flatten()
                .filter(|entry| !is_ignored_name(&entry.file_name().to_string_lossy()))
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());

            entries
                .into_iter()
                .take(max_entries)
                .map(|entry| {
                    let path = entry.path();
                    json!({
                        "path": ctx.display_relative(&path),
                        "kind": file_type_kind(path.as_path()),
                    })
                })
                .collect()
        };

        Ok(ToolOutcome::new(
            format!(
                "Listed {} entries from {}",
                entries.len(),
                ctx.display_relative(&directory)
            ),
            json!({
                "path": ctx.display_relative(&directory),
                "entries": entries,
            }),
        ))
    }
}

pub(crate) struct GlobTool;

impl AgentTool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Glob",
            "Fast file pattern matching. Supports glob patterns like **/*.rs or plain substrings.",
            json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern like src/**/*.rs or a plain substring to match against file paths." },
                    "path": { "type": "string", "description": "Optional workspace-relative search root." },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 500 }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: GlobArgs =
            serde_json::from_value(input).context("invalid Glob arguments")?;
        let root = ctx.resolve_tool_dir(args.path.as_deref())?;
        let max_results = args.max_results.unwrap_or(200).clamp(1, 500);
        let matcher = compile_optional_glob(&args.pattern)?;
        let plain_pattern = args.pattern.to_ascii_lowercase();

        let mut matches = Vec::new();
        for entry in WalkDir::new(&root)
            .into_iter()
            .filter_entry(|entry| !is_ignored_dir(entry))
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = ctx.display_relative(entry.path());
            let normalized = relative.to_ascii_lowercase();
            let is_match = matcher
                .as_ref()
                .map(|glob| glob.is_match(&relative))
                .unwrap_or_else(|| normalized.contains(&plain_pattern));
            if is_match {
                matches.push(relative);
                if matches.len() >= max_results {
                    break;
                }
            }
        }

        Ok(ToolOutcome::new(
            format!("Found {} files matching {}", matches.len(), args.pattern),
            json!({
                "pattern": args.pattern,
                "matches": matches,
            }),
        ))
    }
}

pub(crate) struct ReadTool;

impl AgentTool for ReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Read",
            "Read the contents of a file. Returns the text with line numbers. Use offset and limit for large files.",
            json!({
                "type": "object",
                "required": ["file_path"],
                "properties": {
                    "file_path": { "type": "string", "description": "Workspace-relative path to the file to read." },
                    "offset": { "type": "integer", "minimum": 1, "description": "1-based line number to start reading from." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to return." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: ReadArgs =
            serde_json::from_value(input).context("invalid Read arguments")?;
        let path = ctx.resolve_tool_existing(&args.file_path)?;
        let content = read_text_file(&path)?;
        let lines: Vec<&str> = content.lines().collect();

        let start_line = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(2000);
        let end_line = (start_line + limit - 1).min(lines.len().max(1));

        let selected = if lines.is_empty() {
            String::new()
        } else {
            let start = (start_line - 1).min(lines.len());
            let end = end_line.min(lines.len());
            lines[start..end]
                .iter()
                .enumerate()
                .map(|(i, line)| format!("{}\t{}", start_line + i, line))
                .collect::<Vec<_>>()
                .join("\n")
        };

        Ok(ToolOutcome::new(
            format!(
                "Read {} lines from {}",
                end_line.saturating_sub(start_line).saturating_add(1),
                ctx.display_relative(&path)
            ),
            json!({
                "file_path": ctx.display_relative(&path),
                "offset": start_line,
                "limit": limit,
                "total_lines": lines.len(),
                "content": selected,
            }),
        ))
    }
}

pub(crate) struct GrepTool;

impl AgentTool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Grep",
            "Search for text patterns across workspace files. Supports regex and glob file filters.",
            json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression or plain text to search for." },
                    "path": { "type": "string", "description": "Workspace-relative directory or file to search within." },
                    "glob": { "type": "string", "description": "Glob pattern to filter files, e.g. \"*.rs\" or \"src/**/*.ts\"." },
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count"],
                        "description": "content: show matching lines; files_with_matches: show file paths only; count: show match counts. Defaults to content."
                    },
                    "head_limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Max results to return (default 100)." },
                    "-i": { "type": "boolean", "description": "Case insensitive search." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: GrepArgs =
            serde_json::from_value(input).context("invalid Grep arguments")?;
        let root = ctx.resolve_tool_dir(args.path.as_deref())?;
        let max_results = args.head_limit.unwrap_or(100).clamp(1, 500);
        let file_filter = args.glob.as_deref().map(compile_glob).transpose()?;
        let case_insensitive = args.case_insensitive.unwrap_or(false);
        let output_mode = args.output_mode.as_deref().unwrap_or("content");

        let regex = RegexBuilder::new(&args.pattern)
            .case_insensitive(case_insensitive)
            .build()
            .with_context(|| format!("invalid regex pattern {:?}", args.pattern))?;

        let mut matches = Vec::new();
        let mut files_with_matches: Vec<String> = Vec::new();
        let mut count_map: Vec<(String, usize)> = Vec::new();
        let mut total = 0usize;

        'outer: for entry in WalkDir::new(&root)
            .into_iter()
            .filter_entry(|entry| !is_ignored_dir(entry))
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let relative = ctx.display_relative(entry.path());
            if let Some(glob) = &file_filter {
                if !glob.is_match(&relative) {
                    continue;
                }
            }

            let Ok(content) = read_text_file(entry.path()) else {
                continue;
            };

            match output_mode {
                "files_with_matches" => {
                    if regex.is_match(&content) {
                        files_with_matches.push(relative);
                        total += 1;
                        if total >= max_results {
                            break 'outer;
                        }
                    }
                }
                "count" => {
                    let count = regex.find_iter(&content).count();
                    if count > 0 {
                        count_map.push((relative, count));
                        total += 1;
                        if total >= max_results {
                            break 'outer;
                        }
                    }
                }
                _ => {
                    for (index, line) in content.lines().enumerate() {
                        if regex.is_match(line) {
                            matches.push(json!({
                                "path": relative,
                                "line": index + 1,
                                "text": line,
                            }));
                            total += 1;
                            if total >= max_results {
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }

        let (summary, data) = match output_mode {
            "files_with_matches" => (
                format!("Found {} files matching {}", files_with_matches.len(), args.pattern),
                json!({ "pattern": args.pattern, "files": files_with_matches }),
            ),
            "count" => (
                format!("Found {} files with matches for {}", count_map.len(), args.pattern),
                json!({ "pattern": args.pattern, "counts": count_map }),
            ),
            _ => (
                format!("Found {} matches for {}", matches.len(), args.pattern),
                json!({ "pattern": args.pattern, "matches": matches }),
            ),
        };

        Ok(ToolOutcome::new(summary, data))
    }
}

pub(crate) struct WriteTool;

impl AgentTool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Write",
            "Create or overwrite a file with the provided content. Creates parent directories automatically.",
            json!({
                "type": "object",
                "required": ["file_path", "content"],
                "properties": {
                    "file_path": { "type": "string", "description": "Workspace-relative path to write." },
                    "content": { "type": "string", "description": "Full file content to write." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: WriteArgs =
            serde_json::from_value(input).context("invalid Write arguments")?;
        let path = ctx.resolve_tool_output(&args.file_path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory {}", parent.display())
            })?;
        }

        fs::write(&path, &args.content)
            .with_context(|| format!("failed to write {}", path.display()))?;

        Ok(ToolOutcome::new(
            format!("Wrote {}", ctx.display_relative(&path)),
            json!({
                "file_path": ctx.display_relative(&path),
                "bytes": args.content.len(),
            }),
        ))
    }
}

pub(crate) struct EditTool;

impl AgentTool for EditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Edit",
            "Replace an exact string in a file. The old_string must match exactly once unless replace_all is true.",
            json!({
                "type": "object",
                "required": ["file_path", "old_string", "new_string"],
                "properties": {
                    "file_path": { "type": "string", "description": "Workspace-relative path to the file to edit." },
                    "old_string": { "type": "string", "description": "Exact text to find and replace." },
                    "new_string": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences instead of requiring exactly one." }
                }
            }),
        )
    }

    fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: EditArgs =
            serde_json::from_value(input).context("invalid Edit arguments")?;
        let path = ctx.resolve_tool_existing(&args.file_path)?;
        let content = read_text_file(&path)?;
        let match_count = content.match_indices(&args.old_string).count();
        let replace_all = args.replace_all.unwrap_or(false);

        if match_count == 0 {
            bail!(
                "old_string was not found in {}",
                ctx.display_relative(&path)
            );
        }
        if !replace_all && match_count != 1 {
            bail!(
                "old_string must match exactly once in {} (found {} matches); use replace_all: true to replace all",
                ctx.display_relative(&path),
                match_count
            );
        }

        let updated = if replace_all {
            content.replace(&args.old_string, &args.new_string)
        } else {
            content.replacen(&args.old_string, &args.new_string, 1)
        };
        fs::write(&path, updated)
            .with_context(|| format!("failed to write {}", path.display()))?;

        Ok(ToolOutcome::new(
            format!("Updated {}", ctx.display_relative(&path)),
            json!({
                "file_path": ctx.display_relative(&path),
                "replacements": if replace_all { match_count } else { 1 },
            }),
        ))
    }
}

// ── Argument structs ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LsArgs {
    path: Option<String>,
    recursive: Option<bool>,
    max_entries: Option<usize>,
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    path: Option<String>,
    max_results: Option<usize>,
}

#[derive(Deserialize)]
struct ReadArgs {
    file_path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    output_mode: Option<String>,
    head_limit: Option<usize>,
    #[serde(rename = "-i")]
    case_insensitive: Option<bool>,
}

#[derive(Deserialize)]
struct WriteArgs {
    file_path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn read_text_file(path: &Path) -> Result<String> {
    const MAX_TEXT_BYTES: usize = 512 * 1024;
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.len() as usize > MAX_TEXT_BYTES {
        bail!("{} is larger than {} bytes", path.display(), MAX_TEXT_BYTES);
    }
    fs::read_to_string(path)
        .with_context(|| format!("failed to read {} as UTF-8 text", path.display()))
}

fn compile_optional_glob(pattern: &str) -> Result<Option<GlobSet>> {
    if !pattern.contains('*')
        && !pattern.contains('?')
        && !pattern.contains('[')
        && !pattern.contains('{')
    {
        return Ok(None);
    }
    compile_glob(pattern).map(Some)
}

fn compile_glob(pattern: &str) -> Result<GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    builder.add(Glob::new(pattern).with_context(|| format!("invalid glob pattern {pattern:?}"))?);
    builder.build().context("failed to build glob matcher")
}

fn is_ignored_dir(entry: &DirEntry) -> bool {
    entry.depth() > 0 && is_ignored_name(&entry.file_name().to_string_lossy())
}

fn is_ignored_name(name: &str) -> bool {
    matches!(name, ".amadeus" | ".git" | "node_modules" | "target")
}

fn entry_kind(entry: &DirEntry) -> &'static str {
    file_type_kind(entry.path())
}

fn file_type_kind(path: &Path) -> &'static str {
    if path.is_dir() {
        "dir"
    } else if path.is_file() {
        "file"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{
        boundary::WorkspaceBoundary,
        config::ShellPolicyConfig,
        tools::catalog::{AgentTool, ToolContext},
    };

    use super::LsTool;

    #[test]
    fn ls_hides_agent_private_runtime_entries() -> Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(workspace.join("src"))?;
        fs::create_dir_all(workspace.join(".amadeus").join("workspace"))?;
        fs::write(workspace.join("src").join("main.rs"), "fn main() {}")?;
        fs::write(
            workspace.join(".amadeus").join("workspace").join("SOUL.md"),
            "persona",
        )?;

        let ctx = ToolContext::new(
            WorkspaceBoundary::new(workspace)?,
            ShellPolicyConfig::default(),
        );
        let outcome = LsTool.invoke(json!({}), &ctx)?;
        let entries = outcome.payload["entries"].as_array().expect("entries array");
        let listed_paths = entries
            .iter()
            .filter_map(|entry| entry.get("path").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();

        assert!(listed_paths.iter().any(|path| *path == "src"));
        assert!(!listed_paths.iter().any(|path| *path == ".amadeus"));
        Ok(())
    }
}
