pub(crate) mod loader;

use std::path::PathBuf;

/// A skill definition loaded from .amadeus/skills/*.md
#[derive(Clone, Debug)]
pub(crate) struct Skill {
    /// Short snake_case name, e.g. "commit"
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Optional trigger like "/commit"
    pub trigger: Option<String>,
    /// The prompt template body
    pub prompt_template: String,
    /// Source file path
    #[allow(dead_code)]
    pub path: PathBuf,
}

// ── Built-in bundled skills ───────────────────────────────────────────────────

pub(crate) fn bundled_skills() -> Vec<Skill> {
    vec![
        Skill {
            name: "commit".to_string(),
            description: "Create a well-formatted git commit message from staged changes".to_string(),
            trigger: Some("/commit".to_string()),
            prompt_template: r#"Create a git commit for the staged changes.

1. Run `git diff --staged` to see what's staged.
2. If nothing is staged, check `git status` and stage relevant files.
3. Write a commit message: short subject line (under 70 chars), then a body if needed.
4. Run `git commit -m "<message>"`.
5. Report what was committed."#.to_string(),
            path: PathBuf::from("<bundled:commit>"),
        },
        Skill {
            name: "simplify".to_string(),
            description: "Review changed code for quality and simplify where possible".to_string(),
            trigger: Some("/simplify".to_string()),
            prompt_template: r#"Review the recently changed code for quality.

1. Run `git diff HEAD` to see recent changes.
2. Check for: unnecessary complexity, duplicate code, poor naming, missing error handling.
3. Propose and implement simplifications. Don't change behavior — only improve clarity.
4. After changes, run `cargo check` (or equivalent) to ensure nothing is broken."#.to_string(),
            path: PathBuf::from("<bundled:simplify>"),
        },
        Skill {
            name: "remember".to_string(),
            description: "Save something to memory for future sessions".to_string(),
            trigger: Some("/remember".to_string()),
            prompt_template: r#"Save an important fact to the memory system.

The user wants you to remember something. Write it as a concise .md file in .amadeus/memory/.

Choose a meaningful filename like `user_preferences.md` or `project_context.md`.
Format the content as clean markdown with a brief frontmatter description.

After writing, confirm what was saved and where."#.to_string(),
            path: PathBuf::from("<bundled:remember>"),
        },
        Skill {
            name: "update-config".to_string(),
            description: "Configure the Amadeus agent via .amadeus/config.json".to_string(),
            trigger: Some("/update-config".to_string()),
            prompt_template: r#"Help the user update .amadeus/config.json.

1. Read the current config: Read .amadeus/config.json
2. Understand what the user wants to change.
3. Make the change using the Edit tool.
4. Confirm the change and explain what it affects."#.to_string(),
            path: PathBuf::from("<bundled:update-config>"),
        },
    ]
}
