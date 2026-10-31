# Runners: phase plan

Status: planning, 2026-10-30. No code exists yet. This document is the brief every implementation
phase starts from; each phase is run by an Opus 5.5 subagent (or a session) that reads this file
first, then the files it names.

"Runner" is the word. "Worker" in the apps and the rust-api skill means an apalis queue worker
and keeps that meaning; the docs rename those to "queue workers" where the two would collide.

## 1. What a runner is

A runner is a separate binary on another machine (today: Linux workstations in the office LAN,
behind NAT) that executes **tasks** for an app running on the AWS host. A task is one pure unit
of work: input files + a parameter struct in, output files + a result struct out. Orchestration
(the apalis job, the pipeline stages, every database read and write) stays in the app. A stage
that needs CPU hands one task to the runner pool and awaits it; everything the task needs is
resolved by the app before dispatch and travels in the input.

There is no channel for a task to ask the app anything mid-run. A task that would need one is
split into two tasks with an app stage in between. A task may *push* facts it learned early
(a **checkpoint** on the progress message, one-way, structured JSON); the app stage persists
them and a later attempt gets them back in its input. convonote's prepare_media needs this: it
records the probe (duration, media kind) before the long transcode, so a retry after a crash
still knows the upload was a video, and the file must not be downloaded twice.

First tasks:

| App | Task | Today lives in |
|---|---|---|
| convonote | `prepare_media` (ffprobe, decode check, transcode to the stored file, peaks) | `api-convonote/src/workers/audio/audio_uploaded/prepare_media.rs`, `ffmpeg.rs`, `store_audio.rs` |
| convonote | `convert_media` (the backfill of older audios to audio-only media) | `api-convonote/src/workers/audio/convert_media.rs` |
| core/forge (later) | Office preview (LibreOffice), typst PDF | `core-core/src/forge/previews.rs`, document worker |

Stages that are network-bound (ElevenLabs, LLM calls) or need the app's disk (tantivy index)
stay in the app.

## 2. Decisions (fixed)

