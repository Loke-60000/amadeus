use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use uuid::Uuid;

pub type TaskId = String;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Killed)
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
        };
        write!(f, "{s}")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    LocalBash,
    LocalAgent,
}

impl std::fmt::Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalBash => write!(f, "local_bash"),
            Self::LocalAgent => write!(f, "local_agent"),
        }
    }
}

/// A background task tracked by the task registry.
#[derive(Clone, Debug)]
pub struct Task {
    pub id: TaskId,
    pub kind: TaskType,
    pub label: String,
    pub status: TaskStatus,
    /// Accumulated output from the running process.
    pub output: Arc<Mutex<String>>,
    pub created_at: u64,
    /// Process ID for local tasks (used by TaskStop).
    pub pid: Option<u32>,
}

impl Task {
    pub fn new(kind: TaskType, label: impl Into<String>) -> Self {
        let id = format!("{}-{}", kind, &Uuid::new_v4().to_string()[..8]);
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            id,
            kind,
            label: label.into(),
            status: TaskStatus::Pending,
            output: Arc::new(Mutex::new(String::new())),
            created_at,
            pid: None,
        }
    }

    pub fn snapshot_output(&self) -> String {
        self.output.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Thread-safe registry of all tasks in this session.
#[derive(Clone, Debug)]
pub struct TaskRegistry {
    tasks: Arc<Mutex<HashMap<TaskId, Task>>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new task and return its ID.
    pub fn create(&self, kind: TaskType, label: impl Into<String>) -> Task {
        let task = Task::new(kind, label);
        let mut map = self.tasks.lock().expect("task registry lock poisoned");
        map.insert(task.id.clone(), task.clone());
        task
    }

    pub fn get(&self, id: &str) -> Option<Task> {
        let map = self.tasks.lock().expect("task registry lock poisoned");
        map.get(id).cloned()
    }

    pub fn list(&self) -> Vec<Task> {
        let map = self.tasks.lock().expect("task registry lock poisoned");
        let mut tasks: Vec<Task> = map.values().cloned().collect();
        tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        tasks
    }

    pub fn update_status(&self, id: &str, status: TaskStatus) {
        let mut map = self.tasks.lock().expect("task registry lock poisoned");
        if let Some(task) = map.get_mut(id) {
            task.status = status;
        }
    }

    pub fn set_pid(&self, id: &str, pid: u32) {
        let mut map = self.tasks.lock().expect("task registry lock poisoned");
        if let Some(task) = map.get_mut(id) {
            task.pid = Some(pid);
        }
    }

    pub fn append_output(&self, id: &str, chunk: &str) {
        let map = self.tasks.lock().expect("task registry lock poisoned");
        if let Some(task) = map.get(id) {
            if let Ok(mut out) = task.output.lock() {
                out.push_str(chunk);
                // Cap output at 2MB
                if out.len() > 2_097_152 {
                    let excess = out.len() - 2_097_152;
                    out.drain(..excess);
                }
            }
        }
    }

    pub fn rename(&self, id: &str, label: impl Into<String>) -> bool {
        let mut map = self.tasks.lock().expect("task registry lock poisoned");
        if let Some(task) = map.get_mut(id) {
            task.label = label.into();
            true
        } else {
            false
        }
    }

    pub fn kill(&self, id: &str) -> bool {
        let map = self.tasks.lock().expect("task registry lock poisoned");
        if let Some(task) = map.get(id) {
            if let Some(pid) = task.pid {
                // SIGTERM the process group
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                return true;
            }
        }
        false
    }
}
