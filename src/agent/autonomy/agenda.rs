use std::{
    cmp::Ordering,
    collections::HashSet,
    path::Path,
};

use crate::agent::{
    config::AutonomyConfig,
    session::{
        AgentSession, SessionAutonomyChemistry, SessionAutonomyDrives,
        SessionAutonomyInitiativeKind, SessionAutonomyInterest, SessionAutonomySubagent,
    },
    workspace::AgentWorkspace,
};

use super::research::research_topic_hints;

const MAX_INTERESTS: usize = 12;
const TOPIC_LIMIT: usize = 120;
const RATIONALE_LIMIT: usize = 180;
const BACKLOG_HINT_COUNT: usize = 3;

#[derive(Debug, Clone)]
pub struct AutonomyInitiative {
    pub topic: String,
    pub rationale: String,
    pub source: String,
    pub kind: SessionAutonomyInitiativeKind,
    pub subagent: SessionAutonomySubagent,
}

#[derive(Debug, Clone)]
struct InitiativeCandidate {
    initiative: AutonomyInitiative,
    score: f32,
}

impl SessionAutonomyInitiativeKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Maintenance => "maintenance",
            Self::Continuity => "continuity",
            Self::Review => "review",
        }
    }
}

impl SessionAutonomySubagent {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Scientist => "Scientist",
            Self::Engineer => "Engineer",
            Self::Archivist => "Archivist",
            Self::Skeptic => "Skeptic",
        }
    }

    pub fn charter(self) -> &'static str {
        match self {
            Self::Scientist => {
                "Frame a specific question, separate hypotheses from evidence, and prefer technically serious inquiry over idle novelty."
            }
            Self::Engineer => {
                "Spend the cycle making the workspace measurably better through repair, validation, or architecture-level cleanup."
            }
            Self::Archivist => {
                "Protect continuity: consolidate notes, preserve important context, and leave later cycles a cleaner state to inherit."
            }
            Self::Skeptic => {
                "Challenge assumptions, look for weak reasoning or hidden failure modes, and prefer criticism with evidence over agreeable drift."
            }
        }
    }
}

pub fn choose_initiative(
    session: &AgentSession,
    workspace: &AgentWorkspace,
    config: &AutonomyConfig,
    idle_minutes: f32,
    recent_failures: usize,
    drives: &SessionAutonomyDrives,
    chemistry: &SessionAutonomyChemistry,
) -> Option<AutonomyInitiative> {
    let absent_gate = idle_minutes >= config.research.absent_user_minutes as f32;
    let no_visible_thread = session.last_public_user_message().is_none();
    if recent_failures > 0 || session.autonomy.pending_goal.is_some() || (!absent_gate && !no_visible_thread) {
        return None;
    }

    if let Some(initiative) = configured_priority_initiative(session, config, absent_gate) {
        return Some(initiative);
    }

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for initiative in backlog_initiatives(session) {
        push_candidate(
            &mut candidates,
            &mut seen,
            initiative,
            session,
            drives,
            chemistry,
            absent_gate,
        );
    }
    for initiative in continuity_initiatives(session) {
        push_candidate(
            &mut candidates,
            &mut seen,
            initiative,
            session,
            drives,
            chemistry,
            absent_gate,
        );
    }
    for initiative in workspace_initiatives(workspace) {
        push_candidate(
            &mut candidates,
            &mut seen,
            initiative,
            session,
            drives,
            chemistry,
            absent_gate,
        );
    }
    for initiative in persona_initiatives(config.research.enabled) {
        push_candidate(
            &mut candidates,
            &mut seen,
            initiative,
            session,
            drives,
            chemistry,
            absent_gate,
        );
    }
    if config.research.enabled {
        for initiative in research_initiatives(session, config) {
            push_candidate(
                &mut candidates,
                &mut seen,
                initiative,
                session,
                drives,
                chemistry,
                absent_gate,
            );
        }
    }
    if absent_gate {
        for initiative in dormant_user_initiatives(session) {
            push_candidate(
                &mut candidates,
                &mut seen,
                initiative,
                session,
                drives,
                chemistry,
                absent_gate,
            );
        }
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.initiative.topic.cmp(&right.initiative.topic))
    });

    candidates.into_iter().next().map(|candidate| candidate.initiative)
}

