//! App-side pool tests with in-process runners (and hand-driven sessions where a test needs
//! to misbehave like a real runner would).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{Value, json};
use web_core_runners::{
    app::{
        AttemptStatus, InProcessRunner, InProcessSpawner, LocalFallbackConfig,
        LocalFallbackSupervisor, LocalRunnerMode, LocalRunnerStatus, MemoryStore, PoolConfig,
        RunnerPool, RunnerSession, RunnerSettings, RunnerSettingsPatch, RunnerState,
        SubmitOptions, TaskError, TaskFiles, TaskHandle, TaskStatus,
    },
    protocol::{AppMessage, Hello, Outcome, PROTOCOL_VERSION, RunnerId, RunnerMessage},
    runner::{
        InlineExecutor, ReadyCheck, Runner, RunnerConfig, RunnerDeps, SelfUpdater,
        ServiceManager, ServiceSpec, TaskRegistry, WsConnector,
    },
};

const HASH: &str = "contract-v1";

fn config() -> PoolConfig {
    PoolConfig::new(HASH, "http://127.0.0.1:9")
        .with_lease(Duration::from_millis(400))
        .with_heartbeat(Duration::from_millis(50))
        .with_sweep_interval(Duration::from_millis(25))
}

async fn pool_with(config: PoolConfig) -> (RunnerPool, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::new());
    let pool = RunnerPool::start(config, store.clone())
        .await
        .expect("pool starts");
    (pool, store)
}

async fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !cond() {
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for: {what}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn finish(handle: TaskHandle) -> Result<web_core_runners::app::TaskOutput, TaskError> {
    tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("task finishes in time")
}

async fn register(pool: &RunnerPool, name: &str, settings: RunnerSettings) -> (RunnerId, String) {
    let r = pool
        .register(name, Some(settings))
        .await
        .expect("register");
    (r.runner.id, r.token)
}

fn settings(capacity: u32, priority: i32, weight: u32) -> RunnerSettings {
    RunnerSettings {
        capacity,
        priority,
        weight,
    }
}

/// A gate the `block` task waits on.
#[derive(Clone, Default)]
struct Gate(Arc<AtomicBool>);

impl Gate {
    fn open(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Tasks: `echo` returns its input; `block` waits for the gate (or cancellation), recording
/// the `n` field of its input in start order.
fn registry(gate: &Gate, started: &Arc<Mutex<Vec<i64>>>) -> TaskRegistry {
    let gate = gate.clone();
    let started = started.clone();
    TaskRegistry::new(HASH)
        .raw("echo", |_ctx, input| Ok(input))
        .raw("block", move |ctx, input| {
            if let Ok(mut s) = started.lock() {
                s.push(input["n"].as_i64().unwrap_or(-1));
            }
            while !gate.0.load(Ordering::SeqCst) {
                if ctx.is_cancelled() {
                    return Err("cancelled".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(input)
        })
}

async fn submit(pool: &RunnerPool, task_type: &str, input: Value) -> TaskHandle {
    pool.submit(task_type, input, TaskFiles::new(), SubmitOptions::new())
        .await
        .expect("submit")
}

/// Drives a session by hand: says hello, returns the session.
async fn manual_runner(pool: &RunnerPool, token: &str, in_flight: Vec<web_core_runners::protocol::AttemptId>) -> RunnerSession {
    let session = pool.attach(token).await.expect("attach");
    session.handle(RunnerMessage::Hello(Hello {
        runner_name: "manual".into(),
        protocol_version: PROTOCOL_VERSION,
        contract_hash: HASH.into(),
        tool_versions: Default::default(),
        in_flight,
        local: false,
        runner_version: "test".into(),
    }));
    session
}

async fn next_message(session: &mut RunnerSession) -> AppMessage {
    tokio::time::timeout(Duration::from_secs(5), session.recv())
        .await
        .expect("message in time")
        .expect("session open")
}

#[tokio::test]
async fn echo_round_trip_and_history() {
    let (pool, store) = pool_with(config()).await;
    let (id, token) = register(&pool, "a", RunnerSettings::default()).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let _runner = InProcessRunner::start(&pool, token, registry(&gate, &started));
    eventually("runner online", || {
        pool.get(id).is_some_and(|r| r.state == RunnerState::Online)
    })
    .await;

    let handle = pool
        .submit(
            "echo",
            json!({"hello": "world"}),
            TaskFiles::new(),
            SubmitOptions::new().with_reference("job-1"),
        )
        .await
        .expect("submit");
    let task_id = handle.id();
    let out = finish(handle).await.expect("output");
    assert_eq!(out.output, json!({"hello": "world"}));
    assert_eq!(out.attempts, 1);

    pool.flush_store().await;
    let history = pool
        .task_history(task_id)
        .await
        .expect("history")
        .expect("task row");
    assert_eq!(history.status, TaskStatus::Done);
    assert_eq!(history.reference.as_deref(), Some("job-1"));
    assert_eq!(history.attempts.len(), 1);
    assert_eq!(history.attempts[0].status, AttemptStatus::Done);
    assert_eq!(history.attempts[0].runner_name, "a");
    let stored = store.runner(id).expect("runner row");
    assert_eq!(stored.status.contract_hash.as_deref(), Some(HASH));
}

#[tokio::test]
async fn priority_tiers_fill_first() {
    let (pool, _) = pool_with(config()).await;
    let (first, t1) = register(&pool, "first", settings(2, 0, 1)).await;
    let (second, t2) = register(&pool, "second", settings(2, 1, 1)).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let _a = InProcessRunner::start(&pool, t1, registry(&gate, &started));
    let _b = InProcessRunner::start(&pool, t2, registry(&gate, &started));
    eventually("both online", || {
        pool.list()
            .iter()
            .filter(|r| r.state == RunnerState::Online)
            .count()
            == 2
    })
    .await;

    let mut handles = Vec::new();
    for n in 0..2 {
        handles.push(submit(&pool, "block", json!({"n": n})).await);
    }
    assert_eq!(pool.running_on(first), 2);
    assert_eq!(pool.running_on(second), 0, "the lower tier is untouched");
    handles.push(submit(&pool, "block", json!({"n": 2})).await);
    assert_eq!(pool.running_on(second), 1, "the next tier takes the overflow");

    gate.open();
    for h in handles {
        finish(h).await.expect("output");
    }
}

#[tokio::test]
async fn weighted_spread_within_a_tier() {
    let (pool, _) = pool_with(config()).await;
    let (heavy, t1) = register(&pool, "heavy", settings(10, 0, 2)).await;
    let (light, t2) = register(&pool, "light", settings(10, 0, 1)).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let _a = InProcessRunner::start(&pool, t1, registry(&gate, &started));
    let _b = InProcessRunner::start(&pool, t2, registry(&gate, &started));
    eventually("both online", || {
        pool.list()
            .iter()
            .filter(|r| r.state == RunnerState::Online)
            .count()
            == 2
    })
    .await;

    let mut handles = Vec::new();
    for n in 0..6 {
        handles.push(submit(&pool, "block", json!({"n": n})).await);
    }
    assert_eq!(pool.running_on(heavy), 4);
    assert_eq!(pool.running_on(light), 2);
    gate.open();
    for h in handles {
        finish(h).await.expect("output");
    }
}

#[tokio::test]
async fn queue_when_full_drains_fifo() {
    let (pool, _) = pool_with(config()).await;
    let (id, token) = register(&pool, "solo", settings(1, 0, 1)).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let _runner = InProcessRunner::start(&pool, token, registry(&gate, &started));
    eventually("online", || {
        pool.get(id).is_some_and(|r| r.state == RunnerState::Online)
    })
    .await;

    let mut handles = Vec::new();
    for n in 0..4 {
        handles.push(submit(&pool, "block", json!({"n": n})).await);
    }
    assert_eq!(pool.running(), 1);
    assert_eq!(pool.queued(), 3);
    gate.open();
    for h in handles {
        finish(h).await.expect("output");
    }
    assert_eq!(*started.lock().expect("lock"), vec![0, 1, 2, 3]);
    assert_eq!(pool.queued(), 0);
}

#[tokio::test]
async fn lease_loss_reassigns_to_another_runner() {
    let (pool, store) = pool_with(config()).await;
    let (a_id, ta) = register(&pool, "a", settings(1, 0, 1)).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let a = InProcessRunner::start(&pool, ta, registry(&gate, &started));
    eventually("a online", || {
        pool.get(a_id).is_some_and(|r| r.state == RunnerState::Online)
    })
    .await;

    let handle = submit(&pool, "block", json!({"n": 1})).await;
    let task_id = handle.id();
    assert_eq!(pool.running_on(a_id), 1);

    // a falls off the network; its lease runs out
    a.set_online(false);
    eventually("attempt lost", || pool.running() == 0).await;

    let (b_id, tb) = register(&pool, "b", settings(1, 0, 1)).await;
    let _b = InProcessRunner::start(&pool, tb, registry(&gate, &started));
    eventually("b runs it", || pool.running_on(b_id) == 1).await;
    gate.open();
    let out = finish(handle).await.expect("output");
    assert_eq!(out.attempts, 2);

    pool.flush_store().await;
    let attempts: Vec<_> = store
        .attempts()
        .into_iter()
        .filter(|x| x.task_id == task_id)
        .collect();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].status, AttemptStatus::Lost);
    assert_eq!(attempts[0].runner_id, a_id);
    assert_eq!(attempts[1].status, AttemptStatus::Done);
    assert_eq!(attempts[1].runner_id, b_id);
}

#[tokio::test]
async fn late_result_is_discarded() {
    let (pool, _) = pool_with(config()).await;
    let (_, token) = register(&pool, "manual", settings(1, 0, 1)).await;
    let mut session = manual_runner(&pool, &token, vec![]).await;
    assert!(matches!(
        next_message(&mut session).await,
        AppMessage::Welcome { .. }
    ));

    let handle = submit(&pool, "echo", json!({})).await;
    let AppMessage::Assign(first) = next_message(&mut session).await else {
        panic!("expected an assignment");
    };
    // no heartbeat: the lease expires, the app cancels and assigns again
    assert_eq!(
        next_message(&mut session).await,
        AppMessage::Cancel {
            attempt: first.attempt
        }
    );
    let AppMessage::Assign(second) = next_message(&mut session).await else {
        panic!("expected a second assignment");
    };
    assert_eq!(second.number, 2);

    // the first attempt's result arrives late
    session.handle(RunnerMessage::Result {
        attempt: first.attempt,
        outcome: Outcome::Ok {
            output: json!("stale"),
            files: vec![],
        },
        elapsed_ms: 1,
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(pool.running(), 1, "the late result changed nothing");

    session.handle(RunnerMessage::Result {
        attempt: second.attempt,
        outcome: Outcome::Ok {
            output: json!("fresh"),
            files: vec![],
        },
        elapsed_ms: 1,
    });
    let out = finish(handle).await.expect("output");
    assert_eq!(out.output, json!("fresh"));
}

#[tokio::test]
async fn crash_gets_a_next_attempt() {
    let (pool, store) = pool_with(config()).await;
    let (_, token) = register(&pool, "a", settings(1, 0, 1)).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let registry = TaskRegistry::new(HASH).raw("flaky", move |_ctx, input| {
        if c.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("first attempt dies");
        }
        Ok(input)
    });
    let _runner = InProcessRunner::start(&pool, token, registry);

    let handle = submit(&pool, "flaky", json!({"x": 1})).await;
    let task_id = handle.id();
    let out = finish(handle).await.expect("second attempt succeeds");
    assert_eq!(out.output, json!({"x": 1}));
    assert_eq!(out.attempts, 2);
    pool.flush_store().await;
    let statuses: Vec<_> = store
        .attempts()
        .into_iter()
        .filter(|a| a.task_id == task_id)
        .map(|a| (a.status, a.error))
        .collect();
    assert_eq!(statuses[0].0, AttemptStatus::Crashed);
    assert!(
        statuses[0]
            .1
            .as_deref()
            .is_some_and(|e| e.contains("first attempt dies"))
    );
    assert_eq!(statuses[1].0, AttemptStatus::Done);
}

#[tokio::test]
async fn max_attempts_fails_the_task() {
    let (pool, _) = pool_with(config().with_max_attempts("doomed", 2)).await;
    let (_, token) = register(&pool, "a", settings(1, 0, 1)).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let registry = TaskRegistry::new(HASH)
        .raw("doomed", move |_ctx, _input| {
            c.fetch_add(1, Ordering::SeqCst);
            panic!("always")
        })
        .raw("refused", |_ctx, _input| Err("unsupported media".into()));
    let _runner = InProcessRunner::start(&pool, token, registry);

    let err = finish(submit(&pool, "doomed", json!({})).await)
        .await
        .expect_err("gives up");
    match err {
        TaskError::AttemptsExhausted {
            attempts,
            last_error,
        } => {
            assert_eq!(attempts, 2);
            assert!(last_error.contains("always"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // a task-reported failure is final: no retry
    let err = finish(submit(&pool, "refused", json!({})).await)
        .await
        .expect_err("fails");
    assert_eq!(err, TaskError::Failed("unsupported media".into()));
}

#[tokio::test]
async fn reconnect_handshake_keeps_or_aborts_in_flight() {
    // long enough for a reconnect (Argon2 token check) on a loaded machine
    let (pool, _) = pool_with(config().with_lease(Duration::from_secs(2))).await;
    let (id, token) = register(&pool, "manual", settings(1, 0, 1)).await;
    let mut session = manual_runner(&pool, &token, vec![]).await;
    next_message(&mut session).await; // welcome
    let handle = submit(&pool, "echo", json!({})).await;
    let AppMessage::Assign(first) = next_message(&mut session).await else {
        panic!("expected an assignment");
    };

    // quick reconnect within the lease: the attempt is kept
    drop(session);
    let mut session = manual_runner(&pool, &token, vec![first.attempt]).await;
    match next_message(&mut session).await {
        AppMessage::Welcome { abort, .. } => assert!(abort.is_empty()),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(pool.running_on(id), 1);

    // drop again and stay away past the lease: the attempt is lost
    drop(session);
    eventually("attempt lost", || pool.running() == 0).await;
    let mut session = manual_runner(&pool, &token, vec![first.attempt]).await;
    match next_message(&mut session).await {
        AppMessage::Welcome { abort, .. } => assert_eq!(abort, vec![first.attempt]),
        other => panic!("unexpected {other:?}"),
    }
    let AppMessage::Assign(second) = next_message(&mut session).await else {
        panic!("expected a new assignment");
    };
    assert_eq!(second.number, 2);
    session.handle(RunnerMessage::Result {
        attempt: second.attempt,
        outcome: Outcome::Ok {
            output: json!(2),
            files: vec![],
        },
        elapsed_ms: 1,
    });
    assert_eq!(finish(handle).await.expect("output").output, json!(2));
}

#[tokio::test]
async fn restarted_runner_resumes_nothing() {
    let (pool, store) = pool_with(config()).await;
    let (id, token) = register(&pool, "a", settings(1, 0, 1)).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let mut runner = InProcessRunner::start(&pool, token, registry(&gate, &started));
    let handle = submit(&pool, "block", json!({"n": 7})).await;
    let task_id = handle.id();
    eventually("running", || started.lock().is_ok_and(|s| s.len() == 1)).await;

    // kill -9 and boot again: hello lists nothing, the app re-dispatches at once
    runner.restart().await;
    eventually("second attempt started", || {
        started.lock().is_ok_and(|s| s.len() == 2)
    })
    .await;
    assert_eq!(pool.running_on(id), 1);
    gate.open();
    assert_eq!(finish(handle).await.expect("output").attempts, 2);
    pool.flush_store().await;
    let first = store
        .attempts()
        .into_iter()
        .find(|a| a.task_id == task_id && a.number == 1)
        .expect("first attempt");
    assert_eq!(first.status, AttemptStatus::Lost);
}

#[tokio::test]
async fn settings_change_pushes_capacity_and_fills_it() {
    let (pool, _) = pool_with(config()).await;
    let (id, token) = register(&pool, "manual", settings(1, 0, 1)).await;
    let mut session = manual_runner(&pool, &token, vec![]).await;
    next_message(&mut session).await; // welcome
    let mut handles = Vec::new();
    for n in 0..3 {
        handles.push(submit(&pool, "echo", json!(n)).await);
    }
    assert!(matches!(next_message(&mut session).await, AppMessage::Assign(_)));
    assert_eq!(pool.queued(), 2);

    let view = pool
        .update_settings(
            id,
            RunnerSettingsPatch {
                capacity: Some(3),
                ..Default::default()
            },
        )
        .await
        .expect("update");
    assert_eq!(view.capacity, 3);
    match next_message(&mut session).await {
        AppMessage::Settings(s) => assert_eq!(s.capacity, 3),
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(next_message(&mut session).await, AppMessage::Assign(_)));
    assert!(matches!(next_message(&mut session).await, AppMessage::Assign(_)));
    assert_eq!(pool.queued(), 0);
    assert_eq!(pool.running_on(id), 3);
}

#[derive(Default)]
struct RecordingUpdater(Mutex<Vec<(String, String)>>);

#[async_trait]
impl SelfUpdater for RecordingUpdater {
    async fn update(&self, url: &str, sha256: &str, _token: &str) -> Result<(), String> {
        if let Ok(mut calls) = self.0.lock() {
            calls.push((url.to_string(), sha256.to_string()));
        }
        Err("recorded".into())
    }
}

#[tokio::test]
async fn outdated_runner_gets_no_work_and_updates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = dir.path().join("runner");
    std::fs::write(&binary, b"new runner binary").expect("write");
    let (expected_sha, _) = web_core_runners::hash::sha256_file(binary.clone())
        .await
        .expect("hash");
    let (pool, _) = pool_with(config().with_binary(&binary)).await;
    let (old_id, t_old) = register(&pool, "old", settings(1, 0, 1)).await;
    let updater = Arc::new(RecordingUpdater::default());
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let _old = InProcessRunner::builder(
        &pool,
        t_old,
        registry(&gate, &started).with_contract_hash("contract-v0"),
    )
    .updater(updater.clone())
    .start();
    eventually("outdated", || {
        pool.get(old_id)
            .is_some_and(|r| r.state == RunnerState::Outdated && !r.contract_matches)
    })
    .await;
    eventually("update attempted", || {
        updater.0.lock().is_ok_and(|c| !c.is_empty())
    })
    .await;
    {
        let calls = updater.0.lock().expect("lock");
        assert_eq!(calls[0].0, "http://127.0.0.1:9/runners/binary");
        assert_eq!(calls[0].1, expected_sha);
    }

    let handle = submit(&pool, "echo", json!("x")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(pool.queued(), 1, "the outdated runner gets no work");

    let (_, t_new) = register(&pool, "new", settings(1, 0, 1)).await;
    let _new = InProcessRunner::start(&pool, t_new, registry(&gate, &started));
    assert_eq!(finish(handle).await.expect("output").output, json!("x"));
}

#[tokio::test]
async fn local_fallback_starts_and_stops() {
    let (pool, _) = pool_with(config()).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let spawner = InProcessSpawner::new(&pool, registry(&gate, &started));
    let mut local_config = LocalFallbackConfig::new("/unused/runner", 9, "/unused/work");
    local_config.auto_grace = Duration::ZERO;
    local_config.exit_grace = Duration::from_secs(5);
    local_config.restart_delay = Duration::from_millis(20);
    let supervisor = LocalFallbackSupervisor::start_with(
        pool.clone(),
        local_config,
        LocalRunnerMode::Auto,
        Arc::new(spawner.clone()),
    );
    let summary = pool.summary();
    eventually("local connected", || summary.borrow().local_connected).await;
    assert_eq!(spawner.spawned(), 1);

    // the local runner does the work while no remote exists
    let out = finish(submit(&pool, "echo", json!("local")).await)
        .await
        .expect("output");
    assert_eq!(out.output, json!("local"));
    assert!(pool.list().iter().any(|r| r.local));

    // a remote connects: the local one is told to exit and does
    let (_, token) = register(&pool, "remote", settings(1, 0, 1)).await;
    let remote = InProcessRunner::start(&pool, token, registry(&gate, &started));
    eventually("local stopped", || {
        supervisor.state().status == LocalRunnerStatus::Stopped
    })
    .await;
    assert!(!summary.borrow().local_connected);
    assert_eq!(supervisor.state().restarts, 0);

    // the remote goes away: the local one comes back
    remote.set_online(false);
    eventually("local back", || summary.borrow().local_connected).await;
    assert_eq!(spawner.spawned(), 2);

    // it dies while needed: restarted
    spawner.crash_latest();
    eventually("restarted", || spawner.spawned() == 3).await;
    eventually("counted", || supervisor.state().restarts == 1).await;

    // switched off: stopped
    supervisor.set_mode(LocalRunnerMode::Off);
    eventually("off", || {
        supervisor.state().status == LocalRunnerStatus::Stopped
    })
    .await;
    assert_eq!(supervisor.state().mode, LocalRunnerMode::Off);
}

#[tokio::test]
async fn checkpoint_reaches_subscriber_and_retry_input() {
    let (pool, store) = pool_with(config()).await;
    let (_, token) = register(&pool, "a", settings(1, 0, 1)).await;
    let registry = TaskRegistry::new(HASH).raw("probe", |ctx, input| {
        if let Some(known) = input.get("known_probe").filter(|v| !v.is_null()) {
            return Ok(json!({"probe": known, "probed_again": false}));
        }
        ctx.checkpoint(10.0, &json!({"duration_ms": 1234}));
        std::thread::sleep(Duration::from_millis(100));
        panic!("crash after the checkpoint");
    });
    let _runner = InProcessRunner::start(&pool, token, registry);

    let handle = pool
        .submit(
            "probe",
            json!({"known_probe": null}),
            TaskFiles::new(),
            SubmitOptions::new().with_retry_hook(|input, checkpoint| {
                input["known_probe"] = checkpoint.clone();
            }),
        )
        .await
        .expect("submit");
    let mut progress = handle.progress();
    let task_id = handle.id();
    let saw_checkpoint = tokio::spawn(async move {
        loop {
            if progress.borrow_and_update().checkpoint.is_some() {
                return progress.borrow().checkpoint.clone();
            }
            if progress.changed().await.is_err() {
                return None;
            }
        }
    });
    let out = finish(handle).await.expect("output");
    assert_eq!(
        out.output,
        json!({"probe": {"duration_ms": 1234}, "probed_again": false})
    );
    assert_eq!(
        saw_checkpoint.await.expect("join"),
        Some(json!({"duration_ms": 1234}))
    );
    pool.flush_store().await;
    let first = store
        .attempts()
        .into_iter()
        .find(|a| a.task_id == task_id && a.number == 1)
        .expect("first attempt");
    assert_eq!(first.checkpoint, Some(json!({"duration_ms": 1234})));
}

#[tokio::test]
async fn cancel_stops_the_attempt() {
    let (pool, store) = pool_with(config()).await;
    let (id, token) = register(&pool, "a", settings(1, 0, 1)).await;
    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let _runner = InProcessRunner::start(&pool, token, registry(&gate, &started));
    let handle = submit(&pool, "block", json!({"n": 1})).await;
    let queued = submit(&pool, "block", json!({"n": 2})).await;
    eventually("running", || started.lock().is_ok_and(|s| s.len() == 1)).await;
    let canceller = handle.canceller();
    let queued_id = queued.id();
    canceller.cancel();
    assert_eq!(finish(handle).await, Err(TaskError::Cancelled));
    // the freed slot goes to the queued task; dropping its handle cancels it too
    eventually("next started", || started.lock().is_ok_and(|s| s.len() == 2)).await;
    drop(queued);
    eventually("idle", || pool.running_on(id) == 0).await;
    pool.flush_store().await;
    let task = store
        .tasks()
        .into_iter()
        .find(|t| t.id == queued_id)
        .expect("task row");
    assert_eq!(task.status, TaskStatus::Cancelled);
}

#[tokio::test]
async fn tokens_register_and_revoke() {
    let (pool, store) = pool_with(config()).await;
    let response = pool.register("office-1", None).await.expect("register");
    assert_eq!(response.runner.capacity, 1, "fresh runners get capacity 1");
    let stored = store.runner(response.runner.id).expect("row");
    assert!(!stored.token_hash.contains(response.token.split('.').nth(1).expect("secret")));
    assert!(pool.attach(&response.token).await.is_ok());
    assert!(pool.attach("garbage").await.is_err());
    let wrong = format!("{}.{}", response.runner.id, "0".repeat(64));
    assert!(pool.attach(&wrong).await.is_err());
    assert!(pool.attach(&pool.local_token()).await.is_ok());

    pool.revoke(response.runner.id).await.expect("revoke");
    assert!(pool.attach(&response.token).await.is_err());
    assert!(store.runner(response.runner.id).is_none());
    assert!(pool.revoke(RunnerId::LOCAL).await.is_err());
}

#[derive(Default)]
struct RecordingServices(Mutex<Vec<String>>);

#[async_trait]
impl ServiceManager for RecordingServices {
    async fn start(&self, spec: &ServiceSpec) -> Result<(), String> {
        if let Ok(mut log) = self.0.lock() {
            log.push(format!("start {}", spec.unit));
        }
        Ok(())
    }

    async fn stop(&self, spec: &ServiceSpec) -> Result<(), String> {
        if let Ok(mut log) = self.0.lock() {
            log.push(format!("stop {}", spec.unit));
        }
        Ok(())
    }
}

#[tokio::test]
async fn managed_services_follow_drain_and_resume() {
    let (pool, _) = pool_with(config()).await;
    let (id, token) = register(&pool, "forge", settings(1, 0, 1)).await;
    let services = Arc::new(RecordingServices::default());
    let registry = TaskRegistry::new(HASH)
        .raw("echo", |_ctx, input| Ok(input))
        .service(ServiceSpec::new("clamav-daemon", ReadyCheck::TcpPort(3310)));
    let log = || services.0.lock().map(|l| l.clone()).unwrap_or_default();
    let mut runner = InProcessRunner::builder(&pool, token, registry)
        .services(services.clone())
        .start();
    eventually("started", || log() == vec!["start clamav-daemon"]).await;

    pool.drain(id).expect("drain");
    eventually("stopped", || log().len() == 2).await;
    assert_eq!(log()[1], "stop clamav-daemon");
    let handle = submit(&pool, "echo", json!(1)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(pool.queued(), 1, "a drained runner gets no work");

    pool.resume(id).expect("resume");
    assert_eq!(finish(handle).await.expect("output").output, json!(1));
    assert_eq!(log()[2], "start clamav-daemon");

    // SIGTERM-like shutdown stops the services on the way out
    assert_eq!(runner.shutdown().await, "exited");
    assert_eq!(log().last().map(String::as_str), Some("stop clamav-daemon"));
}

#[tokio::test]
async fn files_and_binary_over_http_and_websocket() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = dir.path().join("runner-bin");
    std::fs::write(&binary, b"binary bytes").expect("write");
    let (pool, _) = pool_with(
        PoolConfig::new(HASH, base.clone())
            .with_heartbeat(Duration::from_millis(100))
            .with_binary(&binary),
    )
    .await;
    let app = pool.router();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let (id, token) = register(&pool, "ws", settings(1, 0, 1)).await;
    let registry = TaskRegistry::new(HASH).raw("upper", |ctx, input| {
        let text = std::fs::read_to_string(ctx.input("source")).map_err(|e| e.to_string())?;
        ctx.progress(50.0);
        std::fs::write(ctx.output("result"), text.to_uppercase()).map_err(|e| e.to_string())?;
        Ok(json!({"len": text.len(), "input": input}))
    });
    let work = dir.path().join("work");
    let config = RunnerConfig {
        url: base.clone(),
        token: token.clone(),
        work_dir: work.clone(),
        log_level: "debug".into(),
        local: false,
    };
    let deps = RunnerDeps {
        connector: Arc::new(WsConnector::new(&base, token.clone())),
        executor: Arc::new(InlineExecutor::new(Arc::new(registry.clone()))),
        services: Arc::new(web_core_runners::runner::NoopServices),
        updater: Arc::new(web_core_runners::runner::NoUpdate),
        transfer: web_core_runners::runner::HttpTransfer::new().expect("client"),
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let runner = tokio::spawn(Runner::with_deps(config, registry, deps).run_until(async move {
        let _ = stop_rx.await;
    }));
    eventually("ws runner online", || {
        pool.get(id).is_some_and(|r| r.state == RunnerState::Online)
    })
    .await;

    let source = dir.path().join("source.txt");
    std::fs::write(&source, "hello runners").expect("write");
    let target = dir.path().join("out/result.txt");
    let handle = pool
        .submit(
            "upper",
            json!({"k": 1}),
            TaskFiles::new()
                .input("source", &source)
                .output("result", &target)
                .optional_output("extra", dir.path().join("extra.bin")),
            SubmitOptions::new(),
        )
        .await
        .expect("submit");
    let out = finish(handle).await.expect("output");
    assert_eq!(out.output, json!({"len": 13, "input": {"k": 1}}));
    assert_eq!(
        std::fs::read_to_string(&target).expect("output file"),
        "HELLO RUNNERS"
    );
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.files[0].path, target);
    // the attempt's work dir was wiped
    let leftovers: Vec<PathBuf> = std::fs::read_dir(work.join("attempts"))
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // the binary endpoint needs a token and reports the hash
    let client = reqwest::Client::new();
    let denied = client
        .get(format!("{base}/runners/binary"))
        .send()
        .await
        .expect("request");
    assert_eq!(denied.status(), 401);
    let ok = client
        .get(format!("{base}/runners/binary"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(ok.status(), 200);
    let headers: HashMap<String, String> = ok
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    assert_eq!(headers.get("x-runner-contract-hash").map(String::as_str), Some(HASH));
    assert_eq!(headers.get("x-runner-sha256").map(|s| s.len()), Some(64));
    assert_eq!(ok.bytes().await.expect("body").as_ref(), b"binary bytes");

    // a file URL without a valid signature is refused
    let forged = client
        .get(format!(
            "{base}/runners/files/{}/source?dir=in&exp=99999999999&sig=00",
            uuid::Uuid::new_v4()
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(forged.status(), 403);

    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), runner).await;
}

#[tokio::test]
async fn reload_runners_applies_store_changes() {
    use web_core_runners::app::{RunnerRecord, RunnerStatus, RunnerStore, issue_runner_token};

    let (pool, store) = pool_with(config()).await;
    // Another process (an admin API) registers a runner straight in the store.
    let id = RunnerId::new();
    let (token, token_hash) = issue_runner_token(id).expect("token");
    let mut record = RunnerRecord {
        id,
        name: "office".into(),
        token_hash,
        settings: settings(1, 0, 1),
        enabled: true,
        created_at: chrono::Utc::now(),
        status: RunnerStatus::default(),
    };
    store.save_runner(&record).await.expect("save");
    assert!(pool.get(id).is_none(), "not known before the reload");
    pool.reload_runners().await.expect("reload");
    assert_eq!(pool.get(id).map(|r| r.state), Some(RunnerState::Offline));

    let gate = Gate::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let _runner = InProcessRunner::start(&pool, token, registry(&gate, &started));
    eventually("online", || {
        pool.get(id).is_some_and(|r| r.state == RunnerState::Online)
    })
    .await;

    // Settings changed in the store reach the pool.
    record.settings.capacity = 3;
    record.name = "office-1".into();
    store.save_runner(&record).await.expect("save");
    pool.reload_runners().await.expect("reload");
    let view = pool.get(id).expect("runner");
    assert_eq!((view.capacity, view.name.as_str()), (3, "office-1"));

    // A running attempt of a deleted runner is lost and queued again.
    let handle = submit(&pool, "block", json!({"n": 1})).await;
    eventually("running", || pool.running_on(id) == 1).await;
    store.delete_runner(id).await.expect("delete");
    pool.reload_runners().await.expect("reload");
    assert!(pool.get(id).is_none());
    assert_eq!(pool.running(), 0);
    assert_eq!(pool.queued(), 1);
    drop(handle);
}
