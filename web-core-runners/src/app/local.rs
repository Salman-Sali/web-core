use std::{fmt, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::{
    process::{Child, Command},
    sync::watch,
    task::JoinHandle,
    time::Instant,
};

use super::pool::{PoolSummary, RunnerPool};

/// Admin toggle of the local fallback runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LocalRunnerMode {
    /// Never run it.
    Off,
    /// Run it only while no remote runner is online.
    #[default]
    Auto,
    /// Always run it (at the lowest priority, capacity 1).
    Always,
}

impl LocalRunnerMode {
    /// The stored string (`off`, `auto`, `always`).
    pub fn as_str(&self) -> &'static str {
        match self {
            LocalRunnerMode::Off => "off",
            LocalRunnerMode::Auto => "auto",
            LocalRunnerMode::Always => "always",
        }
    }
}

impl fmt::Display for LocalRunnerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LocalRunnerMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "off" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            other => Err(format!("unknown local runner mode {other}")),
        }
    }
}

/// How the app starts its local runner: `<binary> --local --url <url> --token <token>
/// --work-dir <dir> --log-level <level>`.
#[derive(Debug, Clone)]
pub struct LocalFallbackConfig {
    /// The runner binary (the aarch64 build on the AWS host).
    pub binary: PathBuf,
    /// The app's listening port; the runner connects to `http://127.0.0.1:<port>`.
    pub port: u16,
    /// The local runner's work dir.
    pub work_dir: PathBuf,
    /// Log level passed to it.
    pub log_level: String,
    /// In `Auto`, how long no remote runner must be online before the local one starts
    /// (covers app restarts, where remotes reconnect within seconds). Default 15 s.
    pub auto_grace: Duration,
    /// How long the local runner may finish its attempt after `Exit`. Default 5 min.
    pub exit_grace: Duration,
    /// Delay before restarting a local runner that died while needed. Default 2 s.
    pub restart_delay: Duration,
}

impl LocalFallbackConfig {
    /// A config with defaults.
    pub fn new(binary: impl Into<PathBuf>, port: u16, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            port,
            work_dir: work_dir.into(),
            log_level: "info".into(),
            auto_grace: Duration::from_secs(15),
            exit_grace: Duration::from_secs(5 * 60),
            restart_delay: Duration::from_secs(2),
        }
    }

    /// The command line for `token`.
    pub fn command(&self, token: &str) -> LocalCommand {
        LocalCommand {
            program: self.binary.clone(),
            args: vec![
                "--local".into(),
                "--url".into(),
                format!("http://127.0.0.1:{}", self.port),
                "--token".into(),
                token.into(),
                "--work-dir".into(),
                self.work_dir.to_string_lossy().into_owned(),
                "--log-level".into(),
                self.log_level.clone(),
            ],
        }
    }
}

/// A program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCommand {
    /// Program path.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<String>,
}

impl LocalCommand {
    /// The value following `flag` in the arguments.
    pub fn arg_value(&self, flag: &str) -> Option<&str> {
        self.args
            .windows(2)
            .find(|w| w[0] == flag)
            .map(|w| w[1].as_str())
    }
}

/// Starts the local runner process. [`TokioSpawner`] is the real one; tests use an in-process
/// runner instead.
pub trait ProcessSpawner: Send + Sync + 'static {
    /// Starts `command`.
    fn spawn(&self, command: &LocalCommand) -> Result<Box<dyn LocalProcess>, String>;
}

/// A started local runner.
#[async_trait]
pub trait LocalProcess: Send {
    /// Waits for it to exit; returns a description of how.
    async fn wait(&mut self) -> String;
    /// Kills it.
    async fn kill(&mut self);
}

/// Spawns a real child process (killed when the supervisor is dropped).
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSpawner;

impl ProcessSpawner for TokioSpawner {
    fn spawn(&self, command: &LocalCommand) -> Result<Box<dyn LocalProcess>, String> {
        let child = Command::new(&command.program)
            .args(&command.args)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", command.program.display()))?;
        Ok(Box::new(ChildProcess(child)))
    }
}

struct ChildProcess(Child);

#[async_trait]
impl LocalProcess for ChildProcess {
    async fn wait(&mut self) -> String {
        match self.0.wait().await {
            Ok(status) => status.to_string(),
            Err(e) => e.to_string(),
        }
    }

    async fn kill(&mut self) {
        let _ = self.0.kill().await;
    }
}

/// Process state of the local runner, for admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRunnerStatus {
    /// Not running.
    Stopped,
    /// Running (connected or connecting).
    Running,
    /// Told to exit; finishing its attempt.
    Stopping,
}

/// What admin shows about the local fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFallbackState {
    /// The toggle.
    pub mode: LocalRunnerMode,
    /// Process state.
    pub status: LocalRunnerStatus,
    /// Restarts after the process died while needed.
    pub restarts: u32,
    /// How the last process ended.
    pub last_exit: Option<String>,
}

/// Runs the app's local fallback runner per [`LocalRunnerMode`]: starts it when needed,
/// sends `Exit` when it is not, restarts it if it dies while needed.
pub struct LocalFallbackSupervisor {
    mode: watch::Sender<LocalRunnerMode>,
    state: watch::Receiver<LocalFallbackState>,
    task: JoinHandle<()>,
}

