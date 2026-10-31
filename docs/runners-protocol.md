# Runners: wire protocol

Protocol version **1** (`web_core_runners::PROTOCOL_VERSION`). Written from the types in
`web-core-runners/src/protocol/`; those types are the source of truth.

## Transport

- The runner opens a WebSocket to `<app base URL>/runners/ws` with
  `Authorization: Bearer <token>`. The token is `<runner id>.<secret>` (the local fallback
  gets an ephemeral token with the nil runner id). Bad tokens get `401` before the upgrade.
- Every message is one **text frame** holding one JSON object, tagged by `"type"` (snake_case).
  Other frame types are ignored. Maximum message size 16 MiB.
- Files never travel over the socket. Each `assign` carries signed HTTPS URLs (see "Files").
- The app never connects to a runner. A newer connection of the same runner replaces the older
  one (the older one is closed).
- Times are never exchanged: the runner sends monotonic elapsed values only; leases and
  timestamps are app time.

## Session

```
runner                                   app
  | -- hello -------------------------->  |  (first message, always)
  |  <------------------------ outdated   |  (only if protocol or contract differ; before welcome)
  |  <------------------------- welcome   |  (runner id, settings, attempts to abort)
  |  <--------------------------- drain   |  (only if the runner is drained in admin)
  | -- heartbeat (every heartbeat_ms) ->  |
  |  <-------------------------- assign   |  (up to capacity at a time)
  | -- progress ----------------------->  |
  | -- result ------------------------->  |
  |  <-- cancel / settings / drain / resume / exit
```

A runner that is outdated gets no `assign`. Messages other than `hello` sent before `hello`
are ignored; a second `hello` on one connection is ignored.

## Runner → app

### `hello`

```json
{"type":"hello","runner_name":"office-1","protocol_version":1,"contract_hash":"3f2a…",
 "tool_versions":{"ffmpeg":"7.1"},"in_flight":["<attempt id>"],"local":false,"runner_version":"0.5.0"}
```

| Field | Meaning |
|---|---|
| `runner_name` | Host name (`local` for the local fallback). Informational. |
| `protocol_version` | Must equal the app's, else `outdated`. |
| `contract_hash` | Must equal the app's, else `outdated`. |
| `tool_versions` | Informational, shown in admin. |
| `in_flight` | Attempts the runner still executes or holds an unsent result for (non-empty only after a reconnect; empty after a restart). |
| `local` | Local mode. |
| `runner_version` | Crate version. Informational. |

The app reconciles `in_flight` with its own record of the runner's attempts:

- listed and still current on this runner → kept, lease renewed;
- listed but not current (lost, reassigned, cancelled) → returned in `welcome.abort`;
- current on this runner but not listed → lost at once, the task gets its next attempt.

### `heartbeat`

```json
{"type":"heartbeat","running":["<attempt id>"]}
```

Sent every `heartbeat_ms`. Renews the lease of every listed attempt that is current on this
runner; for a listed attempt that is not, the app answers `cancel`.

### `progress`

```json
{"type":"progress","attempt":"<attempt id>","percent":42.5,"checkpoint":{"duration_ms":1234}}
```

Renews the attempt's lease. `checkpoint` is optional structured JSON: the app keeps the last
one per task, stores it on the attempt row, forwards it to the submitter, and passes it to the
submitter's retry hook before the next attempt. The runner throttles plain progress to one
message per 500 ms; checkpoints are never throttled. Progress for a non-current attempt gets a
`cancel`.

### `result`

```json
{"type":"result","attempt":"<attempt id>","elapsed_ms":5321,"outcome":{"kind":"ok","output":{},"files":[{"slot":"stored","sha256":"…","size":1024}]}}
```

`outcome.kind`:

| Kind | Fields | App's reaction |
|---|---|---|
| `ok` | `output` (JSON), `files` (`slot`, `sha256`, `size` of every uploaded output) | Verify every output part file against `files`, move it into place, resolve the task. A mismatch counts as `transfer_failed`. |
| `failed` | `reason` | Final: the task fails, no retry. |
| `crashed` | `exit_code` (null when killed by a signal), `stderr_tail` (last 8 KiB) | Next attempt. |
| `timed_out` | — | Next attempt. |
| `transfer_failed` | `reason` | Next attempt. |

A result for an attempt that is not the current one (lost, reassigned, cancelled) is logged
with a warning and discarded. After `max_attempts` attempts without a final outcome the task
fails (`AttemptsExhausted`).

### `draining`

```json
{"type":"draining"}
```

The runner is shutting down (SIGTERM): no more work. Its running attempts may still report
results while connected; when the connection closes they are lost at once (no lease wait).

### `log`

```json
{"type":"log","level":"error","message":"service clamav-daemon failed to start: …","attempt":null}
```