pub fn record_initiative_interest(
    session: &mut AgentSession,
    initiative: &AutonomyInitiative,
    now_ms: u64,
) {
    let topic = compact_excerpt(&initiative.topic, TOPIC_LIMIT);
    let rationale = compact_excerpt(&initiative.rationale, RATIONALE_LIMIT);
    if let Some(existing) = session
        .autonomy
        .interests
        .iter_mut()
        .find(|interest| same_topic(&interest.topic, &topic))
    {
        existing.rationale = rationale;
        existing.source = initiative.source.clone();
        existing.kind = initiative.kind;
        existing.subagent = initiative.subagent;
        existing.last_selected_ms = Some(now_ms);
        existing.selection_count = existing.selection_count.saturating_add(1);
    } else {
        session.autonomy.interests.push(SessionAutonomyInterest {
            topic,
            rationale,
            source: initiative.source.clone(),
            kind: initiative.kind,
            subagent: initiative.subagent,
            last_selected_ms: Some(now_ms),
            selection_count: 1,
        });
    }

    session.autonomy.interests.sort_by(|left, right| {
        right
            .last_selected_ms
            .cmp(&left.last_selected_ms)
            .then_with(|| right.selection_count.cmp(&left.selection_count))
            .then_with(|| left.topic.cmp(&right.topic))
    });
    if session.autonomy.interests.len() > MAX_INTERESTS {
        session.autonomy.interests.truncate(MAX_INTERESTS);
    }
}