impl LocalFallbackSupervisor {
    /// Starts supervising with real child processes.
    pub fn start(pool: RunnerPool, config: LocalFallbackConfig, mode: LocalRunnerMode) -> Self {
        Self::start_with(pool, config, mode, Arc::new(TokioSpawner))
    }

    /// Starts supervising with a custom spawner.
    pub fn start_with(
        pool: RunnerPool,
        config: LocalFallbackConfig,
        mode: LocalRunnerMode,
        spawner: Arc<dyn ProcessSpawner>,
    ) -> Self {
        let (mode_tx, mode_rx) = watch::channel(mode);
        let (state_tx, state_rx) = watch::channel(LocalFallbackState {
            mode,
            status: LocalRunnerStatus::Stopped,
            restarts: 0,
            last_exit: None,
        });
        let task = tokio::spawn(supervise(pool, config, mode_rx, state_tx, spawner));
        Self {
            mode: mode_tx,
            state: state_rx,
            task,
        }
    }

    /// Changes the mode (admin toggle).
    pub fn set_mode(&self, mode: LocalRunnerMode) {
        self.mode.send_replace(mode);
    }

    /// The current mode.
    pub fn mode(&self) -> LocalRunnerMode {
        *self.mode.borrow()
    }

    /// The current state.
    pub fn state(&self) -> LocalFallbackState {
        self.state.borrow().clone()
    }

    /// A receiver of state changes.
    pub fn watch(&self) -> watch::Receiver<LocalFallbackState> {
        self.state.clone()
    }
}

impl Drop for LocalFallbackSupervisor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn wait_child(child: &mut Option<Box<dyn LocalProcess>>) -> String {
    match child {
        Some(c) => c.wait().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn supervise(
    pool: RunnerPool,
    config: LocalFallbackConfig,
    mut mode_rx: watch::Receiver<LocalRunnerMode>,
    state_tx: watch::Sender<LocalFallbackState>,
    spawner: Arc<dyn ProcessSpawner>,
) {
    let mut summary_rx = pool.summary();
    let mut child: Option<Box<dyn LocalProcess>> = None;
    let mut exit_sent = false;
    let mut kill_at: Option<Instant> = None;
    let mut not_before = Instant::now();
    let initial: PoolSummary = *summary_rx.borrow();
    let mut zero_since = (initial.remote_online == 0).then(Instant::now);
    loop {
        let mode = *mode_rx.borrow_and_update();
        let summary: PoolSummary = *summary_rx.borrow_and_update();
        if summary.remote_online > 0 {
            zero_since = None;
        } else if zero_since.is_none() {
            zero_since = Some(Instant::now());
        }
        let now = Instant::now();
        let grace_until = zero_since.map(|t| t + config.auto_grace);
        let wanted = match mode {
            LocalRunnerMode::Off => false,
            LocalRunnerMode::Always => true,
            LocalRunnerMode::Auto => grace_until.is_some_and(|t| t <= now),
        };

        // a running process that is no longer wanted gets Exit (or a kill before it connects)
        if child.is_some() && !wanted && !exit_sent {
            exit_sent = true;
            if pool.exit_local(config.exit_grace) {
                tracing::info!("local runner told to exit");
                kill_at = Some(now + config.exit_grace + Duration::from_secs(10));
            } else if let Some(c) = child.as_mut() {
                c.kill().await;
            }
            state_tx.send_modify(|s| s.status = LocalRunnerStatus::Stopping);
        }
        if child.is_none() && wanted && now >= not_before {
            match spawner.spawn(&config.command(&pool.local_token())) {
                Ok(process) => {
                    tracing::info!("local runner started");
                    child = Some(process);
                    exit_sent = false;
                    kill_at = None;
                    state_tx.send_modify(|s| s.status = LocalRunnerStatus::Running);
                }
                Err(e) => {
                    tracing::error!("local runner: {e}");
                    not_before = now + config.restart_delay;
                    state_tx.send_modify(|s| s.last_exit = Some(e));
                }
            }
        }
        state_tx.send_modify(|s| s.mode = mode);

        let mut timer = kill_at;
        if mode == LocalRunnerMode::Auto
            && child.is_none()
            && let Some(t) = grace_until.filter(|t| *t > now)
        {
            timer = Some(timer.map_or(t, |k| k.min(t)));
        }
        if wanted && child.is_none() && not_before > now {
            timer = Some(timer.map_or(not_before, |k| k.min(not_before)));
        }

        tokio::select! {
            changed = mode_rx.changed() => if changed.is_err() { break },
            changed = summary_rx.changed() => if changed.is_err() { break },
            exit = wait_child(&mut child) => {
                child = None;
                kill_at = None;
                let still_wanted = !exit_sent;
                tracing::info!("local runner exited: {exit}");
                state_tx.send_modify(|s| {
                    s.status = LocalRunnerStatus::Stopped;
                    s.last_exit = Some(exit);
                    if still_wanted {
                        s.restarts += 1;
                    }
                });
                if still_wanted {
                    not_before = Instant::now() + config.restart_delay;
                }
                exit_sent = false;
            }
            _ = sleep_until_opt(timer) => {
                if kill_at.is_some_and(|k| k <= Instant::now())
                    && let Some(c) = child.as_mut()
                {
                    tracing::warn!("local runner did not exit in time; killing it");
                    c.kill().await;
                    kill_at = None;
                }
            }
        }
    }
    if let Some(mut c) = child {
        c.kill().await;
    }
}
