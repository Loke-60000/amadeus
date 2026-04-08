use std::sync::{Arc, Mutex};

/// Whether the agent is currently in planning mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum PlanMode {
    #[default]
    Off,
    Active,
}

/// A question the agent wants to ask the user interactively.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct PendingQuestion {
    pub question: String,
    pub options: Vec<String>,
}

/// Shared mutable planning state passed through the agent.
#[derive(Clone, Debug)]
pub struct PlanningState {
    mode: Arc<Mutex<PlanMode>>,
    /// Question waiting to be shown to the user.
    pending_question: Arc<Mutex<Option<PendingQuestion>>>,
    /// Answer delivered by the UI after the user responds.
    pending_answer: Arc<Mutex<Option<String>>>,
}

#[allow(dead_code)]
impl PlanningState {
    pub fn new() -> Self {
        Self {
            mode: Arc::new(Mutex::new(PlanMode::Off)),
            pending_question: Arc::new(Mutex::new(None)),
            pending_answer: Arc::new(Mutex::new(None)),
        }
    }

    pub fn mode(&self) -> PlanMode {
        *self.mode.lock().expect("plan mode lock poisoned")
    }

    pub fn set_mode(&self, mode: PlanMode) {
        *self.mode.lock().expect("plan mode lock poisoned") = mode;
    }

    /// Post a question for the UI to display.
    pub fn post_question(&self, question: String, options: Vec<String>) {
        *self.pending_question.lock().expect("pending question lock poisoned") =
            Some(PendingQuestion { question, options });
        // Clear any stale previous answer
        *self.pending_answer.lock().expect("pending answer lock poisoned") = None;
    }

    /// Called by UI when the user has responded.
    pub fn deliver_answer(&self, answer: String) {
        *self.pending_answer.lock().expect("pending answer lock poisoned") = Some(answer);
        // Clear the question since it's been answered
        *self.pending_question.lock().expect("pending question lock poisoned") = None;
    }

    /// Called by UI to inspect the current pending question.
    pub fn take_pending_question(&self) -> Option<PendingQuestion> {
        self.pending_question.lock().expect("pending question lock poisoned").take()
    }

    pub fn has_pending_question(&self) -> bool {
        self.pending_question.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// Block the calling thread until an answer arrives or the timeout elapses.
    /// Returns `Some(answer)` on success, `None` on timeout.
    pub fn wait_for_answer(&self, timeout_secs: u64) -> Option<String> {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            if let Ok(mut guard) = self.pending_answer.lock() {
                if guard.is_some() {
                    return guard.take();
                }
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