pub fn interest_backlog_hint(session: &AgentSession) -> String {
    if session.autonomy.interests.is_empty() {
        return "none".to_string();
    }

    session
        .autonomy
        .interests
        .iter()
        .take(BACKLOG_HINT_COUNT)
        .map(|interest| {
            format!(
                "{} [{} via {}]",
                compact_excerpt(&interest.topic, 72),
                interest.kind.display_name(),
                interest.subagent.display_name()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn push_candidate(
    candidates: &mut Vec<InitiativeCandidate>,
    seen: &mut HashSet<String>,
    initiative: AutonomyInitiative,
    session: &AgentSession,
    drives: &SessionAutonomyDrives,
    chemistry: &SessionAutonomyChemistry,
    absent_gate: bool,
) {
    if initiative.topic.trim().is_empty() {
        return;
    }

    let key = normalize_topic(&initiative.topic);
    if !seen.insert(key) {
        return;
    }

    let score = candidate_score(&initiative, session, drives, chemistry, absent_gate);
    candidates.push(InitiativeCandidate { initiative, score });
}

fn configured_priority_initiative(
    session: &AgentSession,
    config: &AutonomyConfig,
    absent_gate: bool,
) -> Option<AutonomyInitiative> {
    if !absent_gate || !config.research.enabled || config.research.topics.is_empty() {
        return None;
    }

    let mut index = session.autonomy.cycle_count as usize % config.research.topics.len();
    if let Some(last_topic) = session.autonomy.last_research_topic.as_deref() {
        if config.research.topics.len() > 1 && same_topic(&config.research.topics[index], last_topic) {
            index = (index + 1) % config.research.topics.len();
        }
    }

    let topic = config.research.topics[index].clone();
    let (kind, subagent) = infer_kind_and_subagent(&topic);
    Some(AutonomyInitiative {
        topic,
        rationale: "An explicit configured research topic should outrank generic self-generated ideas during absent-user cycles."
            .to_string(),
        source: "configured".to_string(),
        kind,
        subagent,
    })
}

fn backlog_initiatives(session: &AgentSession) -> Vec<AutonomyInitiative> {
    session
        .autonomy
        .interests
        .iter()
        .map(|interest| AutonomyInitiative {
            topic: interest.topic.clone(),
            rationale: format!(
                "Resume a previously selected self-directed thread instead of constantly discarding continuity. {}",
                interest.rationale
            ),
            source: "backlog".to_string(),
            kind: interest.kind,
            subagent: interest.subagent,
        })
        .collect()
}

fn continuity_initiatives(session: &AgentSession) -> Vec<AutonomyInitiative> {
    let mut initiatives = vec![AutonomyInitiative {
        topic: "continuity notes, memory consolidation, and what the next cycle should inherit"
            .to_string(),
        rationale: "When the user is absent, idle time should preserve continuity rather than pretending to be busy."
            .to_string(),
        source: "continuity".to_string(),
        kind: SessionAutonomyInitiativeKind::Continuity,
        subagent: SessionAutonomySubagent::Archivist,
    }];

    if !session.autonomy.pending_user_notes.is_empty() {
        initiatives.push(AutonomyInitiative {
            topic: "synthesize queued autonomous findings into a tighter note for the next visible user turn"
                .to_string(),
            rationale: "Deferred notes should become clearer and less redundant before they are delivered to the user."
                .to_string(),
            source: "continuity".to_string(),
            kind: SessionAutonomyInitiativeKind::Continuity,
            subagent: SessionAutonomySubagent::Archivist,
        });
    }

    if session.context.compacted_message_count > 0 {
        initiatives.push(AutonomyInitiative {
            topic: "audit whether context compaction is hiding assumptions or dropping important continuity"
                .to_string(),
            rationale: "Compression is useful, but a skeptical pass is justified once older context is being summarized away."
                .to_string(),
            source: "continuity".to_string(),
            kind: SessionAutonomyInitiativeKind::Review,
            subagent: SessionAutonomySubagent::Skeptic,
        });
    }

    initiatives
}

fn workspace_initiatives(workspace: &AgentWorkspace) -> Vec<AutonomyInitiative> {
    let root = workspace.boundary.root();
    let mut initiatives = vec![AutonomyInitiative {
        topic: "unfinished validation, warning cleanup, or obvious continuity gaps in this workspace"
            .to_string(),
        rationale: "A Kurisu-like autonomous cycle should prefer defensible maintenance over theatrical activity."
            .to_string(),
        source: "workspace".to_string(),
        kind: SessionAutonomyInitiativeKind::Maintenance,
        subagent: SessionAutonomySubagent::Engineer,
    }];

    if root.join("src").join("agent").is_dir() {
        initiatives.push(AutonomyInitiative {
            topic: "the Rust agent core's autonomy architecture, tool discipline, and failure recovery"
                .to_string(),
            rationale: "The workspace contains a dedicated agent runtime, so inspecting its autonomy model is a defensible use of absence-time cycles."
                .to_string(),
            source: "workspace".to_string(),
            kind: SessionAutonomyInitiativeKind::Review,
            subagent: SessionAutonomySubagent::Skeptic,
        });
    }

    if path_exists(root, &["src", "tts"]) {
        initiatives.push(AutonomyInitiative {
            topic: "streaming voice latency, bilingual routing, and audio stability in the local TTS runtime"
                .to_string(),
            rationale: "Local speech is part of the user-facing loop here, so stability and latency are legitimate engineering subjects."
                .to_string(),
            source: "workspace".to_string(),
            kind: SessionAutonomyInitiativeKind::Maintenance,
            subagent: SessionAutonomySubagent::Engineer,
        });
    }

    if path_exists(root, &["src", "core"]) || path_exists(root, &["src", "ui"]) {
        initiatives.push(AutonomyInitiative {
            topic: "human-visible feedback loops, native or UI coherence, and runtime ergonomics in the desktop app"
                .to_string(),
            rationale: "The workspace mixes runtime and UI concerns, so coherence across those layers is a defensible place to spend unsupervised cycles."
                .to_string(),
            source: "workspace".to_string(),
            kind: SessionAutonomyInitiativeKind::Maintenance,
            subagent: SessionAutonomySubagent::Engineer,
        });
    }

    if path_exists(root, &["src", "live2d"]) {
        initiatives.push(AutonomyInitiative {
            topic: "interaction latency and behavioral coherence between the Live2D shell and the autonomous agent runtime"
                .to_string(),
            rationale: "Embodied presentation should not drift away from the underlying agent behavior if the project wants continuity."
                .to_string(),
            source: "workspace".to_string(),
            kind: SessionAutonomyInitiativeKind::Review,
            subagent: SessionAutonomySubagent::Skeptic,
        });
    }

    initiatives
}

fn persona_initiatives(research_enabled: bool) -> Vec<AutonomyInitiative> {
    let mut initiatives = vec![AutonomyInitiative {
        topic: "research ethics, operator autonomy, and non-manipulative behavior in an always-on system"
            .to_string(),
        rationale: "Kurisu's personality should treat ethics and operator autonomy as part of the engineering problem, not decorative philosophy."
            .to_string(),
        source: "persona".to_string(),
        kind: SessionAutonomyInitiativeKind::Review,
        subagent: SessionAutonomySubagent::Skeptic,
    }];

    if research_enabled {
        initiatives.extend([
            AutonomyInitiative {
                topic: "memory continuity, introspective limits, and honest self-report for Amadeus"
                    .to_string(),
                rationale: "Kurisu-adjacent curiosity should naturally return to memory, self-report, and what claims about continuity are actually justified."
                    .to_string(),
                source: "persona".to_string(),
                kind: SessionAutonomyInitiativeKind::Research,
                subagent: SessionAutonomySubagent::Scientist,
            },
            AutonomyInitiative {
                topic: "causal planning, time-order constraints, and long-horizon autonomy loops in this agent"
                    .to_string(),
                rationale: "A Kurisu-like research agenda should care about temporal structure and long-horizon causal failure modes, not just superficial productivity."
                    .to_string(),
                source: "persona".to_string(),
                kind: SessionAutonomyInitiativeKind::Research,
                subagent: SessionAutonomySubagent::Scientist,
            },
        ]);
    }

    initiatives
}

fn research_initiatives(session: &AgentSession, config: &AutonomyConfig) -> Vec<AutonomyInitiative> {
    research_topic_hints(session, config)
        .into_iter()
        .map(|topic| {
            let (kind, subagent) = infer_kind_and_subagent(&topic);
            AutonomyInitiative {
                topic,
                rationale: "A self-directed cycle can originate its own subject if it remains grounded in this workspace, the user's long-running context, or Kurisu-consistent scientific concerns."
                    .to_string(),
                source: if config.research.topics.is_empty() {
                    "persona".to_string()
                } else {
                    "configured".to_string()
                },
                kind,
                subagent,
            }
        })
        .collect()
}

fn dormant_user_initiatives(session: &AgentSession) -> Vec<AutonomyInitiative> {
    let Some(last_user) = session.last_public_user_message() else {
        return Vec::new();
    };

    let topic = format!(
        "the dormant user thread: {}",
        compact_excerpt(last_user, 96)
    );
    let (kind, subagent) = infer_kind_and_subagent(last_user);
    vec![AutonomyInitiative {
        topic,
        rationale: "Even self-directed autonomy should preserve continuity with the user's last serious line of inquiry when there is one."
            .to_string(),
        source: "user".to_string(),
        kind,
        subagent,
    }]
}

fn infer_kind_and_subagent(topic: &str) -> (SessionAutonomyInitiativeKind, SessionAutonomySubagent) {
    let normalized = topic.to_ascii_lowercase();
    if contains_any(
        &normalized,
        &["ethic", "audit", "review", "skeptic", "assumption", "risk", "failure", "warning"],
    ) {
        return (
            SessionAutonomyInitiativeKind::Review,
            SessionAutonomySubagent::Skeptic,
        );
    }
    if contains_any(
        &normalized,
        &["continuity", "memory", "context", "note", "session", "inherit", "handoff"],
    ) {
        return (
            SessionAutonomyInitiativeKind::Continuity,
            SessionAutonomySubagent::Archivist,
        );
    }
    if contains_any(
        &normalized,
        &["validate", "cleanup", "repair", "latency", "stability", "architecture", "runtime", "workspace", "tool", "warning"],
    ) {
        return (
            SessionAutonomyInitiativeKind::Maintenance,
            SessionAutonomySubagent::Engineer,
        );
    }
    (
        SessionAutonomyInitiativeKind::Research,
        SessionAutonomySubagent::Scientist,
    )
}

fn candidate_score(
    initiative: &AutonomyInitiative,
    session: &AgentSession,
    drives: &SessionAutonomyDrives,
    chemistry: &SessionAutonomyChemistry,
    absent_gate: bool,
) -> f32 {
    let mut score = match initiative.kind {
        SessionAutonomyInitiativeKind::Research => {
            drives.curiosity * 0.95 + chemistry.excitement * 0.28 + chemistry.loneliness * 0.14
        }
        SessionAutonomyInitiativeKind::Maintenance => {
            drives.maintenance * 0.86 + drives.follow_through * 0.34 + drives.caution * 0.10
        }
        SessionAutonomyInitiativeKind::Continuity => {
            chemistry.loneliness * 0.82 + drives.follow_through * 0.30 + chemistry.satisfaction * 0.08
        }
        SessionAutonomyInitiativeKind::Review => {
            drives.caution * 0.76 + chemistry.frustration * 0.28 + drives.curiosity * 0.18
        }
    };

    score += match initiative.subagent {
        SessionAutonomySubagent::Scientist => drives.curiosity * 0.08,
        SessionAutonomySubagent::Engineer => drives.maintenance * 0.08,
        SessionAutonomySubagent::Archivist => chemistry.loneliness * 0.12 + drives.follow_through * 0.04,
        SessionAutonomySubagent::Skeptic => drives.caution * 0.10 + chemistry.frustration * 0.04,
    };

    if absent_gate {
        score += 0.12;
    }
    if session.last_public_user_message().is_none()
        && matches!(
            initiative.kind,
            SessionAutonomyInitiativeKind::Research | SessionAutonomyInitiativeKind::Continuity
        )
    {
        score += 0.08;
    }

    score += match initiative.source.as_str() {
        "workspace" => 0.08,
        "persona" => 0.06,
        "configured" => 0.22,
        "backlog" => 0.10,
        "user" => 0.12,
        "continuity" => 0.09,
        _ => 0.0,
    };

    if session
        .autonomy
        .last_initiative_topic
        .as_deref()
        .is_some_and(|topic| same_topic(topic, &initiative.topic))
    {
        score -= 0.40;
    }
    if matches!(initiative.kind, SessionAutonomyInitiativeKind::Research)
        && session
            .autonomy
            .last_research_topic
            .as_deref()
            .is_some_and(|topic| same_topic(topic, &initiative.topic))
    {
        score -= 0.25;
    }
    if session.autonomy.last_subagent == Some(initiative.subagent) {
        score -= 0.12;
    }
    if session.autonomy.last_initiative_kind == Some(initiative.kind) {
        score -= 0.08;
    }

    if !session.autonomy.pending_user_notes.is_empty()
        && initiative.kind == SessionAutonomyInitiativeKind::Continuity
    {
        score += 0.20;
    }
    if session.context.compacted_message_count > 0
        && matches!(
            initiative.kind,
            SessionAutonomyInitiativeKind::Continuity | SessionAutonomyInitiativeKind::Review
        )
    {
        score += 0.14;
    }

    if let Some(interest) = session
        .autonomy
        .interests
        .iter()
        .find(|interest| same_topic(&interest.topic, &initiative.topic))
    {
        score += 0.06 + (interest.selection_count.min(3) as f32) * 0.02;
        if let Some(last_selected_ms) = interest.last_selected_ms {
            score += revisit_bonus(last_selected_ms);
        }
    } else {
        score += 0.05;
    }

    score
}

fn revisit_bonus(last_selected_ms: u64) -> f32 {
    let minutes = minutes_since(last_selected_ms);
    if minutes >= 720.0 {
        0.12
    } else if minutes >= 240.0 {
        0.08
    } else if minutes >= 60.0 {
        0.04
    } else {
        -0.04
    }
}

fn path_exists(root: &Path, components: &[&str]) -> bool {
    let mut path = root.to_path_buf();
    for component in components {
        path = path.join(component);
    }
    path.exists()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn same_topic(left: &str, right: &str) -> bool {
    normalize_topic(left) == normalize_topic(right)
}

fn normalize_topic(topic: &str) -> String {
    topic
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn compact_excerpt(content: &str, limit: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = normalized.chars().take(limit).collect::<String>();
    if normalized.chars().count() > limit {
        excerpt.push_str("...");
    }
    excerpt
}

fn minutes_since(timestamp_ms: u64) -> f32 {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    now_ms.saturating_sub(timestamp_ms) as f32 / 60_000.0
}
