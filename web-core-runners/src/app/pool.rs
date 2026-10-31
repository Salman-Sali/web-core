use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use rand_core::{OsRng, RngCore};
use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use web_core::{
    auth::password_hasher::{hash_password, verify_password},
    error::Error,
    utils::signatory,
};

use super::{
    admin::{RegisterRunnerResponse, RunnerSettingsPatch, RunnerView, TaskHistoryView},
    handle::{
        OutputFile, OutputTarget, RetryHook, SubmitOptions, TaskError, TaskFiles, TaskHandle,
        TaskOutput, TaskProgress, TypedTaskHandle,
    },
    store::{
        AttemptRecord, AttemptStatus, RunnerRecord, RunnerSettings, RunnerState, RunnerStatus,
        RunnerStore, TaskRecord, TaskStatus,
    },
};
use crate::{
    hash::sha256_file,
    protocol::{
        AppMessage, Assign, AttemptId, BINARY_PATH, FileSlot, Hello, InputFile, LogLevel,
        Outcome, OutputSlot, PROTOCOL_VERSION, PushedSettings, RunnerId, RunnerMessage,
        SlotDirection, Task, TaskId, UploadedFile, is_valid_slot_name,
    },
};

const STATUS_WRITE_EVERY: Duration = Duration::from_secs(30);

/// Configuration of a [`RunnerPool`].
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// The app's contract hash; runners with another are outdated.
    pub contract_hash: String,
    /// Base URL runners reach the app at (`https://api.example.com`), for signed file URLs and
    /// the binary URL.
    pub public_url: String,
    /// Base URL the local fallback reaches the app at (`http://127.0.0.1:<port>`); defaults to
    /// `public_url`.
    pub local_url: Option<String>,
    /// Lease length; renewed by heartbeats and progress. Default 60 s.
    pub lease: Duration,
    /// Heartbeat interval pushed to runners. Default 15 s.
    pub heartbeat: Duration,
    /// How often expired leases are swept. Default 5 s.
    pub sweep_interval: Duration,
    /// Per-attempt timeout unless the submission sets one. Default 2 h.
    pub default_timeout: Duration,
    /// Attempts allowed unless the task type or submission sets one. Default 3.
    pub default_max_attempts: u32,
    /// Attempts allowed per task type.
    pub max_attempts: HashMap<String, u32>,
    /// Ed25519 signing key (64 hex chars) for file URLs; generated at start when `None`.
    pub signing_key: Option<String>,
    /// The runner binary served at `/runners/binary` (x86-64 build for office runners).
    pub binary_path: Option<PathBuf>,
}

impl PoolConfig {
    /// A config with defaults.
    pub fn new(contract_hash: impl Into<String>, public_url: impl Into<String>) -> Self {
        Self {
            contract_hash: contract_hash.into(),
            public_url: public_url.into().trim_end_matches('/').to_string(),
            local_url: None,
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            sweep_interval: Duration::from_secs(5),
            default_timeout: Duration::from_secs(2 * 60 * 60),
            default_max_attempts: 3,
            max_attempts: HashMap::new(),
            signing_key: None,
            binary_path: None,
        }
    }

    /// Sets the local fallback's base URL.
    pub fn with_local_url(mut self, url: impl Into<String>) -> Self {
        self.local_url = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    /// Sets the lease length.
    pub fn with_lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// Sets the heartbeat interval.
    pub fn with_heartbeat(mut self, heartbeat: Duration) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    /// Sets the sweep interval.
    pub fn with_sweep_interval(mut self, interval: Duration) -> Self {
        self.sweep_interval = interval;
        self
    }

    /// Sets the default per-attempt timeout.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Sets the default attempts allowed.
    pub fn with_default_max_attempts(mut self, max_attempts: u32) -> Self {
        self.default_max_attempts = max_attempts;
        self
    }

    /// Sets the attempts allowed for one task type.
    pub fn with_max_attempts(mut self, task_type: impl Into<String>, max_attempts: u32) -> Self {
        self.max_attempts.insert(task_type.into(), max_attempts);
        self
    }

    /// Uses a fixed signing key (64 hex chars) instead of a generated one.
    pub fn with_signing_key(mut self, key: impl Into<String>) -> Self {
        self.signing_key = Some(key.into());
        self
    }

    /// Serves this runner binary at `/runners/binary`.
    pub fn with_binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary_path = Some(path.into());
        self
    }
}

/// What the local fallback supervisor watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PoolSummary {
    /// Remote runners connected, enabled, not drained and with a matching contract.
    pub remote_online: usize,
    /// Whether the local fallback runner is connected.
    pub local_connected: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct BinaryInfo {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
}

/// The app side of the runners: registered runners, live connections, the task queue, the
/// scheduler and the lease sweeper. Cheap to clone.
#[derive(Clone)]
pub struct RunnerPool {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    config: PoolConfig,
    secret_key: String,
    public_key: String,
    store: Arc<dyn RunnerStore>,
    store_tx: mpsc::UnboundedSender<StoreOp>,
    state: Mutex<State>,
    summary: watch::Sender<PoolSummary>,
    pub(crate) binary: Option<BinaryInfo>,
    local_secret: String,
    next_conn: AtomicU64,
}

enum StoreOp {
    Status(RunnerId, RunnerStatus),
    Task(TaskRecord),
    Attempt(AttemptRecord),
    Flush(oneshot::Sender<()>),
}

#[derive(Default)]
struct State {
    runners: HashMap<RunnerId, RunnerEntry>,
    tasks: HashMap<TaskId, TaskEntry>,
    queue: VecDeque<TaskId>,
    /// Current attempts → their task.
    attempts: HashMap<AttemptId, TaskId>,
}

struct RunnerEntry {
    record: RunnerRecord,
    local: bool,
    drained: bool,
    conn: Option<Conn>,
    running: HashSet<AttemptId>,
    status_written: Option<Instant>,
}

struct Conn {
    id: u64,
    tx: mpsc::UnboundedSender<AppMessage>,
    hello: Option<Hello>,
    state: RunnerState,
    /// Exit sent or the runner announced `Draining`: its attempts end with the connection.
    leaving: bool,
}

impl RunnerEntry {
    fn accepts_work(&self) -> bool {
        self.record.enabled
            && !self.drained
            && self.running.len() < self.record.settings.capacity as usize
            && self.online()
    }

    fn online(&self) -> bool {
        self.conn
            .as_ref()
            .is_some_and(|c| c.hello.is_some() && c.state == RunnerState::Online)
    }

    fn ratio(&self) -> f64 {
        self.running.len() as f64 / f64::from(self.record.settings.weight.max(1))
    }

    fn state(&self) -> RunnerState {
        match &self.conn {
            Some(c) if c.hello.is_some() => c.state,
            _ => RunnerState::Offline,
        }
    }
}

struct PreparedInput {
    slot: String,
    path: PathBuf,
    sha256: String,
    size: u64,
}

struct Current {
    attempt: AttemptId,
    number: u32,
    runner: RunnerId,
    runner_name: String,
    lease_until: Instant,
    started_at: DateTime<Utc>,
    percent: f32,
    checkpoint: Option<Value>,
    finalizing: bool,
}

