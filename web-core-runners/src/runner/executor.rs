use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use super::registry::{ChildLine, ProgressUpdate, TaskContext, TaskRegistry};

const STDERR_TAIL: usize = 8 * 1024;

/// One task execution request: the attempt's work dir already holds the inputs under `in/`.
#[derive(Debug, Clone)]
pub struct ExecJob {
    /// Task type.
    pub task_type: String,
    /// Input JSON.
    pub input: Value,
    /// The attempt's work dir.
    pub work_dir: PathBuf,
}

/// How an execution ended.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecResult {
    /// Output JSON; outputs are under `out/`.
    Output(Value),
    /// The task reported a failure.
    Failed(String),
    /// The task died.
    Crashed {
        /// Exit code, `None` when killed by a signal.
        exit_code: Option<i32>,
        /// Tail of stderr (or the panic message in-process).
        stderr_tail: String,
    },
    /// Cancelled through the token.
    Cancelled,
}

/// Runs one task. The real runner uses [`ChildProcessExecutor`]; the in-process test runner
/// uses [`InlineExecutor`].
#[async_trait]
pub trait TaskExecutor: Send + Sync + 'static {
    /// Executes `job`, forwarding progress, until it ends or `cancel` fires.
    async fn execute(
        &self,
        job: ExecJob,
        progress: mpsc::UnboundedSender<ProgressUpdate>,
        cancel: CancellationToken,
    ) -> ExecResult;
}

/// Re-executes a program (by default the runner's own binary) as
/// `<program> [prefix args] task <type> --work-dir <dir>`, with the input JSON on stdin and
/// JSON lines on stdout. A crash, panic or kill of the child ends only this attempt.
#[derive(Debug, Clone)]
pub struct ChildProcessExecutor {
    program: PathBuf,
    prefix_args: Vec<String>,
}

impl ChildProcessExecutor {
    /// Executes tasks with the current executable.
    pub fn current_exe() -> Result<Self, String> {
        let program = std::env::current_exe().map_err(|e| e.to_string())?;
        Ok(Self::with_program(program, Vec::new()))
    }

    /// Executes tasks with `program prefix_args... task <type> --work-dir <dir>`.
    pub fn with_program(program: impl Into<PathBuf>, prefix_args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            prefix_args,
        }
    }
}

#[async_trait]
impl TaskExecutor for ChildProcessExecutor {
    async fn execute(
        &self,
        job: ExecJob,
        progress: mpsc::UnboundedSender<ProgressUpdate>,
        cancel: CancellationToken,
    ) -> ExecResult {
        let mut child = match Command::new(&self.program)
            .args(&self.prefix_args)
            .arg("task")
            .arg(&job.task_type)
            .arg("--work-dir")
            .arg(&job.work_dir)
            .current_dir(&job.work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                return ExecResult::Crashed {
                    exit_code: None,
                    stderr_tail: format!("cannot start task process: {e}"),
                };
            }
        };

        if let Some(mut stdin) = child.stdin.take() {
            let bytes = job.input.to_string().into_bytes();
            tokio::spawn(async move {
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.shutdown().await;
            });
        }

        let stdout = child.stdout.take().map(|out| {
            tokio::spawn(async move {
                let mut last: Option<ChildLine> = None;
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    match serde_json::from_str::<ChildLine>(&line) {
                        Ok(ChildLine::Progress {
                            percent,
                            checkpoint,
                        }) => {
                            let _ = progress.send(ProgressUpdate {
                                percent,
                                checkpoint,
                            });
                        }
                        Ok(line) => last = Some(line),
                        Err(_) => tracing::debug!(target: "runner::task", "{line}"),
                    }
                }
                last
            })
        });

        let stderr = child.stderr.take().map(|mut err| {
            tokio::spawn(async move {
                let mut tail: Vec<u8> = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > STDERR_TAIL {
                        tail.drain(..tail.len() - STDERR_TAIL);
                    }
                }
                String::from_utf8_lossy(&tail).into_owned()
            })
        });

        let status = tokio::select! {
            status = child.wait() => status,
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return ExecResult::Cancelled;
            }
        };

        let last = match stdout {
            Some(handle) => handle.await.ok().flatten(),
            None => None,
        };
        let stderr_tail = match stderr {
            Some(handle) => handle.await.unwrap_or_default(),
            None => String::new(),
        };
        match status {
            Ok(status) if status.success() => match last {
                Some(ChildLine::Output { output }) => ExecResult::Output(output),
                Some(ChildLine::Failed { reason }) => ExecResult::Failed(reason),
                _ => ExecResult::Crashed {
                    exit_code: Some(0),
                    stderr_tail: format!("task exited without an output line. {stderr_tail}"),
                },
            },
            Ok(status) => ExecResult::Crashed {
                exit_code: status.code(),
                stderr_tail,
            },
            Err(e) => ExecResult::Crashed {
                exit_code: None,
                stderr_tail: format!("wait failed: {e}"),
            },
        }
    }
}

/// Runs registry closures on a blocking thread of the current process. A panic is reported as
/// a crash (exit code 101, like a panicking child). Cancellation sets
/// [`TaskContext::is_cancelled`]; the closure should poll it when it runs long.
#[derive(Clone)]
pub struct InlineExecutor {
    registry: Arc<TaskRegistry>,
}