1. **Runner dials out.** The runner opens a WebSocket to the app (through nginx, under the
   API's host, path `/runners/ws`) and keeps it open; reconnect with backoff. The app never
   connects to a runner. "Runner down" = missed heartbeats.
2. **Files over HTTPS, not over the socket.** The app issues one-time, task-scoped signed URLs
   for each input file (download) and each expected output file (upload). Transfers are
   resumable (ranges), hash-checked (SHA-256 in the manifest), and expire with the attempt.
3. **No app database on runners, by construction.** The product's contract crate depends on
   serde, schemars, uuid and nothing of `<product>-core`. The runner binary depends on the
   contract and `web-core-runners[runner]` only; it cannot link diesel or the schema.
4. **One token per runner**, created in admin, shown once, stored as an Argon2 hash (web-core's
   password hasher), revocable. The runner's whole config is four values: app URL, token, work
   dir, log level. Nothing else; capacity and every other knob come from the app.
5. **Capacity, priority, weight are app-side settings** per runner, pushed to the runner at
   hello and on change. A freshly registered runner has capacity 1.
   - *capacity*: hard maximum of concurrent tasks on that runner.
   - *priority*: tier; lower numbers are tried first, a tier is filled before the next is tried.
   - *weight*: share within a tier; the scheduler picks the runner with the lowest
     `running / weight` ratio among those with free capacity.
   - The backlog lives in the app. A runner never holds more than `capacity` tasks.
6. **Attempts and leases.** Every dispatch creates an attempt row (task id, attempt number,
   runner id, lease expiry). Heartbeat and progress messages renew the lease. Lease expiry marks
   the attempt lost and the scheduler creates the next attempt elsewhere. Every runner message
   carries the attempt id; a result for an attempt that is not the current one is logged and
   discarded. A reconnecting runner lists its in-flight attempt ids and the app answers with the
   ones to abort. Attempts beyond `max_attempts` (per task type, default 3) fail the task.
7. **A restarted runner resumes nothing.** On boot it clears its in-flight table and the work
   dir. Its sqlite holds only bookkeeping: in-flight attempts, work dirs to clean, a local log.
   No task content is ever stored in sqlite.
8. **Each task runs in a child process of the runner** (the runner re-executes itself with a
   `task` subcommand). A crash or panic ends one attempt; the runner reports `crashed` with exit
   code and the stderr tail and keeps serving. The runner itself runs under systemd
   `Restart=always`.
9. **Local fallback runner.** The app can spawn the *same runner binary* as a child process in
   local mode (connects to `127.0.0.1`, capacity 1, lowest priority). It starts only when zero
   remote runners are alive and is told to drain and exit as soon as any remote runner
   connects. It is a child process, so a crash never touches the app; the app sees the exit
   status and the lease covers the rest. Toggle in admin: off, auto (the above), always-on.
10. **Managed services.** A runner's task-type implementation can declare services it needs
    (forge: clamd). The runner starts them when it starts accepting work and stops them when it
    drains or exits, so a sleeping local fallback costs no RAM. Declared in the product's runner
    crate as `systemctl start/stop <unit>` with a readiness check; the runner's systemd unit and
    sudoers rule allow exactly those units.
11. **Contract hash, not versions.** `<product>-contract` derives a JSON schema (schemars) for
    every task input/output type; `build.rs` hashes the canonical schema set into
    `CONTRACT_HASH`. Hello carries the transport protocol version (from web-core-runners, rarely
    changes) and the contract hash. Mismatch → the runner is marked `outdated` in admin, gets no
    work, and self-updates (next point). Tool versions (ffmpeg, libreoffice) are reported at
    hello and shown in admin; they are informational, not part of the hash.
12. **The app serves the runner binary.** deploy.ab cross-builds the runner for
    `x86_64-unknown-linux-gnu` and the API `.deb` installs it under the app's directory; the app
    serves it at `/runners/binary` (token-authenticated) with its SHA-256 and contract hash.
    An outdated runner downloads it, verifies the hash, swaps the executable and re-executes
    itself (systemd restart). The local fallback uses the aarch64 build of the same crate, which
    the `.deb` also installs.
13. **Privacy.** The work dir is wiped the moment an attempt ends (success, failure or abort),
    and at boot. Nothing of a task survives on the runner.
14. **Clock.** Lease and timestamp arithmetic is done with app time only; the runner sends
    monotonic elapsed values, never wall-clock times.
15. **Office machines are Linux.** No time windows, no Windows process handling, for now.

## 3. Shape of the code

```
public/web-core/
  web-core-runners/               new workspace member, features: app, runner
    src/protocol/                  messages, manifests, hello, contract fingerprint helpers
    src/runner/                    [runner] main loop, sqlite, child-process tasks,
                                   managed services, self-update, local mode
    src/app/                       [app] tables + migrations, RunnerPool (scheduler, leases),
                                   axum routes (ws, files, binary), local fallback supervisor,
                                   admin DTOs, in-process test transport
  docs/runners-plan.md             this file
  docs/runners-protocol.md         written in phase 1, the message-level spec

convonote/api.convonote.in/
  convonote-contract/              task types + CONTRACT_HASH; no diesel
  runner-convonote/                binary: implements the tasks (ffmpeg moves here)
  api-convonote/                   dispatch in the two stages, routes, local fallback, admin APIs
  api-convonote-admin/             runners admin APIs (list, register, settings, revoke, drain)
convonote/admin.convonote.in/      Runners page

bedrock-foundry/project-skills/rust-api/references/runners.md   new
```

## 4. Phases

Each phase ends with its "done" list satisfied, tests passing, and a short note appended to
this file under "Log". Phases 1, 2 and 3 are sequential; the sub-steps inside phase 3 marked
"parallel" can run as separate subagents.

### Phase 1 — `web-core-runners` crate (web-core 0.5.0)

Prerequisite: the uncommitted 0.4.2 work in the checkout is finished and published first.
This phase must not sweep it into its publish.

**1a. Protocol and contract fingerprint** (`src/protocol/`)
- Messages, runner→app: `Hello { runner_name, protocol_version, contract_hash, tool_versions,
  in_flight: Vec<AttemptId> }`, `Heartbeat { running }`, `Progress { attempt, percent, checkpoint: Option<json> }`,
  `Result { attempt, outcome: Ok(output json) | Failed(reason) | Crashed { exit_code, stderr_tail } }`,
  `Draining`, `Log`.
- Messages, app→runner: `Welcome { runner_id, capacity, settings, abort: Vec<AttemptId> }`,
  `Outdated { binary_url, sha256 }`, `Assign { attempt, task_type, input json, files manifest
  (download URLs + hashes), output slots (upload URLs), timeout }`, `Cancel { attempt }`,
  `Settings { capacity, ... }`, `Drain`, `Exit`.
- `Task` trait: `const TYPE: &str`, `Input: Serialize + DeserializeOwned + JsonSchema`,
  `Output` likewise, `fn files(&input) -> Vec<FileSlot>`.
- `contract_hash(types)` helper used from a product's `build.rs`; canonical JSON (sorted keys).
- Done: `docs/runners-protocol.md` written from the types; unit tests for serde round-trips
  and fingerprint stability.

**1b. Runner runtime** (`src/runner/`, feature `runner`)
- `Runner::run(config, registry)`: connect, hello, reconnect loop with backoff and jitter.
- sqlite (rusqlite, bundled) at `<work_dir>/runner.db`: `in_flight`, `work_dirs`, `log`.
  Cleared at boot per decision 7.
- Attempt execution: download inputs (resumable, hash-checked) → spawn `self task <type>` child
  with the input on stdin and the work dir → stream its progress lines → upload outputs →
  send `Result`. Child killed on `Cancel`, on timeout, and on drain-with-exit.
- Managed services: `ServiceSpec { unit, ready_check }` list from the registry; start on first
  `Welcome`, stop on `Exit` and on shutdown signal.
- Self-update on `Outdated`: download, verify, atomic rename, `exec`.
- Local mode: `--local` flag = connect to the URL given, no self-update, exit on `Exit`.
- Graceful shutdown on SIGTERM: send `Draining`, finish or abort per a deadline, exit.
- Done: runs against the in-process test transport of 1c; crash of a task child is reported
  and the runner survives; boot after a simulated kill resumes nothing.

**1c. App side** (`src/app/`, feature `app`)
- Refinement (2026-10-30, before implementation): the crate owns **no database**. The
  pool's live state (connected runners, running attempts, the queue of waiting tasks) is in
  memory; a task is awaited inside the caller's apalis job, and an app restart requeues that
  job anyway, so task rows need no durability for correctness. What must persist goes through
  a `RunnerStore` trait the app implements with its own Diesel: load runner configs, update
  a runner's last-seen/state/tool versions, record attempt history for admin. The crate ships
  an in-memory store for tests. The tables below are therefore the *app's* migrations
  (convonote adds them in 3b), with the crate documenting the expected shape.
- Tables (in the app, Diesel migrations):
  `runners` (id, name, token_hash, capacity, priority, weight, enabled, last_seen_at,
  protocol_version, contract_hash, tool_versions jsonb, state: offline|online|outdated|draining),
  `runner_tasks` (id, task_type, input jsonb, status, max_attempts, created_at, finished_at,
  output jsonb, failure), `runner_attempts` (id, task_id, number, runner_id, lease_until,
  status: running|lost|aborted|done|failed|crashed, progress, started_at, ended_at, error).
- `RunnerPool`: `submit(task) -> TaskHandle`, `await` the handle for the output; scheduler per
  decision 5; lease sweeper per decision 6; progress and cancel hooks the caller can subscribe
  to (convonote forwards percent to `processing::report_progress` and admin cancel to `Cancel`).
- Axum routes mounted by the app: `/runners/ws` (token auth), `/runners/files/{attempt}/{slot}`
  GET/PUT with signed query (web-core signatory), `/runners/binary`.
- Local fallback supervisor: watches alive-runner count; spawns `<app_dir>/runner --local`
  with capacity 1; sends `Exit` when a remote runner connects; restarts the child if it dies
  while still needed; exposes its state for admin.
- Admin DTOs and operations: list, register (returns the token once), update settings, enable
  or disable, revoke, drain, plus task and attempt history for a job.
- Test transport: `InProcessRunner` that runs the runner loop in the same process over
  channels, so app tests exercise dispatch, lease loss, late results and crashes without a
  socket. Convonote's pipeline tests will use it.
- Done: tests for the scheduler (priority fill, weighted spread, queue-when-full), lease loss
  and reassignment, late result discarded, crash → next attempt, max attempts → task failed,
  local fallback start and stop.

**1d. Release**: `web-core-runners` starts at 0.5.0; `web-core` stays at its unpublished
0.4.2 (both apps use path dependencies, so nothing needs publishing for the integration). Superseded 2026-10-31: every crate released as 0.5.0 (see Log).
README section and `publish.ab` order updated (`web-core-runners` after `web-core`).
Publishing itself waits for the user's explicit yes, per the skill's "Changing web-core".

### Phase 2 — skill updates (`project-skills`)

- `rust-api/references/runners.md`: when a stage becomes a task, the contract crate rules,
  the runner crate layout, admin page expectations, local fallback, managed services, testing
  with `InProcessRunner`.
- `rust-api/references/workers.md`: "queue workers" wording; a section "queue worker vs
  runner task" pointing at runners.md.
- `rust-api/references/deployment.md`: the runner is cross-built for x86-64 and bundled in
  the API `.deb`; the runner's own `.deb` for office machines (systemd unit, `Restart=always`,
  sudoers for managed units, four-line env); the aarch64 copy for the local fallback.
