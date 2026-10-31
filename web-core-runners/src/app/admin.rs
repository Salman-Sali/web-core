use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::store::{
    AttemptRecord, AttemptStatus, RunnerSettings, RunnerState, TaskRecord, TaskStatus,
};
use crate::protocol::{AttemptId, RunnerId, TaskId};

/// A runner as shown on the admin Runners page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerView {
    /// Id.
    pub id: RunnerId,
    /// Name.
    pub name: String,
    /// The app's local fallback runner (not editable).
    pub local: bool,
    /// Capacity.
    pub capacity: u32,
    /// Priority tier (lower first).
    pub priority: i32,
    /// Weight within the tier.
    pub weight: u32,
    /// Enabled flag.
    pub enabled: bool,
    /// Drained by admin (no work until resumed).
    pub drained: bool,
    /// Connection state.
    pub state: RunnerState,
    /// Attempts running on it now.
    pub running: u32,
    /// Last message received.
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Protocol version from the last hello.
    pub protocol_version: Option<u32>,
    /// Contract hash from the last hello.
    pub contract_hash: Option<String>,
    /// Whether that contract hash is the app's.
    pub contract_matches: bool,
    /// Tool versions from the last hello.
    pub tool_versions: BTreeMap<String, String>,
    /// Registration time (`None` for the local runner).
    pub created_at: Option<DateTime<Utc>>,
}

/// Body of the admin "register runner" request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterRunnerRequest {
    /// Display name.
    pub name: String,
    /// Initial settings (default: capacity 1, priority 0, weight 1).
    #[serde(default)]
    pub settings: Option<RunnerSettings>,
}

/// Answer to "register runner": the token is shown once and never stored in clear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterRunnerResponse {
    /// The new runner.
    pub runner: RunnerView,
    /// `<runner id>.<secret>`, for the runner's `RUNNER_TOKEN`.
    pub token: String,
}

/// Partial update of a runner's settings from admin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerSettingsPatch {
    /// New name.
    #[serde(default)]
    pub name: Option<String>,
    /// New capacity.
    #[serde(default)]
    pub capacity: Option<u32>,
    /// New priority.
    #[serde(default)]
    pub priority: Option<i32>,
    /// New weight (at least 1).
    #[serde(default)]
    pub weight: Option<u32>,
}

/// One attempt in a task's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptView {
    /// Id.
    pub id: AttemptId,
    /// 1-based number.
    pub number: u32,
    /// Runner id.
    pub runner_id: RunnerId,
    /// Runner name.
    pub runner_name: String,
    /// Status.
    pub status: AttemptStatus,
    /// Last percent.
    pub progress: f32,
    /// Last checkpoint.
    pub checkpoint: Option<Value>,
    /// Start.
    pub started_at: DateTime<Utc>,
    /// End.
    pub ended_at: Option<DateTime<Utc>>,
    /// What went wrong.
    pub error: Option<String>,
}

impl From<AttemptRecord> for AttemptView {
    fn from(a: AttemptRecord) -> Self {
        Self {
            id: a.id,
            number: a.number,
            runner_id: a.runner_id,
            runner_name: a.runner_name,
            status: a.status,
            progress: a.progress,
            checkpoint: a.checkpoint,
            started_at: a.started_at,
            ended_at: a.ended_at,
            error: a.error,
        }
    }
}

/// A task with its attempts, for a job's detail page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskHistoryView {
    /// Id.
    pub id: TaskId,
    /// Task type.
    pub task_type: String,
    /// Status.
    pub status: TaskStatus,
    /// Attempts allowed.
    pub max_attempts: u32,
    /// Submitter's reference.
    pub reference: Option<String>,
    /// Submission time.
    pub created_at: DateTime<Utc>,
    /// End time.
    pub finished_at: Option<DateTime<Utc>>,
    /// Failure reason.
    pub failure: Option<String>,
    /// Attempts, oldest first.
    pub attempts: Vec<AttemptView>,
}

impl TaskHistoryView {
    /// Builds the view from stored rows.
    pub fn new(task: TaskRecord, attempts: Vec<AttemptRecord>) -> Self {
        Self {
            id: task.id,
            task_type: task.task_type,
            status: task.status,
            max_attempts: task.max_attempts,
            reference: task.reference,
            created_at: task.created_at,
            finished_at: task.finished_at,
            failure: task.failure,
            attempts: attempts.into_iter().map(AttemptView::from).collect(),
        }
    }
}