struct TaskEntry {
    id: TaskId,
    task_type: String,
    input: Value,
    original_input: Value,
    inputs: Vec<PreparedInput>,
    outputs: Vec<OutputTarget>,
    max_attempts: u32,
    timeout: Duration,
    reference: Option<String>,
    retry_hook: Option<RetryHook>,
    attempts_made: u32,
    current: Option<Current>,
    last_checkpoint: Option<Value>,
    last_error: Option<String>,
    created_at: DateTime<Utc>,
    result_tx: Option<oneshot::Sender<Result<TaskOutput, TaskError>>>,
    progress_tx: watch::Sender<TaskProgress>,
}

impl TaskEntry {
    fn record(&self, status: TaskStatus) -> TaskRecord {
        TaskRecord {
            id: self.id,
            task_type: self.task_type.clone(),
            input: self.original_input.clone(),
            status,
            max_attempts: self.max_attempts,
            reference: self.reference.clone(),
            created_at: self.created_at,
            finished_at: None,
            output: None,
            failure: None,
        }
    }

    fn attempt_record(&self, cur: &Current, lease: Duration) -> AttemptRecord {
        let remaining = cur.lease_until.saturating_duration_since(Instant::now());
        let lease_until = Utc::now()
            + chrono::Duration::from_std(remaining.min(lease)).unwrap_or_default();
        AttemptRecord {
            id: cur.attempt,
            task_id: self.id,
            number: cur.number,
            runner_id: cur.runner,
            runner_name: cur.runner_name.clone(),
            lease_until,
            status: AttemptStatus::Running,
            progress: cur.percent,
            checkpoint: cur.checkpoint.clone(),
            started_at: cur.started_at,
            ended_at: None,
            error: None,
        }
    }
}

enum AttemptEnd {
    Done(Value, Vec<OutputFile>),
    Failed(String),
    Retry(AttemptStatus, String),
    Aborted,
}

/// Path an upload of `attempt` is written to before verification: `<target>.<attempt>.part`.
pub(crate) fn part_path(target: &Path, attempt: AttemptId) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{attempt}.part"));
    target.with_file_name(name)
}

