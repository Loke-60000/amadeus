use crate::agent::session::{
    AgentSession, SessionAutonomyChemistry, SessionAutonomyDrives,
};

use super::AutonomyActivity;

pub fn derive_chemistry(
    session: &AgentSession,
    idle_minutes: f32,
    recent_failures: usize,
) -> SessionAutonomyChemistry {
    let previous = &session.autonomy.chemistry;
    let failure_pressure = (recent_failures as f32 * 0.14).min(0.58);
    let absence_pressure = (idle_minutes / 240.0).clamp(0.0, 0.55);
    let pending_pressure = if session.autonomy.pending_goal.is_some() {
        0.18
    } else {
        0.0
    };

    SessionAutonomyChemistry {
        excitement: clamp_level(
            previous.excitement * 0.56 + 0.16 + absence_pressure * 0.24 - failure_pressure * 0.22,
        ),
        satisfaction: clamp_level(
            previous.satisfaction * 0.60 + 0.14 + pending_pressure * 0.16 - failure_pressure * 0.34,
        ),
        frustration: clamp_level(previous.frustration * 0.64 + failure_pressure + pending_pressure * 0.22),
        loneliness: clamp_level(previous.loneliness * 0.70 + 0.08 + absence_pressure - previous.satisfaction * 0.12),
        fatigue: clamp_level(previous.fatigue * 0.58 + failure_pressure * 0.34 + if idle_minutes < 45.0 { 0.10 } else { 0.02 }),
    }
}

pub fn derive_drives(
    session: &AgentSession,
    idle_minutes: f32,
    recent_failures: usize,
    chemistry: &SessionAutonomyChemistry,
) -> SessionAutonomyDrives {
    let previous = &session.autonomy.drives;
    let pending_pressure = if session.autonomy.pending_goal.is_some() {
        0.28
    } else {
        0.0
    };
    let failure_pressure = (recent_failures as f32 * 0.14).min(0.42);
    let idle_pressure = (idle_minutes / 90.0).clamp(0.0, 0.45);
    let has_public_history = session.last_public_user_message().is_some();

    SessionAutonomyDrives {
        curiosity: clamp_level(
            previous.curiosity * 0.52 + 0.18 + idle_pressure + chemistry.excitement * 0.22,
        ),
        maintenance: clamp_level(
            previous.maintenance * 0.58 + 0.22 + failure_pressure + chemistry.frustration * 0.18,
        ),
        follow_through: clamp_level(
            previous.follow_through * 0.62
                + 0.18
                + pending_pressure
                + failure_pressure
                + chemistry.satisfaction * 0.08,
        ),
        caution: clamp_level(
            previous.caution * 0.54 + 0.16 + failure_pressure * 0.8 + chemistry.fatigue * 0.18,
        )
        .max(if has_public_history { 0.25 } else { 0.18 }),
    }
}

pub fn settle_chemistry(
    chemistry: &SessionAutonomyChemistry,
    activity: AutonomyActivity,
    recent_failures: usize,
    queued_user_note: bool,
) -> SessionAutonomyChemistry {
    let failure_pressure = (recent_failures as f32 * 0.08).min(0.26);

    match activity {
        AutonomyActivity::Acted => SessionAutonomyChemistry {
            excitement: clamp_level(chemistry.excitement + 0.10),
            satisfaction: clamp_level(chemistry.satisfaction + 0.14),
            frustration: clamp_level(chemistry.frustration - 0.12 + failure_pressure),
            loneliness: clamp_level(chemistry.loneliness + if queued_user_note { -0.08 } else { 0.02 }),
            fatigue: clamp_level(chemistry.fatigue + 0.06 + failure_pressure * 0.4),
        },
        AutonomyActivity::Idle => SessionAutonomyChemistry {
            excitement: clamp_level(chemistry.excitement - 0.08),
            satisfaction: clamp_level(chemistry.satisfaction - 0.04),
            frustration: clamp_level(chemistry.frustration + failure_pressure * 0.7),
            loneliness: clamp_level(chemistry.loneliness + 0.06),
            fatigue: clamp_level(chemistry.fatigue - 0.03),
        },
    }
}

fn clamp_level(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}