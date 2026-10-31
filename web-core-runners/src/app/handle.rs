use std::{
    future::Future,
    marker::PhantomData,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use super::{RunnerPool, store::TaskStatus};
use crate::protocol::{Task, TaskId};

/// Called before attempt 2, 3, ... when an earlier attempt sent a checkpoint: gets the input
/// (to modify) and the last checkpoint. convonote uses it to put the recorded probe into
/// `known_probe` so a retry does not probe again.
pub type RetryHook = Arc<dyn Fn(&mut Value, &Value) + Send + Sync>;

/// Options of one submission.
#[derive(Clone, Default)]
pub struct SubmitOptions {
    /// Attempts allowed (default: the pool's per-type setting, else 3).
    pub max_attempts: Option<u32>,
    /// Per-attempt timeout on the runner (default: the pool's).
    pub timeout: Option<Duration>,
    /// Submitter's reference (a job id) stored on the task row.
    pub reference: Option<String>,
    /// Input augmentation from the last checkpoint before a retry.
    pub retry_hook: Option<RetryHook>,
}

impl SubmitOptions {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the attempts allowed.
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = Some(max_attempts);
        self
    }

    /// Sets the per-attempt timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets the reference stored on the task row.
    pub fn with_reference(mut self, reference: impl Into<String>) -> Self {
        self.reference = Some(reference.into());
        self
    }

    /// Sets the retry hook.
    pub fn with_retry_hook<F>(mut self, hook: F) -> Self
    where
        F: Fn(&mut Value, &Value) + Send + Sync + 'static,
    {
        self.retry_hook = Some(Arc::new(hook));
        self
    }
}

/// An input file on the app's disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSource {
    /// Slot name.
    pub slot: String,
    /// Path the app serves it from.
    pub path: PathBuf,
    /// SHA-256 if already known (otherwise computed at submit).
    pub sha256: Option<String>,
}

/// Where an output slot lands on the app's disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTarget {
    /// Slot name.
    pub slot: String,
    /// Final path; the upload goes to `<path>.<attempt>.part` and is renamed after
    /// verification. The parent directory is created if missing.
    pub path: PathBuf,
    /// The task may succeed without writing it.
    pub optional: bool,
}

/// The files manifest of a submission: inputs to serve, outputs to receive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFiles {
    /// Inputs.
    pub inputs: Vec<InputSource>,
    /// Outputs.
    pub outputs: Vec<OutputTarget>,
}

impl TaskFiles {
    /// No files.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an input slot served from `path`.
    pub fn input(mut self, slot: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.inputs.push(InputSource {
            slot: slot.into(),
            path: path.into(),
            sha256: None,
        });
        self
    }

    /// Adds an input slot whose SHA-256 is already known (skips hashing at submit).
    pub fn input_with_hash(
        mut self,
        slot: impl Into<String>,
        path: impl Into<PathBuf>,
        sha256: impl Into<String>,
    ) -> Self {
        self.inputs.push(InputSource {
            slot: slot.into(),
            path: path.into(),
            sha256: Some(sha256.into()),
        });
        self
    }

    /// Adds a required output slot landing at `path`.
    pub fn output(mut self, slot: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.outputs.push(OutputTarget {
            slot: slot.into(),
            path: path.into(),
            optional: false,
        });
        self
    }

    /// Adds an optional output slot landing at `path`.
    pub fn optional_output(mut self, slot: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.outputs.push(OutputTarget {
            slot: slot.into(),
            path: path.into(),
            optional: true,
        });
        self
    }
}

/// An output file that arrived and was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Slot name.
    pub slot: String,
    /// Final path on the app's disk.
    pub path: PathBuf,
    /// SHA-256, lowercase hex.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// A finished task.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskOutput {
    /// The task's output JSON.
    pub output: Value,
    /// Output files now at their target paths (optional slots the task skipped are absent).
    pub files: Vec<OutputFile>,
    /// Attempts it took.
    pub attempts: u32,
}

impl TaskOutput {
    /// Deserialises the output JSON.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, TaskError> {
        serde_json::from_value(self.output.clone())
            .map_err(|e| TaskError::InvalidOutput(e.to_string()))
    }
}

