use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use super::services::ServiceSpec;
use crate::protocol::Task;

/// A task implementation: gets the context and the input JSON, returns the output JSON or a
/// failure reason (final, not retried). A panic counts as a crash and is retried.
pub type TaskFn = Arc<dyn Fn(&TaskContext, Value) -> Result<Value, String> + Send + Sync>;

type ToolVersionsFn = Arc<dyn Fn() -> BTreeMap<String, String> + Send + Sync>;

/// Everything a product's runner binary provides: its contract hash, the implementation of
/// each task type, the managed services those tasks need and how to read tool versions.
///
/// ```no_run
/// # use web_core_runners::runner::{self, TaskRegistry};
/// fn main() -> std::process::ExitCode {
///     let registry = TaskRegistry::new("<contract hash>")
///         .raw("echo", |_ctx, input| Ok(input))
///         .tool_versions(|| [("ffmpeg".to_string(), "7.1".to_string())].into());
///     runner::main(registry)
/// }
/// ```
#[derive(Clone)]
pub struct TaskRegistry {
    contract_hash: String,
    handlers: HashMap<String, TaskFn>,
    services: Vec<ServiceSpec>,
    tool_versions: Option<ToolVersionsFn>,
}

impl TaskRegistry {
    /// An empty registry for the contract with this hash.
    pub fn new(contract_hash: impl Into<String>) -> Self {
        Self {
            contract_hash: contract_hash.into(),
            handlers: HashMap::new(),
            services: Vec::new(),
            tool_versions: None,
        }
    }

    /// Registers the implementation of task type `T`.
    pub fn task<T, F>(self, f: F) -> Self
    where
        T: Task,
        F: Fn(&TaskContext, T::Input) -> Result<T::Output, String> + Send + Sync + 'static,
    {
        self.raw(T::TYPE, move |ctx, input| {
            let input: T::Input = serde_json::from_value(input)
                .map_err(|e| format!("invalid input for {}: {e}", T::TYPE))?;
            let output = f(ctx, input)?;
            serde_json::to_value(output).map_err(|e| format!("invalid output: {e}"))
        })
    }

    /// Registers an untyped implementation under `task_type`.
    pub fn raw<F>(mut self, task_type: &str, f: F) -> Self
    where
        F: Fn(&TaskContext, Value) -> Result<Value, String> + Send + Sync + 'static,
    {
        self.handlers.insert(task_type.to_string(), Arc::new(f));
        self
    }

    /// Declares a managed service, started when the runner accepts work and stopped when it
    /// drains or exits.
    pub fn service(mut self, spec: ServiceSpec) -> Self {
        self.services.push(spec);
        self
    }

    /// Sets the function that reports tool versions at hello (runs once per boot).
    pub fn tool_versions<F>(mut self, f: F) -> Self
    where
        F: Fn() -> BTreeMap<String, String> + Send + Sync + 'static,
    {
        self.tool_versions = Some(Arc::new(f));
        self
    }

    /// Replaces the contract hash (tests use it to simulate an outdated runner).
    pub fn with_contract_hash(mut self, contract_hash: impl Into<String>) -> Self {
        self.contract_hash = contract_hash.into();
        self
    }

    /// The contract hash sent at hello.
    pub fn contract_hash(&self) -> &str {
        &self.contract_hash
    }

    /// Declared managed services.
    pub fn services(&self) -> &[ServiceSpec] {
        &self.services
    }

    /// The implementation of `task_type`, if registered.
    pub fn handler(&self, task_type: &str) -> Option<TaskFn> {
        self.handlers.get(task_type).cloned()
    }

    /// Registered task types, sorted.
    pub fn task_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self.handlers.keys().cloned().collect();
        types.sort();
        types
    }

    /// Calls the tool versions function (empty without one).
    pub fn collect_tool_versions(&self) -> BTreeMap<String, String> {
        self.tool_versions.as_ref().map(|f| f()).unwrap_or_default()
    }
}

/// One progress report of a running task.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressUpdate {
    /// 0..=100.
    pub percent: f32,
    /// Facts to persist in the app (sent once; the app keeps the last one).
    pub checkpoint: Option<Value>,
}

#[derive(Clone)]
enum ProgressSink {
    Stdout,
    Channel(mpsc::UnboundedSender<ProgressUpdate>),
}

