# web-core-runners

Remote task runners for web-core apps. An app hands pure units of work (**tasks**: input files
and a JSON input in, output files and a JSON output out) to **runners**: separate binaries on
other machines that dial out to the app over a WebSocket, so they work behind NAT. Files travel
over HTTPS with signed, expiring, resumable URLs. Orchestration and every database access stay
in the app.

| Feature | What you get |
|---------|--------------|
| (none) | `protocol`: wire messages, the `Task` trait, `Contract` fingerprint helpers. For the product's contract crate. |
| `runner` | The runner runtime: `TaskRegistry`, `runner::main`, child-process execution, sqlite bookkeeping, managed services, self-update. For the product's runner binary. |
| `app` | `RunnerPool` (scheduler, leases, axum routes, admin operations), `RunnerStore`, `LocalFallbackSupervisor`. For the API. |
| `app` + `runner` | `InProcessRunner` and `InProcessSpawner` for app tests. |

The message-level spec is [`docs/runners-protocol.md`](../docs/runners-protocol.md); the design
and its decisions are in [`docs/runners-plan.md`](../docs/runners-plan.md).

## Crate layout of a product

```
<product>-contract/   serde, schemars, uuid, web-core-runners (no features). Task types + contract_hash()
runner-<product>/     <product>-contract, web-core-runners[runner]. The binary.
<product>-api/        <product>-contract, web-core-runners[app]. Submits tasks.
```

The runner binary cannot link Diesel or the schema: it depends on the contract and
`web-core-runners[runner]` only.

## Contract

```rust
use std::sync::OnceLock;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use web_core_runners::protocol::{Contract, FileSlot, Task};

pub struct Thumbnail;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ThumbnailInput { pub width: u32 }

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ThumbnailOutput { pub height: u32 }

impl Task for Thumbnail {
    const TYPE: &'static str = "thumbnail";
    type Input = ThumbnailInput;
    type Output = ThumbnailOutput;
    fn files(_: &ThumbnailInput) -> Vec<FileSlot> {
        vec![FileSlot::input("image"), FileSlot::output("thumbnail")]
    }
}

/// The contract fingerprint: SHA-256 of the canonical JSON schemas of every task type.
pub fn contract_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| Contract::new().task::<Thumbnail>().hash())
}
```

The hash covers each task's `TYPE` and the schemas of its input and output (sorted keys, no
whitespace, doc-comment `description`s removed). A runner whose hash differs from the app's is
`outdated`: it gets no work and updates itself from the app.

`OnceLock` at first use is the simplest option. If a compile-time constant is wanted
(`CONTRACT_HASH` in `deploy.ab` output, for example), compute it in a `build.rs` of the
**runner or API crate** with the contract crate as a build dependency (a crate's own
`build.rs` cannot use the crate it builds):

```toml
# runner-<product>/Cargo.toml
[build-dependencies]
<product>-contract = { path = "../<product>-contract" }
```

```rust
// runner-<product>/build.rs
fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let hash = product_contract::contract_hash();
    std::fs::write(format!("{out}/contract_hash.rs"), format!("pub const CONTRACT_HASH: &str = {hash:?};")).unwrap();
    println!("cargo:rerun-if-changed=../<product>-contract/src");
}
// src/main.rs: include!(concat!(env!("OUT_DIR"), "/contract_hash.rs"));
```

`<runner binary> contract-hash` prints the hash the binary was built with.

## Runner binary

```rust
use web_core_runners::runner::{self, ReadyCheck, ServiceSpec, TaskRegistry};

fn main() -> std::process::ExitCode {
    let registry = TaskRegistry::new(product_contract::contract_hash())
        .task::<product_contract::Thumbnail, _>(|ctx, input| {
            let image = ctx.input("image");          // <work dir>/in/image, downloaded and verified
            ctx.progress(10.0);
            ctx.checkpoint(20.0, &serde_json::json!({"probe": "facts"})); // persisted by the app
            std::fs::write(ctx.output("thumbnail"), b"...").map_err(|e| e.to_string())?;
            Ok(product_contract::ThumbnailOutput { height: input.width / 2 })
        })
        // forge only: started when accepting work, stopped when drained or exiting
        .service(ServiceSpec::new("clamav-daemon.service", ReadyCheck::TcpPort(3310)))
        .tool_versions(|| [("ffmpeg".to_string(), "7.1".to_string())].into());
    runner::main(registry)
}
```

Configuration is four values, as flags or env (`RUNNER_URL`, `RUNNER_TOKEN`,
`RUNNER_WORK_DIR` default `/var/lib/runner`, `RUNNER_LOG_LEVEL` default `info`), plus `--local`
for the app's fallback. Capacity and every other knob come from the app.

