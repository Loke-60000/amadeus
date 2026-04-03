mod agenda;
mod chemistry;
mod focus;
mod research;

use crate::agent::{
    config::AutonomyConfig,
    context::tool_message_counts_as_failure,
    session::{
        AgentSession, SessionAutonomyChemistry, SessionAutonomyDrives,
        SessionAutonomyInitiativeKind, SessionRole,
    },
    workspace::AgentWorkspace,
};

use agenda::{
    AutonomyInitiative, choose_initiative, interest_backlog_hint, record_initiative_interest,
};
use chemistry::{derive_chemistry, derive_drives, settle_chemistry};
use focus::{build_reason, choose_focus};
use research::{extract_user_notes, retain_pending_user_notes};

const INTERNAL_REPLY_SUMMARY_LIMIT: usize = 280;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AutonomyActivity {
    Acted,
    Idle,
}

#[derive(Debug, Clone)]
pub struct AutonomyCyclePlan {
    pub prompt: String,
    pub focus: String,
    pub recent_failures: usize,
    pub drives: SessionAutonomyDrives,
    pub chemistry: SessionAutonomyChemistry,
    pub initiative: Option<AutonomyInitiative>,
}

#[derive(Debug, Clone)]
pub struct AutonomyCycleReport {
    pub activity: AutonomyActivity,
    pub focus: String,
    pub summary: String,
    pub next_interval_secs: u64,
}

pub fn build_cycle_plan(
    session: &AgentSession,
    workspace: &AgentWorkspace,
    config: &AutonomyConfig,
) -> AutonomyCyclePlan {
    let recent_failures = count_recent_tool_failures(session, 12);
    let idle_minutes = minutes_since(
        session
            .autonomy
            .last_user_message_ms
            .unwrap_or(session.updated_at_ms),
    );
    let chemistry = derive_chemistry(session, idle_minutes, recent_failures);
    let drives = derive_drives(session, idle_minutes, recent_failures, &chemistry);
    let initiative = choose_initiative(
        session,
        workspace,
        config,
        idle_minutes,
        recent_failures,
        &drives,
        &chemistry,
    );
    let focus = choose_focus(
        session,
        recent_failures,
        &drives,
        &chemistry,
        initiative.as_ref(),
    );
    let reason = build_reason(
        session,
        idle_minutes,
        recent_failures,
        &focus,
        &chemistry,
        initiative.as_ref(),
    );
    let last_user = session
        .last_public_user_message()
        .map(compact_excerpt)
        .unwrap_or_else(|| "No visible user request exists yet.".to_string());
    let last_assistant = session
        .last_public_assistant_message()
        .map(compact_excerpt)
        .unwrap_or_else(|| "No visible assistant reply exists yet.".to_string());
    let pending_goal = session
        .autonomy
        .pending_goal
        .clone()
        .unwrap_or_else(|| "none".to_string());
    let workspace_hint = workspace.boundary.display_relative(workspace.boundary.root());
    let initiative_topic_hint = initiative
        .as_ref()
        .map(|initiative| initiative.topic.as_str())
        .unwrap_or("none");
    let initiative_kind_hint = initiative
        .as_ref()
        .map(|initiative| initiative.kind.display_name())
        .unwrap_or("none");
    let initiative_source_hint = initiative
        .as_ref()
        .map(|initiative| initiative.source.as_str())
        .unwrap_or("none");
    let subagent_hint = initiative
        .as_ref()
        .map(|initiative| initiative.subagent.display_name())
        .unwrap_or("none");
    let subagent_charter_hint = initiative
        .as_ref()
        .map(|initiative| initiative.subagent.charter())
        .unwrap_or("No self-directed subagent is active for this cycle.");
    let initiative_rationale_hint = initiative
        .as_ref()
        .map(|initiative| initiative.rationale.as_str())
        .unwrap_or("none");
    let interest_backlog = interest_backlog_hint(session);

    let prompt = format!(
        "Autonomy cycle #{}.
You are running an internal initiative cycle without a fresh human prompt.

Mission:
- Continue only work that is justified by the current workspace, recent failures, pending goals, or the last visible user request.
- Prefer follow-through, validation, repair, and concrete research over unrelated new projects.
- If the user has been absent and no active repair work dominates, you may originate a Kurisu-consistent subject of research or another disciplined way to spend the cycle.
- Use the selected internal subagent as the operating stance for this cycle.
- Self-directed time should look like science, skepticism, engineering, or continuity work, not empty filler.
- Use tools if and only if they create concrete progress.
- If there is nothing concrete and defensible to do right now, reply exactly as `IDLE: <reason>`.
- If a cycle produces something the user should hear later, add a final line exactly as `USER_NOTE: <topic> :: <note>`.
- If you act, finish with a compact internal journal note stating what changed, what remains, and the next focus.

Current focus: {focus}
Why now: {reason}
Cadence: act every {}s when productive, back off to {}s when idle.
Workspace root: {workspace_hint}
Pending goal: {pending_goal}
Selected initiative kind: {initiative_kind_hint}
Selected initiative source: {initiative_source_hint}
Selected internal subagent: {subagent_hint}
Subagent charter: {subagent_charter_hint}
Initiative rationale: {initiative_rationale_hint}
Suggested self-directed subject: {initiative_topic_hint}
Interest backlog: {interest_backlog}
Last visible user request: {last_user}
Last visible assistant reply: {last_assistant}
Recent tool/runtime failures: {recent_failures}
Drive levels:
- curiosity: {:.2}
- maintenance: {:.2}
- follow_through: {:.2}
- caution: {:.2}
Chemistry snapshot:
- excitement: {:.2}
- satisfaction: {:.2}
- frustration: {:.2}
- loneliness: {:.2}
- fatigue: {:.2}

Behavioral constraints:
- Do not invent unrelated greenfield work.
- If prior work appears incomplete, continue or validate it.
- If you originate your own initiative, keep it anchored to this workspace, the user's long-running interests, or Kurisu-aligned scientific concerns such as memory, causality, neuroscience, or research ethics.
- If you detect a blocker, gather evidence and either fix it or leave a concise internal note describing the blocker.
- Stay compatible with the existing framework rather than replacing it with a disconnected prototype.",
        session.autonomy.cycle_count + 1,
        config.interval_secs,
        config.idle_backoff_secs,
        drives.curiosity,
        drives.maintenance,
        drives.follow_through,
        drives.caution,
        chemistry.excitement,
        chemistry.satisfaction,
        chemistry.frustration,
        chemistry.loneliness,
        chemistry.fatigue,
    );

    AutonomyCyclePlan {
        prompt,
        focus,
        recent_failures,
        drives,
        chemistry,
        initiative,
    }
}

