use crate::agent::session::{
    AgentSession, SessionAutonomyChemistry, SessionAutonomyDrives,
};

use super::agenda::AutonomyInitiative;

const INTERNAL_REPLY_SUMMARY_LIMIT: usize = 280;

pub fn choose_focus(
    session: &AgentSession,
    recent_failures: usize,
    drives: &SessionAutonomyDrives,
    chemistry: &SessionAutonomyChemistry,
    initiative: Option<&AutonomyInitiative>,
) -> String {
    if recent_failures > 0 {
        return "resolve recent tool or runtime failures before moving on".to_string();
    }

    if let Some(goal) = session.autonomy.pending_goal.as_deref() {
        return goal.to_string();
    }

    if let Some(initiative) = initiative {
        return initiative_focus(initiative);
    }

    if let Some(last_user) = session.last_public_user_message() {
        return format!(
            "continue the latest user-directed thread without waiting for another prompt: {}",
            compact_excerpt(last_user)
        );
    }

    if drives.maintenance + drives.follow_through + chemistry.frustration
        >= drives.curiosity + chemistry.excitement + 0.10
    {
        "inspect the workspace for unfinished work, missing validation, or obvious breakage"
            .to_string()
    } else if chemistry.loneliness >= 0.55 {
        "prepare a continuity-oriented maintenance or architecture note that will matter to the next human interaction"
            .to_string()
    } else {
        "explore the workspace for the next concrete, defensible improvement".to_string()
    }
}

pub fn build_reason(
    session: &AgentSession,
    idle_minutes: f32,
    recent_failures: usize,
    focus: &str,
    chemistry: &SessionAutonomyChemistry,
    initiative: Option<&AutonomyInitiative>,
) -> String {
    if recent_failures > 0 {
        return format!(
            "{} recent tool/runtime failures remain in session history; follow-through is more important than novelty.",
            recent_failures
        );
    }

    if session.autonomy.pending_goal.is_some() {
        return format!(
            "A pending goal already exists in autonomy state, so this cycle should continue it instead of branching: {focus}."
        );
    }

    if let Some(initiative) = initiative {
        return format!(
            "The user has been absent for about {:.0} minutes, so the {} subagent should take a {} cycle sourced from {}: {}.",
            idle_minutes.max(1.0),
            initiative.subagent.display_name(),
            initiative.kind.display_name(),
            initiative.source,
            initiative.rationale,
        );
    }

    if session.last_public_user_message().is_some() {
        return format!(
            "The user has an existing thread on record and the agent should continue making progress on it after {:.0} idle minutes.",
            idle_minutes.max(1.0)
        );
    }

    format!(
        "No user-visible thread is active; the agent should use its own initiative after {:.0} idle minutes while balancing excitement {:.2}, loneliness {:.2}, and frustration {:.2}.",
        idle_minutes.max(1.0),
        chemistry.excitement,
        chemistry.loneliness,
        chemistry.frustration,
    )
}

fn compact_excerpt(content: &str) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = normalized
        .chars()
        .take(INTERNAL_REPLY_SUMMARY_LIMIT)
        .collect::<String>();
    if normalized.chars().count() > INTERNAL_REPLY_SUMMARY_LIMIT {
        excerpt.push_str("...");
    }
    excerpt
}

fn initiative_focus(initiative: &AutonomyInitiative) -> String {
    match initiative.kind {
        crate::agent::session::SessionAutonomyInitiativeKind::Research => format!(
            "use the {} subagent to investigate {} and leave a concise note if the findings deserve to survive the user's absence",
            initiative.subagent.display_name(),
            initiative.topic,
        ),
        crate::agent::session::SessionAutonomyInitiativeKind::Maintenance => format!(
            "use the {} subagent to spend this cycle on {} and leave the workspace measurably cleaner or better validated",
            initiative.subagent.display_name(),
            initiative.topic,
        ),
        crate::agent::session::SessionAutonomyInitiativeKind::Continuity => format!(
            "use the {} subagent to work on {} so later cycles inherit a cleaner memory of what matters",
            initiative.subagent.display_name(),
            initiative.topic,
        ),
        crate::agent::session::SessionAutonomyInitiativeKind::Review => format!(
            "use the {} subagent to critically examine {} and record any defensible weakness or improvement",
            initiative.subagent.display_name(),
            initiative.topic,
        ),
    }
}