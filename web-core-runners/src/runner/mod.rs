//! The runner runtime (feature `runner`).
//!
//! A product's runner binary is tiny:
//!
//! ```no_run
//! use web_core_runners::runner::{self, TaskRegistry};
//!
//! fn main() -> std::process::ExitCode {
//!     let registry = TaskRegistry::new("<contract hash>").raw("echo", |_ctx, input| Ok(input));
//!     runner::main(registry)
//! }
//! ```
//!
//! [`main`] dispatches the `task <type>` subcommand (the child process that executes one
//! attempt) and otherwise runs the [`Runner`]: connect, hello, execute assignments, reconnect
//! with backoff, exit on `Exit` or SIGTERM.

mod db;
mod executor;
mod link;
mod registry;
mod services;
mod transfer;
mod update;

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

pub use db::RunnerDb;
pub use executor::{ChildProcessExecutor, ExecJob, ExecResult, InlineExecutor, TaskExecutor};
pub use link::{Connector, Link, WsConnector, ws_url};
pub use registry::{ChildLine, ProgressUpdate, TaskContext, TaskFn, TaskRegistry, run_task_child};
pub use services::{NoopServices, ReadyCheck, ServiceManager, ServiceSpec, Systemctl, wait_ready};
pub use transfer::HttpTransfer;
pub use update::{ExecUpdater, NoUpdate, SelfUpdater};

use crate::protocol::{
    AppMessage, Assign, AttemptId, Hello, LogLevel, Outcome, PROTOCOL_VERSION, RunnerMessage,
};

/// How long running attempts may finish after SIGTERM before they are killed.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

const PROGRESS_THROTTLE: Duration = Duration::from_millis(500);

/// The runner's whole configuration: four values (plus the local flag the app uses for its
/// fallback). Capacity and every other knob come from the app.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// App base URL, `https://api.example.com` (`--url`, `RUNNER_URL`).
    pub url: String,
    /// Runner token from admin (`--token`, `RUNNER_TOKEN`).
    pub token: String,
    /// Work dir; holds `runner.db` and the attempt dirs (`--work-dir`, `RUNNER_WORK_DIR`).
    pub work_dir: PathBuf,
    /// Log filter, `info` by default (`--log-level`, `RUNNER_LOG_LEVEL`).
    pub log_level: String,
    /// Local mode (`--local`): the app's fallback runner; no self-update.
    pub local: bool,
}

impl RunnerConfig {
    /// Reads the config from command-line arguments, falling back to `RUNNER_*` env vars.
    /// `RUNNER_WORK_DIR` defaults to `/var/lib/runner`.
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let mut values: HashMap<&str, String> = HashMap::new();
        let mut local = false;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--local" => local = true,
                "--url" | "--token" | "--work-dir" | "--log-level" => {
                    let value = iter.next().ok_or(format!("{arg} needs a value"))?;
                    values.insert(arg.trim_start_matches("--"), value.clone());
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        let get = |key: &str, env: &str| values.get(key).cloned().or(std::env::var(env).ok());
        let url = get("url", "RUNNER_URL").ok_or("--url or RUNNER_URL is required")?;
        let token = get("token", "RUNNER_TOKEN").ok_or("--token or RUNNER_TOKEN is required")?;
        let work_dir = get("work-dir", "RUNNER_WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/runner"));
        let log_level = get("log-level", "RUNNER_LOG_LEVEL").unwrap_or_else(|| "info".into());
        Ok(Self {
            url,
            token,
            work_dir,
            log_level,
            local: local || std::env::var("RUNNER_LOCAL").is_ok_and(|v| v == "1"),
        })
    }
}

