use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use tokio::{
    sync::{oneshot, watch},
    task::{AbortHandle, JoinHandle},
};

use super::{
    local::{LocalCommand, LocalProcess, ProcessSpawner},
    pool::{RunnerPool, RunnerSession},
};
use crate::{
    protocol::{AppMessage, RunnerMessage},
    runner::{
        Connector, HttpTransfer, InlineExecutor, Link, NoUpdate, NoopServices, Runner,
        RunnerConfig, RunnerDeps, SelfUpdater, ServiceManager, TaskRegistry,
    },
};

struct Control {
    online: AtomicBool,
    generation: watch::Sender<u64>,
}

struct InProcessConnector {
    pool: RunnerPool,
    token: String,
    control: Arc<Control>,
}

#[async_trait]
impl Connector for InProcessConnector {
    async fn connect(&self) -> Result<Box<dyn Link>, String> {
        if !self.control.online.load(Ordering::SeqCst) {
            return Err("offline (test)".into());
        }
        let session = self
            .pool
            .attach(&self.token)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let mut generation = self.control.generation.subscribe();
        generation.mark_unchanged();
        Ok(Box::new(InProcessLink {
            session,
            generation,
        }))
    }
}

struct InProcessLink {
    session: RunnerSession,
    generation: watch::Receiver<u64>,
}

#[async_trait]
impl Link for InProcessLink {
    async fn send(&mut self, message: RunnerMessage) -> Result<(), String> {
        if self.generation.has_changed().unwrap_or(true) {
            return Err("disconnected (test)".into());
        }
        self.session.handle(message);
        Ok(())
    }

    async fn recv(&mut self) -> Option<AppMessage> {
        if self.generation.has_changed().unwrap_or(true) {
            return None;
        }
        tokio::select! {
            message = self.session.recv() => message,
            _ = self.generation.changed() => None,
        }
    }
}

/// Builder of an [`InProcessRunner`].
pub struct InProcessRunnerBuilder {
    pool: RunnerPool,
    token: String,
    registry: TaskRegistry,
    name: String,
    local: bool,
    services: Arc<dyn ServiceManager>,
    updater: Arc<dyn SelfUpdater>,
}

impl InProcessRunnerBuilder {
    /// Sets the name sent at hello (default `in-process`).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Runs in local mode (the app's fallback).
    pub fn local(mut self, local: bool) -> Self {
        self.local = local;
        self
    }

    /// Uses this service manager (default: [`NoopServices`]).
    pub fn services(mut self, services: Arc<dyn ServiceManager>) -> Self {
        self.services = services;
        self
    }

    /// Uses this self-updater (default: [`NoUpdate`]).
    pub fn updater(mut self, updater: Arc<dyn SelfUpdater>) -> Self {
        self.updater = updater;
        self
    }

    /// Starts the runner loop on the current runtime.
    pub fn start(self) -> InProcessRunner {
        let work_dir =
            std::env::temp_dir().join(format!("web-core-runner-{}", uuid::Uuid::new_v4()));
        let (generation, _) = watch::channel(0u64);
        let control = Arc::new(Control {
            online: AtomicBool::new(true),
            generation,
        });
        let mut runner = InProcessRunner {
            pool: self.pool,
            token: self.token,
            registry: self.registry,
            name: self.name,
            local: self.local,
            services: self.services,
            updater: self.updater,
            work_dir,
            control,
            task: None,
            shutdown: None,
        };
        runner.spawn();
        runner
    }
}