- `rust-api/references/testing.md`: in-process runner in integration tests.
- `rust-api/references/web-core.md`: the new crate in the table and the public API list.
- `svelte-frontend`: the Runners admin page pattern (list with state chips, register dialog
  showing the token once, settings form, drain and revoke, attempt history on the job page).
- `rust-api/SKILL.md`: the "when to read what" row and the workspace shape gain the contract
  and runner crates.

### Phase 3 — convonote integration

**3a. Contract and runner crates** (parallel with 3b after the task types are agreed)
- `convonote-contract`: `PrepareMedia { input: upload file slot, upload_extension,
  content_type_hint, peak_buckets, max_transcribe_bytes, already_stored: bool,
  known_probe: Option<Probe> } -> { probe, stored file slot (ext), peaks slot, duration_ms,
  media_kind, creation_time }`, with the probe sent as a checkpoint right after ffprobe so
  `record_probe` runs in the app before the transcode starts; `ConvertMedia { input: existing media slot(s) }
  -> { stored file slot, peaks slot, leftovers to delete }`. Exact fields come from reading
  `prepare_media.rs`, `store_audio.rs`, `convert_media.rs` and `ffmpeg.rs`; the rule is that
  everything those functions read from `Audio`, env or settings becomes an input field.
- `runner-convonote`: moves `ffmpeg.rs` and the pure parts of `store_audio.rs` out of
  `api-convonote`; implements both tasks; no managed services. `deb` metadata for the office
  package, `depends = ffmpeg`.
