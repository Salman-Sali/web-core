use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use web_core::error::Error;

use crate::protocol::{AttemptId, RunnerId, TaskId};

/// Settings of a runner, set in admin and pushed to the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerSettings {
    /// Hard maximum of concurrent attempts. 0 pauses the runner.
    pub capacity: u32,
    /// Tier: lower numbers are filled first.
    pub priority: i32,
    /// Share within a tier: the runner with the lowest `running / weight` gets the next task.
    pub weight: u32,
}

impl Default for RunnerSettings {
    /// A freshly registered runner: capacity 1, priority 0, weight 1.
    fn default() -> Self {
        Self {
            capacity: 1,
            priority: 0,
            weight: 1,
        }
    }
}

/// Connection state of a runner, as shown in admin and stored in `runners.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    /// Not connected.
    #[default]
    Offline,
    /// Connected, contract matches, accepting work.
    Online,
    /// Connected with a different contract or protocol; gets no work.
    Outdated,
    /// Connected but not accepting work (admin drain or runner shutdown).
    Draining,
}

impl RunnerState {
    /// The stored string (`offline`, `online`, `outdated`, `draining`).
    pub fn as_str(&self) -> &'static str {
        match self {
            RunnerState::Offline => "offline",
            RunnerState::Online => "online",
            RunnerState::Outdated => "outdated",
            RunnerState::Draining => "draining",
        }
    }
}

/// What the pool learned about a runner from its connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunnerStatus {
    /// Current state.
    pub state: RunnerState,
    /// Last message received (app time).
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Protocol version from the last hello.
    pub protocol_version: Option<u32>,
    /// Contract hash from the last hello.
    pub contract_hash: Option<String>,
    /// Tool versions from the last hello.
    pub tool_versions: BTreeMap<String, String>,
}

/// A `runners` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerRecord {
    /// Id (also the first half of the token).
    pub id: RunnerId,
    /// Display name from admin.
    pub name: String,
    /// Argon2 hash of the token's secret half.
    pub token_hash: String,
    /// Capacity, priority, weight.
    pub settings: RunnerSettings,
    /// Disabled runners may connect but get no work.
    pub enabled: bool,
    /// Registration time.
    pub created_at: DateTime<Utc>,
    /// Last known connection status.
    pub status: RunnerStatus,
}

/// Status of a submitted task (`runner_tasks.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Waiting for a runner.
    Queued,
    /// An attempt is running.
    Running,
    /// Finished with an output.
    Done,
    /// Failed (task failure or attempts exhausted).
    Failed,
    /// Cancelled by the submitter.
    Cancelled,
}

impl TaskStatus {
    /// The stored string.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

/// A `runner_tasks` row (history; the live task is in memory).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    /// Id.
    pub id: TaskId,
    /// Task type.
    pub task_type: String,
    /// Input JSON as submitted.
    pub input: Value,
    /// Status.
    pub status: TaskStatus,
    /// Attempts allowed.
    pub max_attempts: u32,
    /// Submitter's reference (a job id), for "attempt history of this job".
    pub reference: Option<String>,
    /// Submission time.
    pub created_at: DateTime<Utc>,
    /// End time.
    pub finished_at: Option<DateTime<Utc>>,
    /// Output JSON on success.
    pub output: Option<Value>,
    /// Failure reason.
    pub failure: Option<String>,
}

/// Status of an attempt (`runner_attempts.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    /// Executing.
    Running,
    /// Lease expired or the runner left; the task gets another attempt.
    Lost,
    /// Cancelled by the app.
    Aborted,
    /// Succeeded.
    Done,
    /// The task reported a failure.
    Failed,
    /// The task process died, timed out or a transfer failed (`error` says which).
    Crashed,
}

impl AttemptStatus {
    /// The stored string.
    pub fn as_str(&self) -> &'static str {
        match self {
            AttemptStatus::Running => "running",
            AttemptStatus::Lost => "lost",
            AttemptStatus::Aborted => "aborted",
            AttemptStatus::Done => "done",
            AttemptStatus::Failed => "failed",
            AttemptStatus::Crashed => "crashed",
        }
    }
}

/// A `runner_attempts` row. Written when the attempt starts, on each checkpoint and when it
/// ends (not on every lease renewal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptRecord {
    /// Id.
    pub id: AttemptId,
    /// The task.
    pub task_id: TaskId,
    /// 1-based attempt number.
    pub number: u32,
    /// The runner ([`RunnerId::LOCAL`] for the local fallback, which has no `runners` row).
    pub runner_id: RunnerId,
    /// Runner name at the time (survives revocation).
    pub runner_name: String,
    /// Lease expiry when last written (app time).
    pub lease_until: DateTime<Utc>,
    /// Status.
    pub status: AttemptStatus,
    /// Last reported percent.
    pub progress: f32,
    /// Last checkpoint reported by this attempt.
    pub checkpoint: Option<Value>,
    /// Start time (app time).
    pub started_at: DateTime<Utc>,
    /// End time (app time).
    pub ended_at: Option<DateTime<Utc>>,
    /// What went wrong.
    pub error: Option<String>,
}