/// The runner binary's `main`: `task <type> ...` runs one task (child process),
/// `contract-hash` prints the contract hash, anything else runs the runner with
/// [`RunnerConfig::from_args`].
pub fn main(registry: TaskRegistry) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("task") => return run_task_child(&registry, &args[1..]),
        Some("contract-hash") => {
            println!("{}", registry.contract_hash());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let config = match RunnerConfig::from_args(&args) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            eprintln!(
                "usage: runner --url <app url> --token <token> [--work-dir <dir>] [--log-level <level>] [--local]"
            );
            return ExitCode::from(2);
        }
    };
    let level = config
        .log_level
        .parse::<tracing_subscriber::filter::LevelFilter>()
        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO);
    tracing_subscriber::fmt().with_max_level(level).init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async move {
        let runner = Runner::new(config, registry)?;
        runner.run().await
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("runner failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The pluggable parts of a runner. [`Runner::new`] uses the real ones; the in-process test
/// runner swaps the connector, executor, services and updater.
#[derive(Clone)]
pub struct RunnerDeps {
    /// Opens connections to the app.
    pub connector: Arc<dyn Connector>,
    /// Executes tasks.
    pub executor: Arc<dyn TaskExecutor>,
    /// Starts and stops managed services.
    pub services: Arc<dyn ServiceManager>,
    /// Self-update on `Outdated`.
    pub updater: Arc<dyn SelfUpdater>,
    /// File transfers.
    pub transfer: HttpTransfer,
}

/// A runner: connects to the app, executes assignments, reconnects with backoff.
pub struct Runner {
    config: RunnerConfig,
    registry: Arc<TaskRegistry>,
    deps: RunnerDeps,
    name: String,
}

impl Runner {
    /// A runner with the real WebSocket connector, child-process executor, `systemctl`
    /// services and exec self-update (disabled in local mode).
    pub fn new(config: RunnerConfig, registry: TaskRegistry) -> Result<Self, String> {
        let transfer = HttpTransfer::new()?;
        let updater: Arc<dyn SelfUpdater> = if config.local {
            Arc::new(NoUpdate)
        } else {
            Arc::new(ExecUpdater::new(transfer.client().clone()))
        };
        let deps = RunnerDeps {
            connector: Arc::new(WsConnector::new(&config.url, config.token.clone())),
            executor: Arc::new(ChildProcessExecutor::current_exe()?),
            services: Arc::new(Systemctl::default()),
            updater,
            transfer,
        };
        Ok(Self::with_deps(config, registry, deps))
    }

    /// A runner with custom parts.
    pub fn with_deps(config: RunnerConfig, registry: TaskRegistry, deps: RunnerDeps) -> Self {
        let name = if config.local {
            "local".to_string()
        } else {
            gethostname::gethostname().to_string_lossy().into_owned()
        };
        Self {
            config,
            registry: Arc::new(registry),
            deps,
            name,
        }
    }

    /// Overrides the name sent at hello (default: the host name, `local` in local mode).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Runs until the app sends `Exit` or the process gets SIGTERM / Ctrl-C.
    pub async fn run(self) -> Result<(), String> {
        self.run_until(shutdown_signal()).await
    }

    /// Runs until the app sends `Exit` or `shutdown` completes (then drains: running attempts
    /// get [`SHUTDOWN_GRACE`] to finish).
    pub async fn run_until(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), String> {
        std::fs::create_dir_all(&self.config.work_dir)
            .map_err(|e| format!("work dir {}: {e}", self.config.work_dir.display()))?;
        let db = RunnerDb::open(&self.config.work_dir.join("runner.db"))?;
        let attempts_root = self.config.work_dir.join("attempts");
        let dropped = db.boot_cleanup(&attempts_root)?;
        if dropped > 0 {
            tracing::warn!("dropped {dropped} in-flight attempts from before the restart");
        }
        db.log("info", "boot");
        let registry = self.registry.clone();
        let tool_versions = tokio::task::spawn_blocking(move || registry.collect_tool_versions())
            .await
            .unwrap_or_default();
        let shared = Arc::new(Shared {
            config: self.config,
            registry: self.registry,
            deps: self.deps,
            db,
            attempts_root,
            name: self.name,
        });
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut core = Core {
            shared,
            tool_versions,
            attempts: HashMap::new(),
            tasks: JoinSet::new(),
            pending: Vec::new(),
            events_tx,
            events_rx,
            heartbeat: Duration::from_secs(15),
            draining: false,
            outdated: false,
            pending_update: None,
            services_up: false,
            exit_deadline: None,
            shutdown_seen: false,
        };
        core.run(Box::pin(shutdown)).await;
        Ok(())
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        Err(_) => {
            let _ = ctrl_c.await;
        }
    }
}

struct Shared {
    config: RunnerConfig,
    registry: Arc<TaskRegistry>,
    deps: RunnerDeps,
    db: RunnerDb,
    attempts_root: PathBuf,
    name: String,
}

enum Event {
    Progress {
        attempt: AttemptId,
        update: ProgressUpdate,
    },
    Done {
        attempt: AttemptId,
        outcome: Option<Outcome>,
        elapsed_ms: u64,
    },
}

#[derive(PartialEq)]
enum SessionEnd {
    Disconnected,
    Exit,
}

struct Core {
    shared: Arc<Shared>,
    tool_versions: BTreeMap<String, String>,
    attempts: HashMap<AttemptId, CancellationToken>,
    tasks: JoinSet<()>,
    /// Results produced while disconnected, sent after the next `Welcome`.
    pending: Vec<(AttemptId, RunnerMessage)>,
    events_tx: mpsc::UnboundedSender<Event>,
    events_rx: mpsc::UnboundedReceiver<Event>,
    heartbeat: Duration,
    draining: bool,
    outdated: bool,
    pending_update: Option<(String, String)>,
    services_up: bool,
    exit_deadline: Option<Instant>,
    shutdown_seen: bool,
}

type ShutdownFuture = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

impl Core {
    async fn run(&mut self, mut shutdown: ShutdownFuture) {
        let mut backoff = Backoff::new();
        loop {
            if self.exit_deadline.is_some() && self.attempts.is_empty() {
                break;
            }
            let connected = tokio::select! {
                r = self.shared.deps.connector.connect() => Some(r),
                _ = shutdown.as_mut(), if !self.shutdown_seen => {
                    self.begin_shutdown();
                    None
                }
            };
            match connected {
                None => continue,
                Some(Ok(link)) => {
                    tracing::info!("connected to {}", self.shared.config.url);
                    if self.session(link, &mut shutdown, &mut backoff).await == SessionEnd::Exit {
                        break;
                    }
                    tracing::warn!("disconnected from the app");
                }
                Some(Err(e)) => tracing::warn!("cannot connect to {}: {e}", self.shared.config.url),
            }
            self.wait_offline(backoff.next(), &mut shutdown).await;
        }
        self.finish().await;
    }

    /// Waits before reconnecting while attempts keep running (their results are buffered).
    async fn wait_offline(
        &mut self,
        delay: Duration,
        shutdown: &mut ShutdownFuture,
    ) {
        let until = Instant::now() + delay;
        loop {
            let deadline = self.exit_deadline;
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return,
                Some(event) = self.events_rx.recv() => {
                    let _ = self.on_event(event, None).await;
                }
                _ = shutdown.as_mut(), if !self.shutdown_seen => self.begin_shutdown(),
                _ = sleep_until_opt(deadline), if deadline.is_some() && !self.attempts.is_empty() => {
                    self.kill_all();
                }
                Some(_) = self.tasks.join_next(), if !self.tasks.is_empty() => {}
            }
            if self.exit_deadline.is_some() && self.attempts.is_empty() {
                return;
            }
        }
    }

    async fn session(
        &mut self,
        mut link: Box<dyn Link>,
        shutdown: &mut ShutdownFuture,
        backoff: &mut Backoff,
    ) -> SessionEnd {
        let mut in_flight: Vec<AttemptId> = self.attempts.keys().copied().collect();
        in_flight.extend(self.pending.iter().map(|(id, _)| *id));
        let hello = RunnerMessage::Hello(Hello {
            runner_name: self.shared.name.clone(),
            protocol_version: PROTOCOL_VERSION,
            contract_hash: self.shared.registry.contract_hash().to_string(),
            tool_versions: self.tool_versions.clone(),
            in_flight,
            local: self.shared.config.local,
            runner_version: env!("CARGO_PKG_VERSION").to_string(),
        });
        if link.send(hello).await.is_err() {
            return SessionEnd::Disconnected;
        }
        if self.exit_deadline.is_some() && link.send(RunnerMessage::Draining).await.is_err() {
            return SessionEnd::Disconnected;
        }
        let mut heartbeat = self.heartbeat;
        let mut tick = new_interval(heartbeat);
        loop {
            if self.exit_deadline.is_some() && self.attempts.is_empty() {
                return SessionEnd::Exit;
            }
            if (self.draining || self.exit_deadline.is_some())
                && self.attempts.is_empty()
                && self.services_up
            {
                self.stop_services().await;
            }
            if self.attempts.is_empty()
                && let Some((url, sha256)) = self.pending_update.take()
            {
                self.self_update(&url, &sha256, &mut link).await;
            }
            if heartbeat != self.heartbeat {
                heartbeat = self.heartbeat;
                tick = new_interval(heartbeat);
            }
            let deadline = self.exit_deadline;
            let ok = tokio::select! {
                message = link.recv() => match message {
                    None => return SessionEnd::Disconnected,
                    Some(message) => self.on_message(message, &mut link, backoff).await,
                },
                Some(event) = self.events_rx.recv() => self.on_event(event, Some(&mut link)).await,
                _ = tick.tick() => {
                    let running = self.attempts.keys().copied().collect();
                    link.send(RunnerMessage::Heartbeat { running }).await.is_ok()
                }
                _ = shutdown.as_mut(), if !self.shutdown_seen => {
                    self.begin_shutdown();
                    link.send(RunnerMessage::Draining).await.is_ok()
                }
                _ = sleep_until_opt(deadline), if deadline.is_some() && !self.attempts.is_empty() => {
                    self.kill_all();
                    true
                }
                Some(_) = self.tasks.join_next(), if !self.tasks.is_empty() => true,
            };
            if !ok {
                return SessionEnd::Disconnected;
            }
        }
    }

    async fn on_message(
        &mut self,
        message: AppMessage,
        link: &mut Box<dyn Link>,
        backoff: &mut Backoff,
    ) -> bool {
        match message {
            AppMessage::Welcome {
                runner_id,
                settings,
                abort,
            } => {
                tracing::info!(%runner_id, capacity = settings.capacity, "welcomed");
                backoff.reset();
                self.heartbeat = Duration::from_millis(settings.heartbeat_ms.max(10));
                for attempt in &abort {
                    if let Some(token) = self.attempts.remove(attempt) {
                        token.cancel();
                    }
                }
                self.pending.retain(|(id, _)| !abort.contains(id));
                while !self.pending.is_empty() {
                    let (id, message) = self.pending.remove(0);
                    if link.send(message.clone()).await.is_err() {
                        self.pending.insert(0, (id, message));
                        return false;
                    }
                }
                if !self.outdated && !self.draining && self.exit_deadline.is_none() {
                    self.start_services(link).await;
                }
                true
            }
            AppMessage::Outdated {
                binary_url, sha256, ..
            } => {
                self.outdated = true;
                if self.shared.config.local {
                    tracing::error!(
                        "the app reports this local runner as outdated; it gets no work until the app ships a matching binary"
                    );
                } else {
                    tracing::warn!("outdated; updating from {binary_url}");
                    self.pending_update = Some((binary_url, sha256));
                }
                true
            }
            AppMessage::Assign(assign) => {
                if self.outdated || self.exit_deadline.is_some() || self.draining {
                    tracing::warn!(attempt = %assign.attempt, "assignment while not accepting work; ignored");
                    return true;
                }
                self.start_attempt(assign);
                true
            }
            AppMessage::Cancel { attempt } => {
                if let Some(token) = self.attempts.remove(&attempt) {
                    tracing::info!(%attempt, "cancelled by the app");
                    token.cancel();
                }
                self.pending.retain(|(id, _)| *id != attempt);
                true
            }
            AppMessage::Settings(settings) => {
                tracing::info!(capacity = settings.capacity, "settings updated");
                self.heartbeat = Duration::from_millis(settings.heartbeat_ms.max(10));
                true
            }
            AppMessage::Drain => {
                tracing::info!("draining");
                self.draining = true;
                true
            }
            AppMessage::Resume => {
                tracing::info!("resuming");
                self.draining = false;
                if !self.outdated && self.exit_deadline.is_none() {
                    self.start_services(link).await;
                }
                true
            }
            AppMessage::Exit { grace_ms } => {
                tracing::info!(grace_ms, "exit requested by the app");
                self.begin_exit(Duration::from_millis(grace_ms));
                true
            }
        }
    }

    async fn on_event(&mut self, event: Event, link: Option<&mut Box<dyn Link>>) -> bool {
        match event {
            Event::Progress { attempt, update } => {
                if !self.attempts.contains_key(&attempt) {
                    return true;
                }
                match link {
                    Some(link) => link
                        .send(RunnerMessage::Progress {
                            attempt,
                            percent: update.percent,
                            checkpoint: update.checkpoint,
                        })
                        .await
                        .is_ok(),
                    None => true,
                }
            }
            Event::Done {
                attempt,
                outcome,
                elapsed_ms,
            } => {
                let known = self.attempts.remove(&attempt).is_some();
                let Some(outcome) = outcome else {
                    return true;
                };
                if !known {
                    return true;
                }
                tracing::info!(%attempt, outcome = %outcome.describe(), "attempt finished");
                self.shared
                    .db
                    .log("info", &format!("{attempt}: {}", outcome.describe()));
                let message = RunnerMessage::Result {
                    attempt,
                    outcome,
                    elapsed_ms,
                };
                match link {
                    Some(link) => {
                        if link.send(message.clone()).await.is_err() {
                            self.pending.push((attempt, message));
                            return false;
                        }
                        true
                    }
                    None => {
                        self.pending.push((attempt, message));
                        true
                    }
                }
            }
        }
    }

    fn start_attempt(&mut self, assign: Assign) {
        let token = CancellationToken::new();
        self.attempts.insert(assign.attempt, token.clone());
        tracing::info!(attempt = %assign.attempt, task_type = %assign.task_type, number = assign.number, "attempt started");
        let shared = self.shared.clone();
        let events = self.events_tx.clone();
        self.tasks
            .spawn(async move { run_attempt(shared, assign, token, events).await });
    }

    fn kill_all(&mut self) {
        tracing::warn!("grace period over; killing {} attempts", self.attempts.len());
        for (_, token) in self.attempts.drain() {
            token.cancel();
        }
    }

    fn begin_shutdown(&mut self) {
        self.shutdown_seen = true;
        tracing::info!("shutdown requested; draining");
        self.begin_exit(SHUTDOWN_GRACE);
    }

    fn begin_exit(&mut self, grace: Duration) {
        let deadline = Instant::now() + grace;
        self.exit_deadline = Some(match self.exit_deadline {
            Some(existing) => existing.min(deadline),
            None => deadline,
        });
        if grace.is_zero() {
            self.kill_all();
        }
    }

    async fn start_services(&mut self, link: &mut Box<dyn Link>) {
        if self.services_up {
            return;
        }
        self.services_up = true;
        for spec in self.shared.registry.services() {
            match self.shared.deps.services.start(spec).await {
                Ok(()) => tracing::info!(unit = %spec.unit, "service started"),
                Err(e) => {
                    tracing::error!(unit = %spec.unit, "service failed to start: {e}");
                    let _ = link
                        .send(RunnerMessage::Log {
                            level: LogLevel::Error,
                            message: format!("service {} failed to start: {e}", spec.unit),
                            attempt: None,
                        })
                        .await;
                }
            }
        }
    }

    async fn stop_services(&mut self) {
        if !self.services_up {
            return;
        }
        self.services_up = false;
        for spec in self.shared.registry.services().iter().rev() {
            match self.shared.deps.services.stop(spec).await {
                Ok(()) => tracing::info!(unit = %spec.unit, "service stopped"),
                Err(e) => tracing::warn!(unit = %spec.unit, "service failed to stop: {e}"),
            }
        }
    }

    async fn self_update(&mut self, url: &str, sha256: &str, link: &mut Box<dyn Link>) {
        if let Err(e) = self
            .shared
            .deps
            .updater
            .update(url, sha256, &self.shared.config.token)
            .await
        {
            tracing::error!("self-update failed: {e}");
            let _ = link
                .send(RunnerMessage::Log {
                    level: LogLevel::Error,
                    message: format!("self-update failed: {e}"),
                    attempt: None,
                })
                .await;
        }
    }

    async fn finish(&mut self) {
        for (_, token) in self.attempts.drain() {
            token.cancel();
        }
        while self.tasks.join_next().await.is_some() {}
        self.stop_services().await;
        self.shared.db.log("info", "exit");
        tracing::info!("runner exited");
    }
}

fn new_interval(period: Duration) -> tokio::time::Interval {
    let mut tick = tokio::time::interval_at(Instant::now() + period, period);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Reconnect backoff: 0.5 s doubling to 30 s, with ±25 % jitter.
struct Backoff {
    current: Duration,
}

impl Backoff {
    const MIN: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(30);

    fn new() -> Self {
        Self { current: Self::MIN }
    }

    fn reset(&mut self) {
        self.current = Self::MIN;
    }

    fn next(&mut self) -> Duration {
        let base = self.current;
        self.current = (self.current * 2).min(Self::MAX);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        // jitter factor in [0.75, 1.25)
        let factor = 0.75 + f64::from(nanos % 1000) / 2000.0;
        base.mul_f64(factor)
    }
}

/// One attempt: download inputs, execute, upload outputs, wipe the work dir, report.
async fn run_attempt(
    shared: Arc<Shared>,
    assign: Assign,
    cancel: CancellationToken,
    events: mpsc::UnboundedSender<Event>,
) {
    let started = Instant::now();
    let dir = shared.attempts_root.join(assign.attempt.to_string());
    if let Err(e) = shared
        .db
        .begin_attempt(assign.attempt, &assign.task_type, &dir)
    {
        tracing::warn!("runner db: {e}");
    }
    let outcome = attempt_pipeline(&shared, &assign, &dir, &cancel, &events).await;
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("cannot wipe {}: {e}", dir.display());
    }
    if let Err(e) = shared.db.end_attempt(assign.attempt, &dir) {
        tracing::warn!("runner db: {e}");
    }
    let _ = events.send(Event::Done {
        attempt: assign.attempt,
        outcome,
        elapsed_ms: started.elapsed().as_millis() as u64,
    });
}

async fn attempt_pipeline(
    shared: &Shared,
    assign: &Assign,
    dir: &std::path::Path,
    cancel: &CancellationToken,
    events: &mpsc::UnboundedSender<Event>,
) -> Option<Outcome> {
    for sub in ["in", "out", "tmp"] {
        if let Err(e) = tokio::fs::create_dir_all(dir.join(sub)).await {
            return Some(Outcome::TransferFailed {
                reason: format!("cannot create work dir: {e}"),
            });
        }
    }
    for file in &assign.inputs {
        if !crate::protocol::is_valid_slot_name(&file.slot) {
            return Some(Outcome::Failed {
                reason: format!("invalid slot name {}", file.slot),
            });
        }
        let dest = dir.join("in").join(&file.slot);
        let result = tokio::select! {
            r = shared.deps.transfer.download(file, &dest) => r,
            _ = cancel.cancelled() => return None,
        };
        if let Err(reason) = result {
            return Some(Outcome::TransferFailed { reason });
        }
    }

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();
    let attempt = assign.attempt;
    let forward_events = events.clone();
    let forwarder = tokio::spawn(async move {
        let mut last_sent: Option<Instant> = None;
        while let Some(update) = progress_rx.recv().await {
            let due = last_sent.is_none_or(|t| t.elapsed() >= PROGRESS_THROTTLE);
            if update.checkpoint.is_some() || due || update.percent >= 100.0 {
                last_sent = Some(Instant::now());
                let _ = forward_events.send(Event::Progress { attempt, update });
            }
        }
    });
    let job = ExecJob {
        task_type: assign.task_type.clone(),
        input: assign.input.clone(),
        work_dir: dir.to_path_buf(),
    };
    let exec_cancel = cancel.child_token();
    let timeout = Duration::from_millis(assign.timeout_ms);
    let result = tokio::time::timeout(
        timeout,
        shared
            .deps
            .executor
            .execute(job, progress_tx, exec_cancel.clone()),
    )
    .await;
    // the executor dropped its progress sender; let pending updates through before the result
    let _ = forwarder.await;
    let output = match result {
        Err(_) => {
            exec_cancel.cancel();
            return Some(Outcome::TimedOut);
        }
        Ok(ExecResult::Cancelled) => return None,
        Ok(ExecResult::Failed(reason)) => return Some(Outcome::Failed { reason }),
        Ok(ExecResult::Crashed {
            exit_code,
            stderr_tail,
        }) => {
            return Some(Outcome::Crashed {
                exit_code,
                stderr_tail,
            });
        }
        Ok(ExecResult::Output(output)) => output,
    };
    if cancel.is_cancelled() {
        return None;
    }

    let mut files = Vec::new();
    for slot in &assign.outputs {
        if !crate::protocol::is_valid_slot_name(&slot.slot) {
            return Some(Outcome::Failed {
                reason: format!("invalid slot name {}", slot.slot),
            });
        }
        let path = dir.join("out").join(&slot.slot);
        if !path.exists() {
            if slot.optional {
                continue;
            }
            return Some(Outcome::Failed {
                reason: format!("the task did not write output slot {}", slot.slot),
            });
        }
        let result = tokio::select! {
            r = shared.deps.transfer.upload(slot, &path) => r,
            _ = cancel.cancelled() => return None,
        };
        match result {
            Ok(uploaded) => files.push(uploaded),
            Err(reason) => return Some(Outcome::TransferFailed { reason }),
        }
    }
    Some(Outcome::Ok { output, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_args() {
        let args: Vec<String> = [
            "--url",
            "http://127.0.0.1:3000",
            "--token",
            "t",
            "--local",
            "--work-dir",
            "/tmp/x",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let config = RunnerConfig::from_args(&args).expect("config");
        assert_eq!(config.url, "http://127.0.0.1:3000");
        assert_eq!(config.token, "t");
        assert!(config.local);
        assert_eq!(config.work_dir, PathBuf::from("/tmp/x"));
        assert!(RunnerConfig::from_args(&["--bogus".to_string()]).is_err());
    }

    #[test]
    fn backoff_grows_and_resets() {
        let mut b = Backoff::new();
        let first = b.next();
        assert!(first <= Duration::from_millis(625));
        for _ in 0..10 {
            b.next();
        }
        assert!(b.next() <= Duration::from_millis(37_500));
        b.reset();
        assert!(b.next() <= Duration::from_millis(625));
    }
}