- Done: the two tasks run from the command line on a sample file; unit tests moved with
  `ffmpeg.rs` still pass.

**3b. App side**
- Workspace: add the two crates; `api-convonote` depends on `convonote-contract` and
  `web-core-runners[app]`; migrations for the three tables.
- `prepare_media::execute` and `convert_media::convert_one` become: build input, stage input
  files into the task's file slots, `pool.submit`, forward progress to `Progress::report` and
  the stage's cancel future to `Cancel`, apply the output to `Audio` and move the output files
  into place. The DB reads around them (record_probe, ensure_audio_not_moving, the row update
  under lock) stay exactly where they are.
- Routes mounted in `main.rs`; local fallback supervisor started in `start_workers`; the
  `worker_count` setting's hint updated (it now bounds in-flight tasks, not CPU).
- Settings: `local_runner_mode` (off|auto|always) joins `PlatformSettings`.
- `api-convonote-admin`: runners endpoints per 1c; the jobs detail gains the attempt history.
- Tests: `pipeline_test.rs` and `jobs_test.rs` run with `InProcessRunner`; new tests for
  lease loss during prepare_media and for a late result after a retry.
- Done: the full pipeline passes with the in-process runner; no `Command::new("ffmpeg")`
  remains in `api-convonote`.

**3c. Admin frontend** (parallel with 3b once the DTOs exist)
- `admin.convonote.in`: Runners page (list, register, settings, drain, revoke, local fallback
  state and toggle), attempt history on the job detail, overview card "runners online".