`level`: `info` | `warn` | `error`. The app writes it to its own log with the runner's name.

## App → runner

### `welcome`

```json
{"type":"welcome","runner_id":"<id>","settings":{"capacity":2,"heartbeat_ms":15000},"abort":["<attempt id>"]}
```

The runner kills and forgets every attempt in `abort` (and drops their unsent results), sends
the remaining buffered results, then starts its managed services (unless outdated or drained).

### `outdated`

```json
{"type":"outdated","binary_url":"https://api.example.com/runners/binary","sha256":"…","contract_hash":"3f2a…"}
```

Sent before `welcome` when the protocol or contract differs and the app has a binary to serve.
The runner gets no work. A remote runner downloads `binary_url` with its token once idle,
verifies `sha256`, renames it over its executable and re-executes itself. A local runner logs
an error and stays idle.

### `assign`

```json
{"type":"assign","attempt":"<attempt id>","task_id":"<task id>","number":1,"task_type":"prepare_media",
 "input":{},"timeout_ms":7200000,
 "inputs":[{"slot":"upload","url":"https://…/runners/files/<attempt>/upload?dir=in&exp=…&sig=…","sha256":"…","size":123}],
 "outputs":[{"slot":"stored","url":"https://…/runners/files/<attempt>/stored?dir=out&exp=…&sig=…","optional":false}]}
```

The runner downloads every input to `<work dir>/in/<slot>`, runs the task, and on success
uploads every output it finds at `<work dir>/out/<slot>`. A missing non-optional output is a
`failed` outcome. After `timeout_ms` the task is killed (`timed_out`). Every later message about
this run carries `attempt`.

### `cancel`

```json
{"type":"cancel","attempt":"<attempt id>"}
```

Kill the attempt, wipe its work dir, send nothing.

### `settings`

```json
{"type":"settings","capacity":3,"heartbeat_ms":15000}
```

New settings from admin. Capacity is enforced by the app; the runner uses `heartbeat_ms`.

### `drain` / `resume`

```json
{"type":"drain"}
{"type":"resume"}
```

`drain`: the app sends no more work; the runner stops its managed services once idle and stays
connected. `resume` undoes it and restarts the services.

### `exit`

```json
{"type":"exit","grace_ms":300000}
```

Finish running attempts within `grace_ms` (then kill them), stop services, close, exit the
process. The app treats the runner's attempts as lost when the connection closes. Used for the
local fallback when a remote runner comes online.

## Files

URL: `<base>/runners/files/{attempt}/{slot}?dir=in|out&exp=<unix seconds>&sig=<hex>`, where
`<base>` is the app's public URL (or the local URL for the local fallback). `sig` is the
Ed25519 signature (web-core `signatory`, key pair held by the pool) of
`"{attempt}/{slot}/{dir}/{exp}"`. A request is refused with `403` when the signature is wrong,
`exp` has passed, or the attempt is no longer current; URLs therefore expire with the attempt.
Slot names are `[A-Za-z0-9_.-]`, 1 to 64 characters, not starting with a dot.

| Method | `dir` | Meaning |
|---|---|---|
| `GET` | `in` | Download. Supports `Range: bytes=N-` / `bytes=N-M` (`206` + `Content-Range`). |
| `HEAD` | `out` | Upload offset: `Upload-Offset: <bytes already received>`. |
| `PUT` | `out` | Upload. `Content-Range: bytes A-B/T` appends at `A` (must equal the received size, else `409` with `Upload-Offset`); `A = 0` or no header starts over; `bytes */0` is an empty file. `204` on success. |

The runner verifies downloads against `sha256`/`size` (a mismatch restarts the download;
5 tries with backoff). The app writes uploads to `<target>.<attempt>.part` and verifies them
against the `files` of the `ok` result.

## Binary

`GET <base>/runners/binary` with `Authorization: Bearer <token>` serves the runner binary with
`x-runner-sha256` and `x-runner-contract-hash` headers.

## Task child process

The runner executes each attempt as `<runner binary> task <type> --work-dir <dir>` (cwd = the
work dir) with the input JSON on stdin. The child writes JSON lines to stdout:

```json
{"kind":"progress","percent":10,"checkpoint":{"duration_ms":1234}}
{"kind":"output","output":{}}
{"kind":"failed","reason":"unsupported media"}
```

The last `output` or `failed` line with exit status 0 is the outcome; any other exit (or exit
0 without such a line) is `crashed` with the last 8 KiB of stderr. Lines that are not JSON are
ignored.

## Constants

| Name | Value |
|---|---|
| Lease (app default) | 60 s |
| Heartbeat (app default, pushed) | 15 s |
| Reconnect backoff | 0.5 s doubling to 30 s, ±25 % jitter |
| Shutdown grace on SIGTERM | 60 s |
| Default `max_attempts` | 3 |