fn signed_data(attempt: AttemptId, slot: &str, dir: &str, exp: u64) -> String {
    format!("{attempt}/{slot}/{dir}/{exp}")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A new token for runner `id` and the Argon2 hash to store: `(token, token_hash)`. The token
/// is `<runner id>.<secret>` (64 hex chars of secret) and is shown once; only the hash of the
/// secret is kept. [`RunnerPool::register`] uses it; an app whose admin writes the `runners`
/// table from another process calls it directly (blocking: Argon2 takes a while).
pub fn issue_runner_token(id: RunnerId) -> Result<(String, String), Error> {
    let secret = random_hex(32);
    let token_hash = hash_password(&secret)?;
    Ok((format!("{id}.{secret}"), token_hash))
}

/// What a signed file URL grants.
pub(crate) enum FileAccess {
    /// Serve this input file.
    Download(PathBuf),
    /// Append to this part file.
    Upload(PathBuf),
}

impl RunnerPool {
    /// Loads the registered runners from `store`, prepares the signing keys and the served
    /// binary, and starts the lease sweeper and the store writer. Call inside a tokio runtime.
    pub async fn start(config: PoolConfig, store: Arc<dyn RunnerStore>) -> Result<Self, Error> {
        let secret_key = match &config.signing_key {
            Some(key) => key.clone(),
            None => random_hex(32),
        };
        let key_bytes: [u8; 32] = hex::decode(&secret_key)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| {
                Error::new_something_went_wrong("signing key must be 64 hex chars".into())
            })?;
        let public_key =
            hex::encode(ed25519_dalek::SigningKey::from_bytes(&key_bytes).verifying_key().to_bytes());

        let binary = match &config.binary_path {
            Some(path) => match sha256_file(path.clone()).await {
                Ok((sha256, _)) => Some(BinaryInfo {
                    path: path.clone(),
                    sha256,
                }),
                Err(e) => {
                    tracing::warn!(
                        "runner binary {} unavailable ({e}); outdated runners cannot update",
                        path.display()
                    );
                    None
                }
            },
            None => None,
        };

        let mut state = State::default();
        for record in store.load_runners().await? {
            let mut record = record;
            record.status.state = RunnerState::Offline;
            state.runners.insert(
                record.id,
                RunnerEntry {
                    record,
                    local: false,
                    drained: false,
                    conn: None,
                    running: HashSet::new(),
                    status_written: None,
                },
            );
        }
        state.runners.insert(
            RunnerId::LOCAL,
            RunnerEntry {
                record: RunnerRecord {
                    id: RunnerId::LOCAL,
                    name: "local".into(),
                    token_hash: String::new(),
                    settings: RunnerSettings {
                        capacity: 1,
                        priority: i32::MAX,
                        weight: 1,
                    },
                    enabled: true,
                    created_at: Utc::now(),
                    status: RunnerStatus::default(),
                },
                local: true,
                drained: false,
                conn: None,
                running: HashSet::new(),
                status_written: None,
            },
        );

        let (store_tx, store_rx) = mpsc::unbounded_channel();
        let (summary, _) = watch::channel(PoolSummary::default());
        let inner = Arc::new(Inner {
            config,
            secret_key,
            public_key,
            store: store.clone(),
            store_tx,
            state: Mutex::new(state),
            summary,
            binary,
            local_secret: random_hex(32),
            next_conn: AtomicU64::new(1),
        });
        tokio::spawn(store_writer(store, store_rx));
        tokio::spawn(sweeper(Arc::downgrade(&inner), inner.config.sweep_interval));
        Ok(Self { inner })
    }

    /// The pool's configuration.
    pub fn config(&self) -> &PoolConfig {
        &self.inner.config
    }

    /// Submits a task and returns its handle. Inputs without a known hash are hashed first.
    pub async fn submit(
        &self,
        task_type: &str,
        input: Value,
        files: TaskFiles,
        options: SubmitOptions,
    ) -> Result<TaskHandle, Error> {
        let mut seen = HashSet::new();
        for slot in files
            .inputs
            .iter()
            .map(|i| &i.slot)
            .chain(files.outputs.iter().map(|o| &o.slot))
        {
            if !is_valid_slot_name(slot) {
                return Err(Error::bad_request_error(&format!("invalid slot name {slot}")));
            }
            if !seen.insert(slot.clone()) {
                return Err(Error::bad_request_error(&format!("duplicate slot {slot}")));
            }
        }
        let mut inputs = Vec::with_capacity(files.inputs.len());
        for source in files.inputs {
            let (sha256, size) = match source.sha256 {
                Some(sha256) => {
                    let size = tokio::fs::metadata(&source.path)
                        .await
                        .map_err(|e| {
                            Error::bad_request_error(&format!(
                                "input {} ({}): {e}",
                                source.slot,
                                source.path.display()
                            ))
                        })?
                        .len();
                    (sha256, size)
                }
                None => sha256_file(source.path.clone()).await.map_err(|e| {
                    Error::bad_request_error(&format!(
                        "input {} ({}): {e}",
                        source.slot,
                        source.path.display()
                    ))
                })?,
            };
            inputs.push(PreparedInput {
                slot: source.slot,
                path: source.path,
                sha256,
                size,
            });
        }

        let config = &self.inner.config;
        let max_attempts = options
            .max_attempts
            .or_else(|| config.max_attempts.get(task_type).copied())
            .unwrap_or(config.default_max_attempts)
            .max(1);
        let id = TaskId::new();
        let (result_tx, rx) = oneshot::channel();
        let (progress_tx, progress) = watch::channel(TaskProgress::default());
        let entry = TaskEntry {
            id,
            task_type: task_type.to_string(),
            original_input: input.clone(),
            input,
            inputs,
            outputs: files.outputs,
            max_attempts,
            timeout: options.timeout.unwrap_or(config.default_timeout),
            reference: options.reference,
            retry_hook: options.retry_hook,
            attempts_made: 0,
            current: None,
            last_checkpoint: None,
            last_error: None,
            created_at: Utc::now(),
            result_tx: Some(result_tx),
            progress_tx,
        };
        {
            let mut st = self.inner.lock();
            self.inner
                .store_op(StoreOp::Task(entry.record(TaskStatus::Queued)));
            st.tasks.insert(id, entry);
            st.queue.push_back(id);
            self.inner.schedule(&mut st);
        }
        Ok(TaskHandle {
            id,
            rx,
            progress,
            pool: self.clone(),
            done: false,
        })
    }

    /// Submits a task of contract type `T`, checking `files` against `T::files(&input)`.
    pub async fn submit_typed<T: Task>(
        &self,
        input: T::Input,
        files: TaskFiles,
        options: SubmitOptions,
    ) -> Result<TypedTaskHandle<T>, Error> {
        for FileSlot {
            name,
            direction,
            optional,
        } in T::files(&input)
        {
            let present = match direction {
                SlotDirection::In => files.inputs.iter().any(|i| i.slot == name),
                SlotDirection::Out => optional || files.outputs.iter().any(|o| o.slot == name),
            };
            if !present {
                return Err(Error::bad_request_error(&format!(
                    "{} needs file slot {name}",
                    T::TYPE
                )));
            }
        }
        let value = serde_json::to_value(&input).map_err(|e| {
            Error::new_something_went_wrong(format!("serialise {} input: {e}", T::TYPE))
        })?;
        let inner = self.submit(T::TYPE, value, files, options).await?;
        Ok(TypedTaskHandle {
            inner,
            _task: std::marker::PhantomData,
        })
    }

    /// Cancels a task (dequeue, or `Cancel` to its runner); its handle resolves to
    /// [`TaskError::Cancelled`]. No-op for finished tasks.
    pub fn cancel_task(&self, id: TaskId) {
        let mut st = self.inner.lock();
        let Some(task) = st.tasks.get(&id) else {
            return;
        };
        match task.current.as_ref() {
            Some(cur) => {
                let (attempt, runner) = (cur.attempt, cur.runner);
                self.inner.send_to(&st, runner, AppMessage::Cancel { attempt });
                self.inner.finish_attempt(&mut st, id, AttemptEnd::Aborted);
            }
            None => self.inner.resolve(&mut st, id, Err(TaskError::Cancelled)),
        }
        self.inner.schedule(&mut st);
    }

    /// The last checkpoint of a live task (`None` when finished or never sent).
    pub fn last_checkpoint(&self, id: TaskId) -> Option<Value> {
        self.inner
            .lock()
            .tasks
            .get(&id)
            .and_then(|t| t.last_checkpoint.clone())
    }

    /// Tasks waiting for a runner.
    pub fn queued(&self) -> usize {
        self.inner.lock().queue.len()
    }

    /// Attempts running now.
    pub fn running(&self) -> usize {
        self.inner.lock().attempts.len()
    }

    /// Attempts running now on one runner.
    pub fn running_on(&self, id: RunnerId) -> usize {
        self.inner
            .lock()
            .runners
            .get(&id)
            .map(|r| r.running.len())
            .unwrap_or(0)
    }

    /// Watches the counts the local fallback supervisor needs.
    pub fn summary(&self) -> watch::Receiver<PoolSummary> {
        self.inner.summary.subscribe()
    }

    /// Waits until every queued store write has been applied (tests, shutdown).
    pub async fn flush_store(&self) {
        let (tx, rx) = oneshot::channel();
        if self.inner.store_tx.send(StoreOp::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }

    // ----- admin -----

    /// All runners, the local fallback included while it is connected.
    pub fn list(&self) -> Vec<RunnerView> {
        let st = self.inner.lock();
        let mut views: Vec<RunnerView> = st
            .runners
            .values()
            .filter(|r| !r.local || r.conn.is_some())
            .map(|r| self.inner.view(r))
            .collect();
        views.sort_by(|a, b| {
            (a.local, a.priority, &a.name).cmp(&(b.local, b.priority, &b.name))
        });
        views
    }

    /// One runner.
    pub fn get(&self, id: RunnerId) -> Option<RunnerView> {
        let st = self.inner.lock();
        st.runners.get(&id).map(|r| self.inner.view(r))
    }

    /// Registers a runner and returns its token (shown once; only its Argon2 hash is stored).
    pub async fn register(
        &self,
        name: &str,
        settings: Option<RunnerSettings>,
    ) -> Result<RegisterRunnerResponse, Error> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Error::new_field_validation_error(
                "name",
                "Name must be 1 to 64 characters",
            ));
        }
        let settings = settings.unwrap_or_default();
        if settings.weight == 0 {
            return Err(Error::new_field_validation_error(
                "weight",
                "Weight must be at least 1",
            ));
        }
        let id = RunnerId::new();
        let (token, token_hash) = tokio::task::spawn_blocking(move || issue_runner_token(id))
            .await
            .map_err(|e| Error::new_something_went_wrong(e.to_string()))??;
        let record = RunnerRecord {
            id,
            name: name.to_string(),
            token_hash,
            settings,
            enabled: true,
            created_at: Utc::now(),
            status: RunnerStatus::default(),
        };
        self.inner.store.save_runner(&record).await?;
        let mut st = self.inner.lock();
        let entry = RunnerEntry {
            record,
            local: false,
            drained: false,
            conn: None,
            running: HashSet::new(),
            status_written: None,
        };
        let runner = self.inner.view(&entry);
        st.runners.insert(id, entry);
        Ok(RegisterRunnerResponse { runner, token })
    }

    /// Re-reads the registered runners from the store and applies what changed there: new
    /// runners are added (offline until they connect), deleted ones are removed (their
    /// connection closes, their attempts are reassigned), and name, token hash, settings and
    /// the enabled flag of the others are updated (a changed capacity is pushed to a connected
    /// runner). For apps whose admin writes the `runners` table from another process instead
    /// of calling the pool. Nothing is written back to the store.
    pub async fn reload_runners(&self) -> Result<(), Error> {
        let records = self.inner.store.load_runners().await?;
        let mut st = self.inner.lock();
        let loaded: HashSet<RunnerId> = records.iter().map(|r| r.id).collect();
        let removed: Vec<RunnerId> = st
            .runners
            .iter()
            .filter(|(id, r)| !r.local && !loaded.contains(*id))
            .map(|(id, _)| *id)
            .collect();
        for id in removed {
            if let Some(entry) = st.runners.remove(&id) {
                tracing::info!(runner = %entry.record.name, "runner removed from the store");
                for attempt in entry.running {
                    self.inner.lose_attempt(&mut st, attempt, "runner revoked");
                }
            }
        }
        for record in records {
            if record.id == RunnerId::LOCAL {
                continue;
            }
            match st.runners.get_mut(&record.id) {
                Some(entry) => {
                    let capacity_changed =
                        entry.record.settings.capacity != record.settings.capacity;
                    entry.record.name = record.name;
                    entry.record.token_hash = record.token_hash;
                    entry.record.settings = record.settings;
                    entry.record.enabled = record.enabled;
                    if capacity_changed
                        && let Some(conn) = entry.conn.as_ref().filter(|c| c.hello.is_some())
                    {
                        let _ = conn
                            .tx
                            .send(AppMessage::Settings(self.inner.pushed(&entry.record.settings)));
                    }
                }
                None => {
                    let mut record = record;
                    record.status.state = RunnerState::Offline;
                    st.runners.insert(
                        record.id,
                        RunnerEntry {
                            record,
                            local: false,
                            drained: false,
                            conn: None,
                            running: HashSet::new(),
                            status_written: None,
                        },
                    );
                }
            }
        }
        self.inner.schedule(&mut st);
        self.inner.publish_summary(&st);
        Ok(())
    }

    /// Updates name, capacity, priority or weight; pushes the new capacity to the runner and
    /// reschedules (a raised capacity takes queued work at once).
    pub async fn update_settings(
        &self,
        id: RunnerId,
        patch: RunnerSettingsPatch,
    ) -> Result<RunnerView, Error> {
        let mut record = self.editable_record(id)?;
        if let Some(name) = patch.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 64 {
                return Err(Error::new_field_validation_error(
                    "name",
                    "Name must be 1 to 64 characters",
                ));
            }
            record.name = name;
        }
        if let Some(capacity) = patch.capacity {
            record.settings.capacity = capacity;
        }
        if let Some(priority) = patch.priority {
            record.settings.priority = priority;
        }
        if let Some(weight) = patch.weight {
            if weight == 0 {
                return Err(Error::new_field_validation_error(
                    "weight",
                    "Weight must be at least 1",
                ));
            }
            record.settings.weight = weight;
        }
        self.inner.store.save_runner(&record).await?;
        let mut st = self.inner.lock();
        let pushed = self.inner.pushed(&record.settings);
        let entry = st
            .runners
            .get_mut(&id)
            .ok_or_else(|| Error::new_not_found("Runner not found"))?;
        entry.record.name = record.name;
        entry.record.settings = record.settings;
        if let Some(conn) = entry.conn.as_ref().filter(|c| c.hello.is_some()) {
            let _ = conn.tx.send(AppMessage::Settings(pushed));
        }
        let view = self.inner.view(entry);
        self.inner.schedule(&mut st);
        Ok(view)
    }

    /// Enables or disables a runner (a disabled runner stays connected but gets no work).
    pub async fn set_enabled(&self, id: RunnerId, enabled: bool) -> Result<RunnerView, Error> {
        let mut record = self.editable_record(id)?;
        record.enabled = enabled;
        self.inner.store.save_runner(&record).await?;
        let mut st = self.inner.lock();
        let entry = st
            .runners
            .get_mut(&id)
            .ok_or_else(|| Error::new_not_found("Runner not found"))?;
        entry.record.enabled = enabled;
        let view = self.inner.view(entry);
        self.inner.schedule(&mut st);
        self.inner.publish_summary(&st);
        Ok(view)
    }

    /// Revokes a runner: deletes it, closes its connection and reassigns its attempts.
    pub async fn revoke(&self, id: RunnerId) -> Result<(), Error> {
        self.editable_record(id)?;
        self.inner.store.delete_runner(id).await?;
        let mut st = self.inner.lock();
        if let Some(entry) = st.runners.remove(&id) {
            for attempt in entry.running {
                self.inner.lose_attempt(&mut st, attempt, "runner revoked");
            }
        }
        self.inner.schedule(&mut st);
        self.inner.publish_summary(&st);
        Ok(())
    }

    /// Drains a runner: it gets no new work (running attempts finish) and stops its managed
    /// services once idle, until [`RunnerPool::resume`].
    pub fn drain(&self, id: RunnerId) -> Result<RunnerView, Error> {
        let mut st = self.inner.lock();
        let entry = st
            .runners
            .get_mut(&id)
            .ok_or_else(|| Error::new_not_found("Runner not found"))?;
        entry.drained = true;
        if let Some(conn) = entry.conn.as_mut().filter(|c| c.hello.is_some()) {
            if conn.state == RunnerState::Online {
                conn.state = RunnerState::Draining;
            }
            let _ = conn.tx.send(AppMessage::Drain);
        }
        self.inner.write_status(entry, true);
        let view = self.inner.view(entry);
        self.inner.publish_summary(&st);
        Ok(view)
    }

    /// Undoes [`RunnerPool::drain`].
    pub fn resume(&self, id: RunnerId) -> Result<RunnerView, Error> {
        let mut st = self.inner.lock();
        let entry = st
            .runners
            .get_mut(&id)
            .ok_or_else(|| Error::new_not_found("Runner not found"))?;
        entry.drained = false;
        if let Some(conn) = entry
            .conn
            .as_mut()
            .filter(|c| c.hello.is_some() && !c.leaving)
        {
            if conn.state == RunnerState::Draining {
                conn.state = RunnerState::Online;
            }
            let _ = conn.tx.send(AppMessage::Resume);
        }
        self.inner.write_status(entry, true);
        let view = self.inner.view(entry);
        self.inner.schedule(&mut st);
        self.inner.publish_summary(&st);
        Ok(view)
    }

    /// A task with its attempt history (from the store).
    pub async fn task_history(&self, id: TaskId) -> Result<Option<TaskHistoryView>, Error> {
        Ok(self
            .inner
            .store
            .task_history(id)
            .await?
            .map(|(task, attempts)| TaskHistoryView::new(task, attempts)))
    }

    fn editable_record(&self, id: RunnerId) -> Result<RunnerRecord, Error> {
        if id == RunnerId::LOCAL {
            return Err(Error::bad_request_error(
                "The local runner is managed by the local fallback setting",
            ));
        }
        self.inner
            .lock()
            .runners
            .get(&id)
            .map(|r| r.record.clone())
            .ok_or_else(|| Error::new_not_found("Runner not found"))
    }

    // ----- local fallback -----

    /// Token of the local fallback runner (ephemeral, valid for this pool only).
    pub fn local_token(&self) -> String {
        format!("{}.{}", RunnerId::LOCAL, self.inner.local_secret)
    }

    /// Sends `Exit` to the local fallback runner. `false` when it is not connected.
    pub fn exit_local(&self, grace: Duration) -> bool {
        let mut st = self.inner.lock();
        let Some(entry) = st.runners.get_mut(&RunnerId::LOCAL) else {
            return false;
        };
        let Some(conn) = entry.conn.as_mut().filter(|c| c.hello.is_some()) else {
            return false;
        };
        conn.leaving = true;
        conn.state = RunnerState::Draining;
        let sent = conn
            .tx
            .send(AppMessage::Exit {
                grace_ms: grace.as_millis() as u64,
            })
            .is_ok();
        self.inner.publish_summary(&st);
        sent
    }

    // ----- connections -----

    /// Authenticates a runner token (`<runner id>.<secret>`).
    pub async fn authenticate(&self, token: &str) -> Result<RunnerId, Error> {
        let unauthorized = || Error::new_unauthorized("invalid runner token");
        let (id, secret) = token.split_once('.').ok_or_else(unauthorized)?;
        let id: RunnerId = id.parse().map_err(|_| unauthorized())?;
        if id == RunnerId::LOCAL {
            return if constant_time_eq(secret.as_bytes(), self.inner.local_secret.as_bytes()) {
                Ok(id)
            } else {
                Err(unauthorized())
            };
        }
        let hash = self
            .inner
            .lock()
            .runners
            .get(&id)
            .map(|r| r.record.token_hash.clone())
            .ok_or_else(unauthorized)?;
        let secret = secret.to_string();
        let ok = tokio::task::spawn_blocking(move || verify_password(secret, hash))
            .await
            .map_err(|e| Error::new_something_went_wrong(e.to_string()))??;
        if ok { Ok(id) } else { Err(unauthorized()) }
    }

    /// Authenticates `token` and opens a session for one connection. The WebSocket route uses
    /// it; tests use it to act as a runner by hand. A newer session replaces an older one.
    pub async fn attach(&self, token: &str) -> Result<RunnerSession, Error> {
        let runner_id = self.authenticate(token).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let conn_id = self.inner.next_conn.fetch_add(1, Ordering::Relaxed);
        let mut st = self.inner.lock();
        let entry = st
            .runners
            .get_mut(&runner_id)
            .ok_or_else(|| Error::new_unauthorized("runner revoked"))?;
        if entry.conn.is_some() {
            tracing::info!(runner = %entry.record.name, "new connection replaces the old one");
        }
        entry.conn = Some(Conn {
            id: conn_id,
            tx,
            hello: None,
            state: RunnerState::Offline,
            leaving: false,
        });
        Ok(RunnerSession {
            pool: self.clone(),
            runner_id,
            conn_id,
            rx,
        })
    }

    pub(crate) fn file_access(
        &self,
        attempt: AttemptId,
        slot: &str,
        dir: &str,
        exp: u64,
        sig: &str,
    ) -> Option<FileAccess> {
        if exp < unix_now() {
            return None;
        }
        signatory::verify(
            &self.inner.public_key,
            &signed_data(attempt, slot, dir, exp),
            sig,
        )
        .ok()?;
        let st = self.inner.lock();
        let task = st.tasks.get(st.attempts.get(&attempt)?)?;
        let cur = task.current.as_ref()?;
        if cur.attempt != attempt {
            return None;
        }
        match dir {
            "in" => task
                .inputs
                .iter()
                .find(|i| i.slot == slot)
                .map(|i| FileAccess::Download(i.path.clone())),
            "out" if !cur.finalizing => task
                .outputs
                .iter()
                .find(|o| o.slot == slot)
                .map(|o| FileAccess::Upload(part_path(&o.path, attempt))),
            _ => None,
        }
    }

    fn on_message(&self, runner_id: RunnerId, conn_id: u64, message: RunnerMessage) {
        match message {
            RunnerMessage::Hello(hello) => self.inner.on_hello(runner_id, conn_id, hello),
            RunnerMessage::Heartbeat { running } => {
                self.inner.on_heartbeat(runner_id, conn_id, running)
            }
            RunnerMessage::Progress {
                attempt,
                percent,
                checkpoint,
            } => self
                .inner
                .on_progress(runner_id, conn_id, attempt, percent, checkpoint),
            RunnerMessage::Result {
                attempt,
                outcome,
                elapsed_ms,
            } => self.on_result(runner_id, conn_id, attempt, outcome, elapsed_ms),
            RunnerMessage::Draining => self.inner.on_draining(runner_id, conn_id),
            RunnerMessage::Log {
                level,
                message,
                attempt,
            } => {
                let name = self.inner.runner_name(runner_id);
                let attempt = attempt.map(|a| a.to_string()).unwrap_or_default();
                match level {
                    LogLevel::Info => tracing::info!(runner = %name, %attempt, "{message}"),
                    LogLevel::Warn => tracing::warn!(runner = %name, %attempt, "{message}"),
                    LogLevel::Error => tracing::error!(runner = %name, %attempt, "{message}"),
                }
            }
        }
    }

    fn on_result(
        &self,
        runner_id: RunnerId,
        conn_id: u64,
        attempt: AttemptId,
        outcome: Outcome,
        elapsed_ms: u64,
    ) {
        let mut st = self.inner.lock();
        if !self.inner.is_current_conn(&st, runner_id, conn_id) {
            return;
        }
        self.inner.touch(&mut st, runner_id);
        let Some(task_id) = self.inner.current_task(&st, runner_id, attempt) else {
            tracing::warn!(
                %attempt, runner = %self.inner.name_in(&st, runner_id), outcome = %outcome.describe(),
                "late result for an attempt that is no longer current; discarded"
            );
            return;
        };
        tracing::info!(%attempt, elapsed_ms, outcome = %outcome.describe(), "attempt result");
        let end = match outcome {
            Outcome::Ok { output, files } => {
                let Some(task) = st.tasks.get_mut(&task_id) else {
                    return;
                };
                let Some(cur) = task.current.as_mut() else {
                    return;
                };
                cur.finalizing = true;
                let targets = task.outputs.clone();
                drop(st);
                let pool = self.clone();
                tokio::spawn(async move {
                    pool.finalize(task_id, attempt, output, files, targets).await;
                });
                return;
            }
            Outcome::Failed { reason } => AttemptEnd::Failed(reason),
            other => AttemptEnd::Retry(AttemptStatus::Crashed, other.describe()),
        };
        self.inner.finish_attempt(&mut st, task_id, end);
        self.inner.schedule(&mut st);
    }

    async fn finalize(
        &self,
        task_id: TaskId,
        attempt: AttemptId,
        output: Value,
        uploaded: Vec<UploadedFile>,
        targets: Vec<OutputTarget>,
    ) {
        let mut files = Vec::new();
        let mut error = None;
        for target in &targets {
            let part = part_path(&target.path, attempt);
            let Some(up) = uploaded.iter().find(|f| f.slot == target.slot) else {
                if !target.optional {
                    error = Some(format!("output slot {} was not uploaded", target.slot));
                    break;
                }
                continue;
            };
            match sha256_file(part.clone()).await {
                Ok((sha256, size)) if sha256 == up.sha256 && size == up.size => {
                    if let Err(e) = tokio::fs::rename(&part, &target.path).await {
                        error = Some(format!("cannot move output {}: {e}", target.slot));
                        break;
                    }
                    files.push(OutputFile {
                        slot: target.slot.clone(),
                        path: target.path.clone(),
                        sha256,
                        size,
                    });
                }
                Ok((sha256, size)) => {
                    error = Some(format!(
                        "output {} does not match: received {size} bytes ({sha256}), runner sent {} bytes ({})",
                        target.slot, up.size, up.sha256
                    ));
                    break;
                }
                Err(e) => {
                    error = Some(format!("output {} missing: {e}", target.slot));
                    break;
                }
            }
        }
        let mut st = self.inner.lock();
        let current = st
            .tasks
            .get(&task_id)
            .and_then(|t| t.current.as_ref())
            .is_some_and(|c| c.attempt == attempt);
        if !current {
            tracing::warn!(%attempt, "task ended while its outputs were verified; result discarded");
            return;
        }
        let end = match error {
            Some(e) => AttemptEnd::Retry(AttemptStatus::Crashed, format!("transfer failed: {e}")),
            None => AttemptEnd::Done(output, files),
        };
        self.inner.finish_attempt(&mut st, task_id, end);
        self.inner.schedule(&mut st);
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn store_op(&self, op: StoreOp) {
        let _ = self.store_tx.send(op);
    }

    fn pushed(&self, settings: &RunnerSettings) -> PushedSettings {
        PushedSettings {
            capacity: settings.capacity,
            heartbeat_ms: self.config.heartbeat.as_millis() as u64,
        }
    }

    fn base_url(&self, local: bool) -> &str {
        match (&self.config.local_url, local) {
            (Some(url), true) => url,
            _ => &self.config.public_url,
        }
    }

    fn sign_url(
        &self,
        base: &str,
        attempt: AttemptId,
        slot: &str,
        dir: &str,
        exp: u64,
    ) -> Result<String, Error> {
        let sig = signatory::sign(&self.secret_key, &signed_data(attempt, slot, dir, exp))?;
        Ok(format!(
            "{base}/runners/files/{attempt}/{slot}?dir={dir}&exp={exp}&sig={sig}"
        ))
    }

    fn view(&self, r: &RunnerEntry) -> RunnerView {
        let status = &r.record.status;
        RunnerView {
            id: r.record.id,
            name: r.record.name.clone(),
            local: r.local,
            capacity: r.record.settings.capacity,
            priority: r.record.settings.priority,
            weight: r.record.settings.weight,
            enabled: r.record.enabled,
            drained: r.drained,
            state: r.state(),
            running: r.running.len() as u32,
            last_seen_at: status.last_seen_at,
            protocol_version: status.protocol_version,
            contract_hash: status.contract_hash.clone(),
            contract_matches: status.contract_hash.as_deref()
                == Some(self.config.contract_hash.as_str())
                && status.protocol_version == Some(PROTOCOL_VERSION),
            tool_versions: status.tool_versions.clone(),
            created_at: (!r.local).then_some(r.record.created_at),
        }
    }

    fn runner_name(&self, id: RunnerId) -> String {
        let st = self.lock();
        self.name_in(&st, id)
    }

    fn name_in(&self, st: &State, id: RunnerId) -> String {
        st.runners
            .get(&id)
            .map(|r| r.record.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn send_to(&self, st: &State, runner: RunnerId, message: AppMessage) {
        if let Some(conn) = st.runners.get(&runner).and_then(|r| r.conn.as_ref()) {
            let _ = conn.tx.send(message);
        }
    }

    fn is_current_conn(&self, st: &State, runner_id: RunnerId, conn_id: u64) -> bool {
        st.runners
            .get(&runner_id)
            .and_then(|r| r.conn.as_ref())
            .is_some_and(|c| c.id == conn_id && c.hello.is_some())
    }

    /// The task whose current attempt is `attempt` on `runner_id`.
    fn current_task(&self, st: &State, runner_id: RunnerId, attempt: AttemptId) -> Option<TaskId> {
        let task_id = *st.attempts.get(&attempt)?;
        let cur = st.tasks.get(&task_id)?.current.as_ref()?;
        (cur.attempt == attempt && cur.runner == runner_id).then_some(task_id)
    }

    /// Records that a message arrived (status written at most every 30 s).
    fn touch(&self, st: &mut State, runner_id: RunnerId) {
        if let Some(entry) = st.runners.get_mut(&runner_id) {
            entry.record.status.last_seen_at = Some(Utc::now());
            self.write_status(entry, false);
        }
    }

    fn write_status(&self, entry: &mut RunnerEntry, force: bool) {
        entry.record.status.state = entry.state();
        if entry.local {
            return;
        }
        let due = entry
            .status_written
            .is_none_or(|t| t.elapsed() >= STATUS_WRITE_EVERY);
        if force || due {
            entry.status_written = Some(Instant::now());
            self.store_op(StoreOp::Status(entry.record.id, entry.record.status.clone()));
        }
    }

    fn publish_summary(&self, st: &State) {
        let summary = PoolSummary {
            remote_online: st
                .runners
                .values()
                .filter(|r| !r.local && r.record.enabled && !r.drained && r.online())
                .count(),
            local_connected: st
                .runners
                .get(&RunnerId::LOCAL)
                .and_then(|r| r.conn.as_ref())
                .is_some_and(|c| c.hello.is_some()),
        };
        self.summary.send_if_modified(|current| {
            let changed = *current != summary;
            *current = summary;
            changed
        });
    }

    fn on_hello(&self, runner_id: RunnerId, conn_id: u64, hello: Hello) {
        let mut st = self.lock();
        let lease = self.config.lease;
        let Some(entry) = st.runners.get_mut(&runner_id) else {
            return;
        };
        let pushed = self.pushed(&entry.record.settings);
        let drained = entry.drained;
        let local = entry.local;
        let Some(conn) = entry.conn.as_mut().filter(|c| c.id == conn_id) else {
            return;
        };
        if conn.hello.is_some() {
            tracing::warn!("second hello on one connection ignored");
            return;
        }
        let outdated = hello.protocol_version != PROTOCOL_VERSION
            || hello.contract_hash != self.config.contract_hash;
        conn.state = if outdated {
            RunnerState::Outdated
        } else if drained {
            RunnerState::Draining
        } else {
            RunnerState::Online
        };
        let tx = conn.tx.clone();
        let in_flight: HashSet<AttemptId> = hello.in_flight.iter().copied().collect();
        entry.record.status = RunnerStatus {
            state: conn.state,
            last_seen_at: Some(Utc::now()),
            protocol_version: Some(hello.protocol_version),
            contract_hash: Some(hello.contract_hash.clone()),
            tool_versions: hello.tool_versions.clone(),
        };
        conn.hello = Some(hello);
        self.write_status(entry, true);
        tracing::info!(runner = %entry.record.name, state = entry.state().as_str(), "runner connected");

        let abort: Vec<AttemptId> = in_flight
            .iter()
            .filter(|a| !entry.running.contains(a))
            .copied()
            .collect();
        let missing: Vec<AttemptId> = entry
            .running
            .iter()
            .filter(|a| !in_flight.contains(a))
            .copied()
            .collect();
        let kept: Vec<AttemptId> = entry
            .running
            .iter()
            .filter(|a| in_flight.contains(a))
            .copied()
            .collect();

        if outdated {
            match &self.binary {
                Some(binary) => {
                    let _ = tx.send(AppMessage::Outdated {
                        binary_url: format!("{}{BINARY_PATH}", self.base_url(local)),
                        sha256: binary.sha256.clone(),
                        contract_hash: self.config.contract_hash.clone(),
                    });
                }
                None => tracing::warn!(
                    runner = %entry.record.name,
                    "runner is outdated and no binary is configured to update it"
                ),
            }
        }
        let _ = tx.send(AppMessage::Welcome {
            runner_id,
            settings: pushed,
            abort,
        });
        if drained && !outdated {
            let _ = tx.send(AppMessage::Drain);
        }
        let now = Instant::now();
        for attempt in kept {
            if let Some(cur) = st
                .attempts
                .get(&attempt)
                .copied()
                .and_then(|t| st.tasks.get_mut(&t))
                .and_then(|t| t.current.as_mut())
            {
                cur.lease_until = now + lease;
            }
        }
        for attempt in missing {
            self.lose_attempt(&mut st, attempt, "the runner reconnected without it");
        }
        self.schedule(&mut st);
        self.publish_summary(&st);
    }

    fn on_heartbeat(&self, runner_id: RunnerId, conn_id: u64, running: Vec<AttemptId>) {
        let mut st = self.lock();
        if !self.is_current_conn(&st, runner_id, conn_id) {
            return;
        }
        self.touch(&mut st, runner_id);
        let lease_until = Instant::now() + self.config.lease;
        for attempt in running {
            match self.current_task(&st, runner_id, attempt) {
                Some(task_id) => {
                    if let Some(cur) = st.tasks.get_mut(&task_id).and_then(|t| t.current.as_mut())
                    {
                        cur.lease_until = lease_until;
                    }
                }
                None => self.send_to(&st, runner_id, AppMessage::Cancel { attempt }),
            }
        }
    }

    fn on_progress(
        &self,
        runner_id: RunnerId,
        conn_id: u64,
        attempt: AttemptId,
        percent: f32,
        checkpoint: Option<Value>,
    ) {
        let mut st = self.lock();
        if !self.is_current_conn(&st, runner_id, conn_id) {
            return;
        }
        self.touch(&mut st, runner_id);
        let Some(task_id) = self.current_task(&st, runner_id, attempt) else {
            tracing::warn!(%attempt, "progress for an attempt that is no longer current");
            self.send_to(&st, runner_id, AppMessage::Cancel { attempt });
            return;
        };
        let lease = self.config.lease;
        let Some(task) = st.tasks.get_mut(&task_id) else {
            return;
        };
        let Some(cur) = task.current.as_mut() else {
            return;
        };
        cur.lease_until = Instant::now() + lease;
        cur.percent = percent.clamp(0.0, 100.0);
        if let Some(cp) = &checkpoint {
            cur.checkpoint = Some(cp.clone());
            task.last_checkpoint = Some(cp.clone());
        }
        let percent = cur.percent;
        task.progress_tx.send_modify(|p| {
            p.percent = percent;
            if let Some(cp) = &checkpoint {
                p.checkpoint = Some(cp.clone());
            }
        });
        if checkpoint.is_some()
            && let Some(cur) = task.current.as_ref()
        {
            self.store_op(StoreOp::Attempt(task.attempt_record(cur, lease)));
        }
    }

    fn on_draining(&self, runner_id: RunnerId, conn_id: u64) {
        let mut st = self.lock();
        if !self.is_current_conn(&st, runner_id, conn_id) {
            return;
        }
        if let Some(entry) = st.runners.get_mut(&runner_id) {
            if let Some(conn) = entry.conn.as_mut() {
                conn.leaving = true;
                conn.state = RunnerState::Draining;
            }
            tracing::info!(runner = %entry.record.name, "runner is shutting down");
            entry.record.status.last_seen_at = Some(Utc::now());
            self.write_status(entry, true);
        }
        self.publish_summary(&st);
    }

    fn on_disconnect(&self, runner_id: RunnerId, conn_id: u64) {
        let mut st = self.lock();
        let Some(entry) = st.runners.get_mut(&runner_id) else {
            return;
        };
        if entry.conn.as_ref().is_none_or(|c| c.id != conn_id) {
            return;
        }
        let leaving = entry.conn.as_ref().is_some_and(|c| c.leaving);
        entry.conn = None;
        entry.record.status.last_seen_at = Some(Utc::now());
        self.write_status(entry, true);
        tracing::info!(runner = %entry.record.name, "runner disconnected");
        if leaving {
            let running: Vec<AttemptId> = entry.running.iter().copied().collect();
            for attempt in running {
                self.lose_attempt(&mut st, attempt, "the runner left");
            }
        }
        self.schedule(&mut st);
        self.publish_summary(&st);
    }

    fn sweep(&self) {
        let mut st = self.lock();
        let now = Instant::now();
        let expired: Vec<AttemptId> = st
            .tasks
            .values()
            .filter_map(|t| t.current.as_ref())
            .filter(|c| !c.finalizing && c.lease_until <= now)
            .map(|c| c.attempt)
            .collect();
        for attempt in expired {
            self.lose_attempt(&mut st, attempt, "lease expired");
        }
        self.schedule(&mut st);
    }

    /// Ends an attempt as lost (the task gets another one) and tells its runner to drop it.
    fn lose_attempt(&self, st: &mut State, attempt: AttemptId, reason: &str) {
        let Some(&task_id) = st.attempts.get(&attempt) else {
            return;
        };
        if let Some(cur) = st.tasks.get(&task_id).and_then(|t| t.current.as_ref()) {
            tracing::warn!(%attempt, runner = %cur.runner_name, "attempt lost: {reason}");
            self.send_to(st, cur.runner, AppMessage::Cancel { attempt });
        }
        self.finish_attempt(
            st,
            task_id,
            AttemptEnd::Retry(AttemptStatus::Lost, reason.to_string()),
        );
    }

    fn finish_attempt(&self, st: &mut State, task_id: TaskId, end: AttemptEnd) {
        let lease = self.config.lease;
        let Some(task) = st.tasks.get_mut(&task_id) else {
            return;
        };
        let Some(cur) = task.current.take() else {
            return;
        };
        st.attempts.remove(&cur.attempt);
        if let Some(runner) = st.runners.get_mut(&cur.runner) {
            runner.running.remove(&cur.attempt);
        }
        for target in &task.outputs {
            let part = part_path(&target.path, cur.attempt);
            if let Err(e) = std::fs::remove_file(&part)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("cannot remove {}: {e}", part.display());
            }
        }
        let (status, error) = match &end {
            AttemptEnd::Done(..) => (AttemptStatus::Done, None),
            AttemptEnd::Failed(reason) => (AttemptStatus::Failed, Some(reason.clone())),
            AttemptEnd::Retry(status, reason) => (*status, Some(reason.clone())),
            AttemptEnd::Aborted => (AttemptStatus::Aborted, Some("cancelled".to_string())),
        };
        let mut record = task.attempt_record(&cur, lease);
        record.status = status;
        record.ended_at = Some(Utc::now());
        record.error = error;
        self.store_op(StoreOp::Attempt(record));

        let attempts = task.attempts_made;
        match end {
            AttemptEnd::Done(output, files) => self.resolve(
                st,
                task_id,
                Ok(TaskOutput {
                    output,
                    files,
                    attempts,
                }),
            ),
            AttemptEnd::Failed(reason) => self.resolve(st, task_id, Err(TaskError::Failed(reason))),
            AttemptEnd::Aborted => self.resolve(st, task_id, Err(TaskError::Cancelled)),
            AttemptEnd::Retry(_, reason) => {
                if attempts >= task.max_attempts {
                    self.resolve(
                        st,
                        task_id,
                        Err(TaskError::AttemptsExhausted {
                            attempts,
                            last_error: reason,
                        }),
                    );
                } else {
                    task.last_error = Some(reason);
                    task.progress_tx.send_modify(|p| {
                        p.status = TaskStatus::Queued;
                        p.runner = None;
                        p.percent = 0.0;
                    });
                    self.store_op(StoreOp::Task(task.record(TaskStatus::Queued)));
                    st.queue.push_front(task_id);
                }
            }
        }
    }

    /// Ends a task: removes it, writes its row and wakes the handle.
    fn resolve(&self, st: &mut State, task_id: TaskId, result: Result<TaskOutput, TaskError>) {
        let Some(mut task) = st.tasks.remove(&task_id) else {
            return;
        };
        st.queue.retain(|id| *id != task_id);
        if let Some(cur) = task.current.take() {
            st.attempts.remove(&cur.attempt);
            if let Some(runner) = st.runners.get_mut(&cur.runner) {
                runner.running.remove(&cur.attempt);
            }
        }
        let (status, output, failure) = match &result {
            Ok(out) => (TaskStatus::Done, Some(out.output.clone()), None),
            Err(TaskError::Cancelled) => (TaskStatus::Cancelled, None, Some("cancelled".into())),
            Err(e) => (TaskStatus::Failed, None, Some(e.to_string())),
        };
        let mut record = task.record(status);
        record.finished_at = Some(Utc::now());
        record.output = output;
        record.failure = failure;
        self.store_op(StoreOp::Task(record));
        task.progress_tx.send_modify(|p| {
            p.status = status;
            if status == TaskStatus::Done {
                p.percent = 100.0;
            }
        });
        if let Some(tx) = task.result_tx.take() {
            let _ = tx.send(result);
        }
    }

    /// Fills free capacity from the queue (decision 5): the head of the FIFO goes to the
    /// lowest priority tier with room, and within it to the lowest `running / weight`.
    fn schedule(&self, st: &mut State) {
        while let Some(&task_id) = st.queue.front() {
            if !st.tasks.contains_key(&task_id) {
                st.queue.pop_front();
                continue;
            }
            let Some(runner_id) = Self::pick_runner(st) else {
                break;
            };
            st.queue.pop_front();
            self.dispatch(st, task_id, runner_id);
        }
    }

    fn pick_runner(st: &State) -> Option<RunnerId> {
        st.runners
            .values()
            .filter(|r| r.accepts_work())
            .min_by(|a, b| {
                a.record
                    .settings
                    .priority
                    .cmp(&b.record.settings.priority)
                    .then_with(|| a.ratio().total_cmp(&b.ratio()))
                    .then_with(|| a.record.name.cmp(&b.record.name))
                    .then_with(|| a.record.id.cmp(&b.record.id))
            })
            .map(|r| r.record.id)
    }

    fn dispatch(&self, st: &mut State, task_id: TaskId, runner_id: RunnerId) {
        let lease = self.config.lease;
        let Some(runner) = st.runners.get_mut(&runner_id) else {
            return;
        };
        let Some(conn) = runner.conn.as_ref() else {
            return;
        };
        let Some(task) = st.tasks.get_mut(&task_id) else {
            return;
        };
        task.attempts_made += 1;
        let number = task.attempts_made;
        if number > 1
            && let (Some(hook), Some(checkpoint)) = (&task.retry_hook, &task.last_checkpoint)
        {
            hook(&mut task.input, checkpoint);
        }
        let attempt = AttemptId::new();
        let base = self.base_url(runner.local).to_string();
        let exp = unix_now() + (task.timeout + lease).as_secs() + 600;
        let signed: Result<(Vec<InputFile>, Vec<OutputSlot>), Error> = (|| {
            let mut inputs = Vec::with_capacity(task.inputs.len());
            for i in &task.inputs {
                inputs.push(InputFile {
                    slot: i.slot.clone(),
                    url: self.sign_url(&base, attempt, &i.slot, "in", exp)?,
                    sha256: i.sha256.clone(),
                    size: i.size,
                });
            }
            let mut outputs = Vec::with_capacity(task.outputs.len());
            for o in &task.outputs {
                outputs.push(OutputSlot {
                    slot: o.slot.clone(),
                    url: self.sign_url(&base, attempt, &o.slot, "out", exp)?,
                    optional: o.optional,
                });
            }
            Ok((inputs, outputs))
        })();
        let (inputs, outputs) = match signed {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("cannot sign file URLs: {e:?}");
                self.resolve(
                    st,
                    task_id,
                    Err(TaskError::Failed("cannot sign file URLs".into())),
                );
                return;
            }
        };
        let _ = conn.tx.send(AppMessage::Assign(Assign {
            attempt,
            task_id,
            number,
            task_type: task.task_type.clone(),
            input: task.input.clone(),
            inputs,
            outputs,
            timeout_ms: task.timeout.as_millis() as u64,
        }));
        runner.running.insert(attempt);
        st.attempts.insert(attempt, task_id);
        let cur = Current {
            attempt,
            number,
            runner: runner_id,
            runner_name: runner.record.name.clone(),
            lease_until: Instant::now() + lease,
            started_at: Utc::now(),
            percent: 0.0,
            checkpoint: None,
            finalizing: false,
        };
        tracing::info!(%attempt, task = %task_id, task_type = %task.task_type, number, runner = %cur.runner_name, "attempt assigned");
        self.store_op(StoreOp::Attempt(task.attempt_record(&cur, lease)));
        self.store_op(StoreOp::Task(task.record(TaskStatus::Running)));
        let runner_name = cur.runner_name.clone();
        task.progress_tx.send_modify(|p| {
            p.status = TaskStatus::Running;
            p.attempt = number;
            p.percent = 0.0;
            p.runner = Some(runner_name);
        });
        task.current = Some(cur);
    }
}