impl InlineExecutor {
    /// An executor over `registry`'s handlers.
    pub fn new(registry: Arc<TaskRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl TaskExecutor for InlineExecutor {
    async fn execute(
        &self,
        job: ExecJob,
        progress: mpsc::UnboundedSender<ProgressUpdate>,
        cancel: CancellationToken,
    ) -> ExecResult {
        let Some(handler) = self.registry.handler(&job.task_type) else {
            return ExecResult::Failed(format!("unknown task type {}", job.task_type));
        };
        let flag = Arc::new(AtomicBool::new(false));
        {
            let flag = flag.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                cancel.cancelled().await;
                flag.store(true, Ordering::Relaxed);
            });
        }
        let ctx = TaskContext::in_process(job.work_dir, progress, flag);
        let input = job.input;
        let run = tokio::task::spawn_blocking(move || {
            catch_unwind(AssertUnwindSafe(|| handler(&ctx, input)))
        });
        tokio::select! {
            joined = run => match joined {
                Ok(Ok(Ok(output))) => ExecResult::Output(output),
                Ok(Ok(Err(reason))) => ExecResult::Failed(reason),
                Ok(Err(panic)) => ExecResult::Crashed {
                    exit_code: Some(101),
                    stderr_tail: panic_message(panic.as_ref()),
                },
                Err(e) => ExecResult::Crashed { exit_code: None, stderr_tail: e.to_string() },
            },
            _ = cancel.cancelled() => ExecResult::Cancelled,
        }
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        format!("panicked: {s}")
    } else if let Some(s) = panic.downcast_ref::<String>() {
        format!("panicked: {s}")
    } else {
        "panicked".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sh(script: &str) -> ChildProcessExecutor {
        // `sh -c <script> sh task <type> --work-dir <dir>`
        ChildProcessExecutor::with_program("sh", vec!["-c".into(), script.into(), "sh".into()])
    }

    async fn run(exec: &ChildProcessExecutor) -> (ExecResult, Vec<ProgressUpdate>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = exec
            .execute(
                ExecJob {
                    task_type: "t".into(),
                    input: json!({"n": 1}),
                    work_dir: dir.path().to_path_buf(),
                },
                tx,
                CancellationToken::new(),
            )
            .await;
        let mut updates = Vec::new();
        while let Ok(u) = rx.try_recv() {
            updates.push(u);
        }
        (result, updates)
    }

    #[tokio::test]
    async fn child_output_and_progress() {
        let exec = sh(r#"read input; test "$1" = task || exit 9;
            echo '{"kind":"progress","percent":10}';
            echo 'noise from a tool';
            echo '{"kind":"progress","percent":50,"checkpoint":{"probe":1}}';
            echo "{\"kind\":\"output\",\"output\":$input}""#);
        let (result, updates) = run(&exec).await;
        assert_eq!(result, ExecResult::Output(json!({"n": 1})));
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[1].checkpoint, Some(json!({"probe": 1})));
    }

    #[tokio::test]
    async fn child_failure_is_reported() {
        let exec = sh(r#"echo '{"kind":"failed","reason":"unsupported"}'"#);
        assert_eq!(run(&exec).await.0, ExecResult::Failed("unsupported".into()));
    }

    #[tokio::test]
    async fn child_crash_is_detected_by_exit_status() {
        let exec = sh(r#"echo '{"kind":"progress","percent":10}'; echo boom >&2; exit 3"#);
        match run(&exec).await.0 {
            ExecResult::Crashed {
                exit_code,
                stderr_tail,
            } => {
                assert_eq!(exit_code, Some(3));
                assert!(stderr_tail.contains("boom"));
            }
            other => panic!("unexpected {other:?}"),
        }
        // a zero exit without an output line is a crash too
        let exec = sh("true");
        assert!(matches!(run(&exec).await.0, ExecResult::Crashed { .. }));
    }

    #[tokio::test]
    async fn child_is_killed_on_cancel() {
        let exec = sh("sleep 30");
        let dir = tempfile::tempdir().expect("tempdir");
        let (tx, _rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            c.cancel();
        });
        let started = std::time::Instant::now();
        let result = exec
            .execute(
                ExecJob {
                    task_type: "t".into(),
                    input: json!(null),
                    work_dir: dir.path().to_path_buf(),
                },
                tx,
                cancel,
            )
            .await;
        assert_eq!(result, ExecResult::Cancelled);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn inline_panic_is_a_crash() {
        let registry = TaskRegistry::new("h").raw("boom", |_ctx, _input| panic!("kaboom"));
        let exec = InlineExecutor::new(Arc::new(registry));
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = exec
            .execute(
                ExecJob {
                    task_type: "boom".into(),
                    input: json!(null),
                    work_dir: PathBuf::from("."),
                },
                tx,
                CancellationToken::new(),
            )
            .await;
        match result {
            ExecResult::Crashed {
                exit_code,
                stderr_tail,
            } => {
                assert_eq!(exit_code, Some(101));
                assert!(stderr_tail.contains("kaboom"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