/// Persistence the app implements with its own database (the crate owns no tables; see the
/// README for the expected shape). Live pool state stays in memory; this is configuration and
/// history. Status and history writes are queued and applied in order by a background task, so
/// a slow database never blocks dispatch; failures are logged.
#[async_trait]
pub trait RunnerStore: Send + Sync + 'static {
    /// All registered runners (read once at pool start).
    async fn load_runners(&self) -> Result<Vec<RunnerRecord>, Error>;
    /// Inserts or updates a runner's name, token hash, settings and enabled flag.
    async fn save_runner(&self, runner: &RunnerRecord) -> Result<(), Error>;
    /// Deletes a revoked runner.
    async fn delete_runner(&self, id: RunnerId) -> Result<(), Error>;
    /// Updates a runner's state, last seen time, protocol, contract hash and tool versions.
    async fn update_runner_status(&self, id: RunnerId, status: &RunnerStatus)
    -> Result<(), Error>;
    /// Inserts or updates a task row.
    async fn save_task(&self, task: &TaskRecord) -> Result<(), Error>;
    /// Inserts or updates an attempt row.
    async fn save_attempt(&self, attempt: &AttemptRecord) -> Result<(), Error>;
    /// A task with its attempts (oldest first), for admin.
    async fn task_history(
        &self,
        task_id: TaskId,
    ) -> Result<Option<(TaskRecord, Vec<AttemptRecord>)>, Error>;
}

/// An in-memory [`RunnerStore`] for tests.
#[derive(Default)]
pub struct MemoryStore {
    runners: Mutex<HashMap<RunnerId, RunnerRecord>>,
    tasks: Mutex<HashMap<TaskId, TaskRecord>>,
    attempts: Mutex<Vec<AttemptRecord>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stored runners.
    pub fn runners(&self) -> Vec<RunnerRecord> {
        self.runners
            .lock()
            .map(|r| r.values().cloned().collect())
            .unwrap_or_default()
    }

    /// One stored runner.
    pub fn runner(&self, id: RunnerId) -> Option<RunnerRecord> {
        self.runners.lock().ok().and_then(|r| r.get(&id).cloned())
    }

    /// Stored tasks.
    pub fn tasks(&self) -> Vec<TaskRecord> {
        self.tasks
            .lock()
            .map(|t| t.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Stored attempts, in insertion order.
    pub fn attempts(&self) -> Vec<AttemptRecord> {
        self.attempts.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

fn poisoned() -> Error {
    Error::new_something_went_wrong("memory store lock poisoned".into())
}

#[async_trait]
impl RunnerStore for MemoryStore {
    async fn load_runners(&self) -> Result<Vec<RunnerRecord>, Error> {
        Ok(self.runners())
    }

    async fn save_runner(&self, runner: &RunnerRecord) -> Result<(), Error> {
        let mut runners = self.runners.lock().map_err(|_| poisoned())?;
        let status = runners
            .get(&runner.id)
            .map(|r| r.status.clone())
            .unwrap_or_else(|| runner.status.clone());
        runners.insert(
            runner.id,
            RunnerRecord {
                status,
                ..runner.clone()
            },
        );
        Ok(())
    }

    async fn delete_runner(&self, id: RunnerId) -> Result<(), Error> {
        self.runners.lock().map_err(|_| poisoned())?.remove(&id);
        Ok(())
    }

    async fn update_runner_status(
        &self,
        id: RunnerId,
        status: &RunnerStatus,
    ) -> Result<(), Error> {
        if let Some(r) = self.runners.lock().map_err(|_| poisoned())?.get_mut(&id) {
            r.status = status.clone();
        }
        Ok(())
    }

    async fn save_task(&self, task: &TaskRecord) -> Result<(), Error> {
        self.tasks
            .lock()
            .map_err(|_| poisoned())?
            .insert(task.id, task.clone());
        Ok(())
    }

    async fn save_attempt(&self, attempt: &AttemptRecord) -> Result<(), Error> {
        let mut attempts = self.attempts.lock().map_err(|_| poisoned())?;
        match attempts.iter_mut().find(|a| a.id == attempt.id) {
            Some(existing) => *existing = attempt.clone(),
            None => attempts.push(attempt.clone()),
        }
        Ok(())
    }

    async fn task_history(
        &self,
        task_id: TaskId,
    ) -> Result<Option<(TaskRecord, Vec<AttemptRecord>)>, Error> {
        let Some(task) = self
            .tasks
            .lock()
            .map_err(|_| poisoned())?
            .get(&task_id)
            .cloned()
        else {
            return Ok(None);
        };
        let mut attempts: Vec<AttemptRecord> = self
            .attempts
            .lock()
            .map_err(|_| poisoned())?
            .iter()
            .filter(|a| a.task_id == task_id)
            .cloned()
            .collect();
        attempts.sort_by_key(|a| a.number);
        Ok(Some((task, attempts)))
    }
}