async fn store_writer(store: Arc<dyn RunnerStore>, mut rx: mpsc::UnboundedReceiver<StoreOp>) {
    while let Some(op) = rx.recv().await {
        let result = match op {
            StoreOp::Status(id, status) => store.update_runner_status(id, &status).await,
            StoreOp::Task(task) => store.save_task(&task).await,
            StoreOp::Attempt(attempt) => store.save_attempt(&attempt).await,
            StoreOp::Flush(done) => {
                let _ = done.send(());
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::error!("runner store write failed: {e:?}");
        }
    }
}

async fn sweeper(inner: Weak<Inner>, interval: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        inner.sweep();
    }
}

/// One runner connection, between authentication and close. Feed it the runner's messages
/// with [`RunnerSession::handle`] and send what [`RunnerSession::recv`] yields; dropping it is
/// the disconnect.
pub struct RunnerSession {
    pool: RunnerPool,
    runner_id: RunnerId,
    conn_id: u64,
    rx: mpsc::UnboundedReceiver<AppMessage>,
}

impl RunnerSession {
    /// The authenticated runner.
    pub fn runner_id(&self) -> RunnerId {
        self.runner_id
    }

    /// The next message for the runner; `None` when the pool closed this connection
    /// (replaced, revoked).
    pub async fn recv(&mut self) -> Option<AppMessage> {
        self.rx.recv().await
    }

    /// Handles one message from the runner.
    pub fn handle(&self, message: RunnerMessage) {
        self.pool.on_message(self.runner_id, self.conn_id, message);
    }
}

impl Drop for RunnerSession {
    fn drop(&mut self) {
        self.pool.inner.on_disconnect(self.runner_id, self.conn_id);
    }
}