**3d. Deployment**
- `deploy.ab`: cross-build `runner-convonote` for x86-64 and aarch64; the API `.deb` installs
  both under the app dir; a separate `runner-convonote` x86-64 `.deb` for office machines with
  its systemd unit and `postinst` asking for the four env values. `Instructions.md` gets an
  "Installing a runner" section.
- nginx: WebSocket upgrade and long `proxy_read_timeout` on `/runners/ws`; large bodies on
  `/runners/files`.

**3e. Rollout**
1. Deploy with `local_runner_mode = always` and no remote runners: everything still runs on
   the AWS host, now through the runner path. Watch the jobs page for a few days.
2. Install one office runner with capacity 1, mode `auto`. Confirm the local fallback exits
   when it connects and returns when it is switched off overnight.
3. Raise capacity, add runners.

### Later (not planned in detail)
- core/forge: previews and PDF tasks, clamd as a managed service of the forge runner.
- Signing task payloads with ed25519 if a runner ever leaves the office network.
- Alerting when no runner has been online for N minutes.

## 5. Open questions

- Office uplink bandwidth: measure before 3e step 2; it decides whether 5 GiB uploads are
  worth sending out at all or whether a size cutoff keeps the largest files local.
- Where the runner's work dir lives on office machines and its size quota.
- Whether `jobs_per_workspace` fairness should also apply to runner assignment (today it is
  enforced at claim time in the app, which still holds; noted so nobody re-implements it).

## Log

- 2026-10-30: plan written.
- 2026-10-31: phase 1 (1a–1d) implemented in `web-core-runners` 0.5.0 (not published, not
  committed). No database in the crate: in-memory pool + `RunnerStore` trait (`MemoryStore` for
  tests); table shapes in `web-core-runners/README.md`; protocol in `docs/runners-protocol.md`.
  Additions to the plan's message set: `Resume` (undo `Drain`), `Exit { grace_ms }`,
  `Outdated.contract_hash`, `Assign.task_id/number`, `Result.elapsed_ms`, outcome kinds
  `timed_out` and `transfer_failed` (retried, stored as attempt status `crashed` with the
  reason in `error`), uploaded-file hashes in `Outcome::Ok.files`. rusqlite is 0.32 (sqlx's
  libsqlite3-sys pin). 40 unit/integration tests pass (`--features app,runner`).
- 2026-10-31: phases 3a and 3b implemented in api.convonote.in (branch `pricing`, not
  committed): `convonote-contract`, `runner-convonote` (lib + bin; `ffmpeg.rs` and the pure
  parts of `store_audio.rs` moved), migrations `2026-11-11-000001-0000_runners` and
  `2026-11-11-000002-0000_runner_settings`, `PgRunnerStore`, admin `/runners` endpoints.
  Admin has no pool: it writes `runners` (new `drained` column) and table triggers NOTIFY
  `convonote_runners`; the API re-reads with `RunnerPool::reload_runners` (added to the crate)
  and applies drain / mode. The upload check (`complete_upload`) still runs ffprobe on the API
  host through `runner_convonote::media` (the chunks are only there).
- 2026-10-31: all phases implemented (web-core-runners 0.5.0; skills; convonote contract, runner, app, admin API, admin UI, packaging). Nothing committed or published; convonote repos on `pricing`. The local fallback work dir moved to `LOCAL_RUNNER_WORK_DIR` (default `/var/cache/convonote/local-runner`) to match the skill. Not yet exercised: a real office runner against a deployed API, the production deploy, the maintainer scripts on a host.
- 2026-10-31: web-core 0.4.2 (sized pool) committed on its own; then every workspace crate bumped to 0.5.0, committed, pushed and published to crates.io in publish.ab order.
- 2026-10-31: convonote deployment files consolidated under `deployment/` (`deploy.ab` run from the root with `<dev|production> [api|admin|all]`, `debian/<crate>/` for api-convonote, api-convonote-admin and runner-convonote; `maintainer-scripts = "../deployment/debian/<crate>/"`); the frontends' `deploy.ab` moved to `deployment/` too; skill updated to the same rule. Verified with `amber run deployment/deploy.ab dev`.