/// What a task implementation sees: its work dir, its file slots, and a way to report progress.
#[derive(Clone)]
pub struct TaskContext {
    work_dir: PathBuf,
    sink: ProgressSink,
    cancelled: Arc<AtomicBool>,
}

impl TaskContext {
    /// Context of a task in a child process: progress goes to stdout as JSON lines.
    pub fn for_child(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
            sink: ProgressSink::Stdout,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Context of a task run in-process: progress goes to `progress`, cancellation is the flag.
    pub fn in_process(
        work_dir: impl Into<PathBuf>,
        progress: mpsc::UnboundedSender<ProgressUpdate>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            work_dir: work_dir.into(),
            sink: ProgressSink::Channel(progress),
            cancelled,
        }
    }

    /// The attempt's private directory; wiped when the attempt ends.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// Path of a downloaded input slot (`<work dir>/in/<slot>`).
    pub fn input(&self, slot: &str) -> PathBuf {
        self.work_dir.join("in").join(slot)
    }

    /// Path the task writes an output slot to (`<work dir>/out/<slot>`).
    pub fn output(&self, slot: &str) -> PathBuf {
        self.work_dir.join("out").join(slot)
    }

    /// A scratch directory inside the work dir (`<work dir>/tmp`).
    pub fn scratch(&self) -> PathBuf {
        self.work_dir.join("tmp")
    }

    /// Reports progress (0..=100).
    pub fn progress(&self, percent: f32) {
        self.emit(ProgressUpdate {
            percent,
            checkpoint: None,
        });
    }

    /// Reports progress with a checkpoint: facts the app persists so a later attempt can
    /// receive them in its input.
    pub fn checkpoint<C: Serialize>(&self, percent: f32, checkpoint: &C) {
        match serde_json::to_value(checkpoint) {
            Ok(value) => self.emit(ProgressUpdate {
                percent,
                checkpoint: Some(value),
            }),
            Err(e) => tracing::warn!("checkpoint not serialisable: {e}"),
        }
    }

    /// Whether the attempt was cancelled. Only in-process tasks need to poll it; a child
    /// process is killed instead.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn emit(&self, update: ProgressUpdate) {
        match &self.sink {
            ProgressSink::Channel(tx) => {
                let _ = tx.send(update);
            }
            ProgressSink::Stdout => write_line(&ChildLine::Progress {
                percent: update.percent,
                checkpoint: update.checkpoint,
            }),
        }
    }
}

/// One JSON line a task child writes to stdout. The last line is `output` or `failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChildLine {
    /// Progress, optionally with a checkpoint.
    Progress {
        /// 0..=100.
        percent: f32,
        /// Facts to persist in the app.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<Value>,
    },
    /// The task's output JSON.
    Output {
        /// Output JSON.
        output: Value,
    },
    /// The task failed (final).
    Failed {
        /// Reason.
        reason: String,
    },
}

fn write_line(line: &ChildLine) {
    if let Ok(text) = serde_json::to_string(line) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{text}");
        let _ = stdout.flush();
    }
}

/// Entry point of the `task` subcommand: `<binary> task <type> --work-dir <dir>`, the input
/// JSON on stdin. Writes progress lines and the final `output`/`failed` line to stdout.
/// Called by [`super::main`]; exposed for products that parse their own arguments.
pub fn run_task_child(registry: &TaskRegistry, args: &[String]) -> ExitCode {
    let Some(task_type) = args.first() else {
        eprintln!("usage: task <type> --work-dir <dir>");
        return ExitCode::from(2);
    };
    let work_dir = args
        .windows(2)
        .find(|w| w[0] == "--work-dir")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut stdin = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut stdin) {
        eprintln!("cannot read input: {e}");
        return ExitCode::from(2);
    }
    let input: Value = match serde_json::from_str(&stdin) {
        Ok(v) => v,
        Err(e) => {
            write_line(&ChildLine::Failed {
                reason: format!("input is not JSON: {e}"),
            });
            return ExitCode::SUCCESS;
        }
    };
    let Some(handler) = registry.handler(task_type) else {
        write_line(&ChildLine::Failed {
            reason: format!("unknown task type {task_type}"),
        });
        return ExitCode::SUCCESS;
    };
    let ctx = TaskContext::for_child(work_dir);
    match handler(&ctx, input) {
        Ok(output) => write_line(&ChildLine::Output { output }),
        Err(reason) => write_line(&ChildLine::Failed { reason }),
    }
    ExitCode::SUCCESS
}