/// A runner running in the app's process, talking to the pool over channels instead of a
/// socket, executing registry closures on blocking threads ([`InlineExecutor`]). For app
/// tests: dispatch, lease loss, late results, crashes and the local fallback without a
/// network. File slots still go over HTTP, so a test with files serves
/// [`RunnerPool::router`] on a local port.
pub struct InProcessRunner {
    pool: RunnerPool,
    token: String,
    registry: TaskRegistry,
    name: String,
    local: bool,
    services: Arc<dyn ServiceManager>,
    updater: Arc<dyn SelfUpdater>,
    work_dir: PathBuf,
    control: Arc<Control>,
    task: Option<JoinHandle<Result<(), String>>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl InProcessRunner {
    /// A builder for a runner authenticating with `token`, executing `registry`.
    pub fn builder(
        pool: &RunnerPool,
        token: impl Into<String>,
        registry: TaskRegistry,
    ) -> InProcessRunnerBuilder {
        InProcessRunnerBuilder {
            pool: pool.clone(),
            token: token.into(),
            registry,
            name: "in-process".into(),
            local: false,
            services: Arc::new(NoopServices),
            updater: Arc::new(NoUpdate),
        }
    }

    /// Starts a runner with defaults.
    pub fn start(pool: &RunnerPool, token: impl Into<String>, registry: TaskRegistry) -> Self {
        Self::builder(pool, token, registry).start()
    }

    fn spawn(&mut self) {
        let config = RunnerConfig {
            url: "in-process".into(),
            token: self.token.clone(),
            work_dir: self.work_dir.clone(),
            log_level: "debug".into(),
            local: self.local,
        };
        let transfer = match HttpTransfer::new() {
            Ok(t) => t,
            Err(e) => {
                self.task = Some(tokio::spawn(async move { Err(e) }));
                return;
            }
        };
        let registry = Arc::new(self.registry.clone());
        let deps = RunnerDeps {
            connector: Arc::new(InProcessConnector {
                pool: self.pool.clone(),
                token: self.token.clone(),
                control: self.control.clone(),
            }),
            executor: Arc::new(InlineExecutor::new(registry)),
            services: self.services.clone(),
            updater: self.updater.clone(),
            transfer,
        };
        let runner =
            Runner::with_deps(config, self.registry.clone(), deps).with_name(self.name.clone());
        let (tx, rx) = oneshot::channel::<()>();
        self.shutdown = Some(tx);
        self.task = Some(tokio::spawn(runner.run_until(async move {
            let _ = rx.await;
        })));
    }

    /// Drops the current connection (the runner reconnects after its backoff).
    pub fn disconnect(&self) {
        self.control.generation.send_modify(|g| *g += 1);
    }

    /// Takes the runner off the network (`false`: connections fail and the current one drops)
    /// or back on (`true`).
    pub fn set_online(&self, online: bool) {
        self.control.online.store(online, Ordering::SeqCst);
        if !online {
            self.disconnect();
        }
    }

    /// Simulates a kill -9: aborts the runner loop and its attempts without a goodbye.
    pub async fn kill(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }

    /// Kills and boots the runner again on the same work dir (it resumes nothing).
    pub async fn restart(&mut self) {
        self.kill().await;
        self.spawn();
    }

    /// Sends the shutdown signal (like SIGTERM) and waits for the runner to exit.
    pub async fn shutdown(&mut self) -> String {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.wait().await
    }

    /// Waits until the runner loop ends (after `Exit`, shutdown or kill). Cancel-safe.
    pub async fn wait(&mut self) -> String {
        let Some(task) = self.task.as_mut() else {
            return "not running".into();
        };
        let result = task.await;
        self.task = None;
        match result {
            Ok(Ok(())) => "exited".into(),
            Ok(Err(e)) => format!("failed: {e}"),
            Err(e) if e.is_cancelled() => "killed".into(),
            Err(e) => format!("panicked: {e}"),
        }
    }

    /// Whether the runner loop has ended.
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(|t| t.is_finished())
    }

    fn abort_handle(&self) -> Option<AbortHandle> {
        self.task.as_ref().map(|t| t.abort_handle())
    }
}

impl Drop for InProcessRunner {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

/// A [`ProcessSpawner`] that starts an [`InProcessRunner`] in local mode instead of a child
/// process, so the local fallback supervisor can be tested (and used in app tests with
/// `LocalRunnerMode::Always`).
#[derive(Clone)]
pub struct InProcessSpawner {
    pool: RunnerPool,
    registry: TaskRegistry,
    spawned: Arc<AtomicUsize>,
    latest: Arc<Mutex<Option<AbortHandle>>>,
}

impl InProcessSpawner {
    /// A spawner whose local runners execute `registry`.
    pub fn new(pool: &RunnerPool, registry: TaskRegistry) -> Self {
        Self {
            pool: pool.clone(),
            registry,
            spawned: Arc::new(AtomicUsize::new(0)),
            latest: Arc::new(Mutex::new(None)),
        }
    }

    /// How many local runners were started so far.
    pub fn spawned(&self) -> usize {
        self.spawned.load(Ordering::SeqCst)
    }

    /// Kills the most recently started local runner (simulates a crash).
    pub fn crash_latest(&self) {
        if let Ok(latest) = self.latest.lock()
            && let Some(handle) = latest.as_ref()
        {
            handle.abort();
        }
    }
}

impl ProcessSpawner for InProcessSpawner {
    fn spawn(&self, command: &LocalCommand) -> Result<Box<dyn LocalProcess>, String> {
        let token = command
            .arg_value("--token")
            .ok_or("no --token argument")?
            .to_string();
        let runner = InProcessRunner::builder(&self.pool, token, self.registry.clone())
            .name("local")
            .local(true)
            .start();
        if let Ok(mut latest) = self.latest.lock() {
            *latest = runner.abort_handle();
        }
        self.spawned.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(InProcessProcess(runner)))
    }
}

struct InProcessProcess(InProcessRunner);

#[async_trait]
impl LocalProcess for InProcessProcess {
    async fn wait(&mut self) -> String {
        self.0.wait().await
    }

    async fn kill(&mut self) {
        self.0.kill().await;
    }
}