What the runtime does:

- Connects to `<url>/runners/ws` with `Authorization: Bearer <token>`, says hello, reconnects
  with backoff (0.5 s doubling to 30 s, ±25 % jitter). Results produced while disconnected are
  sent after the next `Welcome` unless the app aborts them.
- Each attempt runs in a child process: the binary re-executes itself as
  `<binary> task <type> --work-dir <dir>` with the input JSON on stdin; the child writes JSON
  lines (`progress`, then `output` or `failed`) to stdout. Non-JSON stdout lines are ignored. A
  panic or kill is detected by exit status and reported as `crashed` with the stderr tail; the
  runner keeps serving.
- Inputs are downloaded with `Range` resume and SHA-256 checks; outputs are uploaded with
  `HEAD` (received offset) + `PUT` with `Content-Range`.
- Bookkeeping in `<work dir>/runner.db` (sqlite: `in_flight`, `work_dirs`, `log`). At boot the
  in-flight table and every attempt dir are wiped: a restarted runner resumes nothing. An
  attempt's dir is wiped the moment it ends.
- `Outdated` → download `/runners/binary` (token-authenticated), verify, atomic rename over
  the executable, `exec` with the same arguments. **The executable's directory must be writable
  by the runner's user.** Disabled in local mode.
- SIGTERM → `Draining`, running attempts get 60 s, then are killed; services stop; exit.
- Managed services run through `sudo -n systemctl start|stop <unit>` when not root (the
  runner's sudoers rule must allow exactly those units), then poll the readiness check.

TLS for the WebSocket and transfers uses rustls with the `ring` provider (via reqwest). Do not
add a dependency that enables rustls' `aws-lc-rs` provider too, or install a default provider
first.

## App

```rust
use std::sync::Arc;
use web_core_runners::app::*;

let pool = RunnerPool::start(
    PoolConfig::new(product_contract::contract_hash(), "https://api.example.com")
        .with_local_url(format!("http://127.0.0.1:{port}"))
        .with_binary("/usr/lib/product/runner-x86_64")
        .with_max_attempts("prepare_media", 3),
    Arc::new(MyDieselRunnerStore::new(db_pool)),
)
.await?;

let app = Router::new().merge(pool.router()) /* outside the user auth layer */;

let local = LocalFallbackSupervisor::start(
    pool.clone(),
    LocalFallbackConfig::new("/usr/lib/product/runner-aarch64", port, "/var/lib/product/local-runner"),
    LocalRunnerMode::Auto, // from settings
);

// inside a job stage
let handle = pool
    .submit_typed::<Thumbnail>(
        ThumbnailInput { width: 320 },
        TaskFiles::new().input("image", &source).output("thumbnail", &target),
        SubmitOptions::new()
            .with_reference(job_id.to_string())
            .with_retry_hook(|input, checkpoint| input["known_probe"] = checkpoint.clone()),
    )
    .await?;
let mut progress = handle.progress();         // watch: status, attempt, percent, checkpoint
let cancel = handle.canceller();              // cancel() from anywhere
let out = handle.await?;                      // TypedOutput { output, files, attempts }
```

- **Scheduling.** The queue is FIFO and lives in the app. The head goes to the runner in the
  lowest priority number with free capacity; within a tier, to the lowest `running / weight`.
  A runner never holds more than `capacity` attempts. Disabled, drained, outdated and
  disconnected runners get no work.
- **Leases.** Heartbeats (every `heartbeat`, pushed to the runner) and progress renew an
  attempt's lease. A sweeper ends expired attempts as `lost`, sends `Cancel` to the runner if it
  is still connected, and queues the task again at the front. Results for attempts that are no
  longer current are logged (`tracing::warn!`) and discarded.
- **Retries.** `crashed`, `timed_out`, `transfer_failed` and `lost` get another attempt until
  `max_attempts` (per submission, else per task type, else 3); then the task fails with
  `TaskError::AttemptsExhausted`. A `failed` outcome is final.
- **Checkpoints** are forwarded to `progress()`, stored on the attempt row, and handed to the
  retry hook before the next attempt.
- **Dropping** an unfinished `TaskHandle` cancels the task.
- **Files.** Inputs are hashed at submit (or pass `input_with_hash`). Outputs are written to
  `<target>.<attempt>.part`, verified against the runner's SHA-256 and size, then renamed to the
  target. The parent directory is created when missing.
- **Tokens** are `<runner id>.<secret>`; only the Argon2 hash of the secret is stored. The local
  fallback gets an ephemeral token held in memory, and the reserved id `RunnerId::LOCAL`
  (nil UUID), which has no `runners` row.
- **Admin:** `list`, `get`, `register` (token shown once), `update_settings`
  (`RunnerSettingsPatch`), `set_enabled`, `revoke`, `drain`, `resume`, `task_history`; the
  supervisor's `state()` / `set_mode()` for the local fallback. All DTOs are serde types.

### Routes

| Route | Auth |
|-------|------|
| `GET /runners/ws` | `Authorization: Bearer <runner token>` |
| `GET` / `HEAD` / `PUT /runners/files/{attempt}/{slot}?dir=&exp=&sig=` | Ed25519 signature (web-core `signatory`), expiry, and the attempt must be current |
| `GET /runners/binary` | runner token; headers `x-runner-sha256`, `x-runner-contract-hash` |

nginx: WebSocket upgrade and a long `proxy_read_timeout` on `/runners/ws`; no body size limit
and no request buffering on `/runners/files`.

### Tables the app creates

The crate owns no database. Live state (connections, running attempts, the queue) is in
memory; a task is awaited inside the caller's job, and an app restart requeues that job. What
persists goes through the `RunnerStore` trait, which the app implements with its own Diesel
models over these tables (`MemoryStore` is the test implementation):

```sql
CREATE TABLE runners (
    id               UUID PRIMARY KEY,
    name             TEXT NOT NULL,
    token_hash       TEXT NOT NULL,
    capacity         INTEGER NOT NULL DEFAULT 1,
    priority         INTEGER NOT NULL DEFAULT 0,
    weight           INTEGER NOT NULL DEFAULT 1,
    enabled          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at     TIMESTAMPTZ,
    protocol_version INTEGER,
    contract_hash    TEXT,
    tool_versions    JSONB NOT NULL DEFAULT '{}',
    state            TEXT NOT NULL DEFAULT 'offline'  -- offline | online | outdated | draining
);

CREATE TABLE runner_tasks (
    id           UUID PRIMARY KEY,
    task_type    TEXT NOT NULL,
    input        JSONB NOT NULL,
    status       TEXT NOT NULL,                       -- queued | running | done | failed | cancelled
    max_attempts INTEGER NOT NULL,
    reference    TEXT,                                -- the submitter's job id
    created_at   TIMESTAMPTZ NOT NULL,
    finished_at  TIMESTAMPTZ,
    output       JSONB,
    failure      TEXT
);
CREATE INDEX runner_tasks_reference ON runner_tasks (reference);

CREATE TABLE runner_attempts (
    id          UUID PRIMARY KEY,
    task_id     UUID NOT NULL REFERENCES runner_tasks (id) ON DELETE CASCADE,
    number      INTEGER NOT NULL,
    runner_id   UUID NOT NULL,                        -- no FK: nil UUID = local fallback, revoked runners are deleted
    runner_name TEXT NOT NULL,
    lease_until TIMESTAMPTZ NOT NULL,
    status      TEXT NOT NULL,                        -- running | lost | aborted | done | failed | crashed
    progress    REAL NOT NULL DEFAULT 0,
    checkpoint  JSONB,
    started_at  TIMESTAMPTZ NOT NULL,
    ended_at    TIMESTAMPTZ,
    error       TEXT
);
CREATE INDEX runner_attempts_task ON runner_attempts (task_id);
```

Store semantics: `save_runner` upserts name, token hash, settings and enabled (not the status
columns); `update_runner_status` writes the status columns; `save_task` and `save_attempt` are
upserts by id. Status and history writes are queued and applied in order by a background task
(failures are logged, never block dispatch); `RunnerPool::flush_store()` waits for them.
`last_seen_at` is written at most every 30 s per runner plus on every state change. Attempt
rows are written at start, on each checkpoint and at the end, not on every lease renewal, so
`lease_until` and `progress` are as of the last write. At app start the pool reads `runners`
once and treats them all as offline.

## Testing

```rust
let pool = RunnerPool::start(PoolConfig::new(HASH, "http://127.0.0.1:9"), Arc::new(MemoryStore::new())).await?;
let token = pool.register("test", None).await?.token;
let registry = TaskRegistry::new(HASH).raw("echo", |_ctx, input| Ok(input));
let runner = InProcessRunner::start(&pool, token, registry);  // closures, no socket
// runner.disconnect(), runner.set_online(false), runner.restart(), runner.kill(), runner.shutdown()
```

In-process tasks run closures on blocking threads; a panic counts as a crash. A long closure
should poll `ctx.is_cancelled()`, since it cannot be killed. Tests with file slots serve
`pool.router()` on a local port and set `public_url` to it. `InProcessSpawner` makes the local
fallback supervisor start in-process runners (also handy with `LocalRunnerMode::Always`).
`pool.attach(token)` returns a raw `RunnerSession` to act as a misbehaving runner by hand.