/// Why a task produced no output.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TaskError {
    /// The task reported a failure (final).
    #[error("task failed: {0}")]
    Failed(String),
    /// Every allowed attempt crashed, timed out, lost its lease or failed a transfer.
    #[error("task gave up after {attempts} attempts: {last_error}")]
    AttemptsExhausted {
        /// Attempts made.
        attempts: u32,
        /// What happened to the last one.
        last_error: String,
    },
    /// Cancelled through the handle.
    #[error("task cancelled")]
    Cancelled,
    /// The pool was dropped.
    #[error("runner pool closed")]
    PoolClosed,
    /// The output JSON did not match the expected type.
    #[error("invalid task output: {0}")]
    InvalidOutput(String),
}

/// Live progress of a task, published on every change.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskProgress {
    /// Task status.
    pub status: TaskStatus,
    /// Current (or last) attempt number; 0 while first queued.
    pub attempt: u32,
    /// Percent of the current attempt.
    pub percent: f32,
    /// Last checkpoint of any attempt.
    pub checkpoint: Option<Value>,
    /// Name of the runner executing the current attempt.
    pub runner: Option<String>,
}

impl Default for TaskProgress {
    fn default() -> Self {
        Self {
            status: TaskStatus::Queued,
            attempt: 0,
            percent: 0.0,
            checkpoint: None,
            runner: None,
        }
    }
}

/// Cancels a task from anywhere (cloneable, outlives the awaited handle).
#[derive(Clone)]
pub struct TaskCanceller {
    pub(crate) pool: RunnerPool,
    pub(crate) id: TaskId,
}

impl TaskCanceller {
    /// Cancels the task: dequeues it or sends `Cancel` to its runner. The handle resolves to
    /// [`TaskError::Cancelled`].
    pub fn cancel(&self) {
        self.pool.cancel_task(self.id);
    }
}

/// A submitted task. Await it for the output; `progress()` for live updates. Dropping an
/// unfinished handle cancels the task (an aborted apalis job takes its task with it).
pub struct TaskHandle {
    pub(crate) id: TaskId,
    pub(crate) rx: oneshot::Receiver<Result<TaskOutput, TaskError>>,
    pub(crate) progress: watch::Receiver<TaskProgress>,
    pub(crate) pool: RunnerPool,
    pub(crate) done: bool,
}

impl TaskHandle {
    /// The task id.
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// A receiver of progress updates (percent, checkpoint, attempt, status).
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.progress.clone()
    }

    /// The last checkpoint any attempt of this task sent.
    pub fn last_checkpoint(&self) -> Option<Value> {
        self.progress.borrow().checkpoint.clone()
    }

    /// A cloneable canceller, usable while the handle is being awaited.
    pub fn canceller(&self) -> TaskCanceller {
        TaskCanceller {
            pool: self.pool.clone(),
            id: self.id,
        }
    }

    /// Cancels the task.
    pub fn cancel(&self) {
        self.pool.cancel_task(self.id);
    }
}

impl Future for TaskHandle {
    type Output = Result<TaskOutput, TaskError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(result) => {
                self.done = true;
                Poll::Ready(result.unwrap_or(Err(TaskError::PoolClosed)))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        if !self.done {
            self.pool.cancel_task(self.id);
        }
    }
}

/// A finished typed task.
#[derive(Debug)]
pub struct TypedOutput<T: Task> {
    /// The task's output.
    pub output: T::Output,
    /// Output files now at their target paths.
    pub files: Vec<OutputFile>,
    /// Attempts it took.
    pub attempts: u32,
}

/// A [`TaskHandle`] whose output is deserialised into `T::Output`.
pub struct TypedTaskHandle<T: Task> {
    pub(crate) inner: TaskHandle,
    pub(crate) _task: PhantomData<fn() -> T>,
}

impl<T: Task> TypedTaskHandle<T> {
    /// The untyped handle (progress, cancel, id).
    pub fn handle(&self) -> &TaskHandle {
        &self.inner
    }

    /// The task id.
    pub fn id(&self) -> TaskId {
        self.inner.id
    }

    /// Progress updates.
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.inner.progress()
    }

    /// A cloneable canceller.
    pub fn canceller(&self) -> TaskCanceller {
        self.inner.canceller()
    }

    /// Cancels the task.
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

impl<T: Task> Future for TypedTaskHandle<T> {
    type Output = Result<TypedOutput<T>, TaskError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.inner).poll(cx) {
            Poll::Ready(Ok(out)) => Poll::Ready(out.parse::<T::Output>().map(|output| {
                TypedOutput {
                    output,
                    files: out.files,
                    attempts: out.attempts,
                }
            })),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}