pub fn finalize_cycle(
    session: &mut AgentSession,
    config: &AutonomyConfig,
    plan: &AutonomyCyclePlan,
    reply: &str,
) -> AutonomyCycleReport {
    let now = now_ms();
    let trimmed_reply = reply.trim();
    let notes = extract_user_notes(
        trimmed_reply,
        plan.initiative.as_ref().map(|initiative| initiative.topic.as_str()),
        config.research.max_pending_notes,
    );
    let reply_without_notes = strip_user_notes(trimmed_reply);
    let (activity, summary) = if let Some(reason) = reply_without_notes.strip_prefix("IDLE:") {
        (AutonomyActivity::Idle, compact_excerpt(reason.trim()))
    } else {
        (AutonomyActivity::Acted, compact_excerpt(reply_without_notes.trim()))
    };

    session.autonomy.cycle_count += 1;
    session.autonomy.last_cycle_ms = Some(now);
    session.autonomy.current_focus = Some(plan.focus.clone());
    session.autonomy.drives = plan.drives.clone();
    session.autonomy.recent_failure_count = plan.recent_failures as u32;
    session.autonomy.last_outcome = Some(summary.clone());
    session.autonomy.chemistry = settle_chemistry(
        &plan.chemistry,
        activity,
        plan.recent_failures,
        !notes.is_empty(),
    );

    if !notes.is_empty() {
        retain_pending_user_notes(
            &mut session.autonomy.pending_user_notes,
            notes,
            config.research.max_pending_notes,
        );
    }

    if activity == AutonomyActivity::Acted {
        if let Some(initiative) = &plan.initiative {
            session.autonomy.last_initiative_topic = Some(initiative.topic.clone());
            session.autonomy.last_initiative_kind = Some(initiative.kind);
            session.autonomy.last_subagent = Some(initiative.subagent);
            record_initiative_interest(session, initiative, now);

            if initiative.kind == SessionAutonomyInitiativeKind::Research {
                session.autonomy.last_research_topic = Some(initiative.topic.clone());
                session.autonomy.last_research_ms = Some(now);
            }
        }
    }

    match activity {
        AutonomyActivity::Idle => {
            session.autonomy.idle_streak = session.autonomy.idle_streak.saturating_add(1);
            if plan.recent_failures == 0 && plan.initiative.is_none() {
                session.autonomy.pending_goal = None;
            }
        }
        AutonomyActivity::Acted => {
            session.autonomy.idle_streak = 0;
            session.autonomy.pending_goal = match plan.initiative.as_ref().map(|initiative| initiative.kind) {
                Some(SessionAutonomyInitiativeKind::Research) => None,
                _ => Some(plan.focus.clone()),
            };
        }
    }

    let next_interval_secs = match activity {
        AutonomyActivity::Acted => config.interval_secs,
        AutonomyActivity::Idle => config.idle_backoff_secs,
    };

    AutonomyCycleReport {
        activity,
        focus: plan.focus.clone(),
        summary,
        next_interval_secs,
    }
}

fn count_recent_tool_failures(session: &AgentSession, limit: usize) -> usize {
    session
        .messages
        .iter()
        .rev()
        .filter(|message| matches!(message.role, SessionRole::Tool))
        .take(limit)
        .filter(|message| tool_message_counts_as_failure(message))
        .count()
}

fn minutes_since(timestamp_ms: u64) -> f32 {
    let now = now_ms();
    let elapsed_ms = now.saturating_sub(timestamp_ms);
    elapsed_ms as f32 / 60_000.0
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

fn strip_user_notes(reply: &str) -> String {
    let filtered = reply
        .lines()
        .filter(|line| !line.trim().starts_with("USER_NOTE:"))
        .collect::<Vec<_>>()
        .join("\n");
    filtered.trim().to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crate::agent::{
        autonomy::{AutonomyActivity, build_cycle_plan, finalize_cycle},
        config::{AutonomyConfig, AutonomyResearchConfig},
        session::{AgentSession, SessionVisibility},
        workspace::AgentWorkspace,
    };

    use anyhow::Result;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn pending_goal_is_preferred_for_focus() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join(".amadeus").join("workspace"))?;
        let workspace = AgentWorkspace::load(root)?;
        let mut session = AgentSession::new("main");
        session.autonomy.pending_goal = Some("finish validating the native logs window".to_string());

        let plan = build_cycle_plan(&session, &workspace, &AutonomyConfig::default());
        assert!(plan.focus.contains("finish validating the native logs window"));
        Ok(())
    }

    #[test]
    fn absent_user_cycles_can_switch_to_research() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join(".amadeus").join("workspace"))?;
        let workspace = AgentWorkspace::load(root)?;
        let mut session = AgentSession::new("main");
        session.autonomy.last_user_message_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64
                - 4 * 60 * 60 * 1000,
        );
        let config = AutonomyConfig {
            research: AutonomyResearchConfig {
                enabled: true,
                absent_user_minutes: 30,
                max_pending_notes: 4,
                topics: vec!["context compaction and memory discipline".to_string()],
            },
            ..AutonomyConfig::default()
        };

        let plan = build_cycle_plan(&session, &workspace, &config);
        assert!(plan.focus.contains("context compaction and memory discipline"));
        assert!(plan.initiative.is_some());
        Ok(())
    }

    #[test]
    fn absent_user_cycles_can_generate_self_directed_persona_work() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join(".amadeus").join("workspace"))?;
        let workspace = AgentWorkspace::load(root)?;
        let mut session = AgentSession::new("main");
        session.autonomy.last_user_message_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64
                - 4 * 60 * 60 * 1000,
        );
        let config = AutonomyConfig {
            research: AutonomyResearchConfig {
                enabled: true,
                absent_user_minutes: 30,
                max_pending_notes: 4,
                topics: Vec::new(),
            },
            ..AutonomyConfig::default()
        };

        let plan = build_cycle_plan(&session, &workspace, &config);
        assert!(plan.initiative.is_some());
        assert!(plan.focus.contains("subagent"));
        Ok(())
    }

    #[test]
    fn finalize_cycle_extracts_user_notes() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join(".amadeus").join("workspace"))?;
        let workspace = AgentWorkspace::load(root)?;
        let mut session = AgentSession::new("main");
        let config = AutonomyConfig {
            research: AutonomyResearchConfig {
                enabled: true,
                absent_user_minutes: 30,
                max_pending_notes: 4,
                topics: vec!["agent continuity".to_string()],
            },
            ..AutonomyConfig::default()
        };

        session.autonomy.last_user_message_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64
                - 4 * 60 * 60 * 1000,
        );
        let plan = build_cycle_plan(&session, &workspace, &config);
        let report = finalize_cycle(
            &mut session,
            &config,
            &plan,
            "Inspected the agent runtime and found a continuity gap.\nUSER_NOTE: agent continuity :: The agent needs persisted context compaction and deferred research notes.",
        );

        assert_eq!(report.activity, AutonomyActivity::Acted);
        assert_eq!(session.autonomy.pending_user_notes.len(), 1);
        assert!(session.autonomy.pending_user_notes[0]
            .note
            .contains("persisted context compaction"));
        assert_eq!(session.autonomy.interests.len(), 1);
        assert!(session.autonomy.last_subagent.is_some());
        Ok(())
    }

    #[test]
    fn idle_cycles_increment_idle_streak() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join(".amadeus").join("workspace"))?;
        let workspace = AgentWorkspace::load(root)?;
        let mut session = AgentSession::new("main");
        let config = AutonomyConfig::default();
        let plan = build_cycle_plan(&session, &workspace, &config);
        let report = finalize_cycle(&mut session, &config, &plan, "IDLE: no concrete work");

        assert_eq!(report.activity, AutonomyActivity::Idle);
        assert_eq!(session.autonomy.idle_streak, 1);
        assert_eq!(report.next_interval_secs, config.idle_backoff_secs);
        Ok(())
    }

    #[test]
    fn benign_tool_rejections_do_not_lock_autonomy_focus() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join(".amadeus").join("workspace"))?;
        let workspace = AgentWorkspace::load(root)?;
        let mut session = AgentSession::new("main");
        session.push_tool_message_with_visibility(
            "call-1",
            "list_dir",
            r#"{"ok":false,"error":".amadeus/sessions/main.json is not a directory"}"#,
            SessionVisibility::Internal,
        );

        let plan = build_cycle_plan(&session, &workspace, &AutonomyConfig::default());

        assert_eq!(plan.recent_failures, 0);
        assert!(!plan.focus.contains("recent tool or runtime failures"));
        Ok(())
    }
}