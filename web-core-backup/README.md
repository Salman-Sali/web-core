# web-core-backup

Backs up an app's Postgres databases and storage folders into one `.tar.zst` archive on S3, and restores it. The app's own binary gets two subcommands:

```bash
my-api backup
my-api restore --list [--host <hostname>]
my-api restore <timestamp> [--host <hostname>]
```

There is also an optional in-process scheduler for regular backups.

## Setup

**Cargo.toml** of the app:

```toml
[dependencies]
web-core-backup = "0.3"

[package.metadata.deb]
depends = "$auto, postgresql-client"
```

`pg_dump`, `pg_restore` and `psql` must be installed. `pg_dump` must be the same major version as the Postgres server or newer. If the server is newer than your distro's default `postgresql-client`, depend on `postgresql-client-<major>` from the PGDG apt repository instead. `build()` fails when the tools are missing. A backup fails with a clear message when `pg_dump` is older than the server.

**main.rs**:

```rust
use std::time::Duration;
use web_core_backup::{Backup, EmailNotify, S3Target};

#[tokio::main]
async fn main() {
    let config = AppConfig::load();

    let backup = Backup::builder("my-api")
        .app_version(env!("CARGO_PKG_VERSION"))
        .database("main", &config.database_url)
        .database("analytics", &config.analytics_database_url)
        .folder("uploads", "/var/lib/my-api/uploads")
        .folder("avatars", "/var/lib/my-api/avatars")
        .s3(S3Target::new("my-backups-bucket")
            .region("ap-south-1")
            .profile("backup")
            .prefix("my-api"))
        .notify(EmailNotify::new("backups@example.com", ["ops@example.com"]))
        .schedule_every(Duration::from_secs(24 * 60 * 60))   // optional
        .build()
        .expect("invalid backup config");

    // Must come before connecting to databases, running migrations or binding a port.
    backup.handle_cli().await;
    backup.start_scheduler();

    // ...start the app
}
```

`handle_cli()` exits the process when the arguments are `backup` or `restore ...`. Exit code 0 means success, 1 means failure, 2 means bad usage. Any other arguments make it return straight away. **Call it before anything that starts the app.** Otherwise `my-api backup` would also run migrations, or fail because the port is already in use.

### Builder reference

| Method | |
|---|---|
| `database(name, connection_string)` | Repeatable. Every schema of the database is included. `name` identifies the dump inside the archive. Connection strings are never stored in the archive. |
| `folder(name, path)` | Repeatable. A folder that does not exist yet is archived as empty. Registering one parent folder (the app's whole data directory) is fine, as long as `work_dir` is not inside it. |
| `s3(S3Target)` | Bucket and region are required. `profile` is a named profile in the service user's `~/.aws`. `prefix` defaults to the app name. |
| `notify(EmailNotify)` | Sent through SES with the same profile and region. Emails on failure only by default; add `.on_success(true)` to also email on success. `from` must be a verified SES identity. |
| `schedule_every(Duration)` | Interval for `start_scheduler()`. |
| `work_dir(path)` | Holds the dumps, downloads, the lock file and the last-backup record. Defaults to `/var/tmp`. **Must lie outside every registered folder**: the dumps are written here while the folders are streamed, so a work dir inside a registered folder ends up in the archive, and a later restore refuses because that folder is not empty. |
| `service_name(unit)` | The systemd unit that restore checks is stopped. Defaults to `<app>.service`. |
| `app_version(v)` | Recorded in the archive manifest. |

Names (app, databases, folders) may only use `A-Z a-z 0-9 - _`.

## Running the commands on a server

Run the commands as the service user and with the service's environment, so they use the same `DATABASE_URL` and `~/.aws` profile as the API. If the service loads its config from an `EnvironmentFile`, the simplest way is `systemd-run`:

```bash
sudo systemd-run --pipe --wait --collect -p User=my-api -p EnvironmentFile=/etc/my-api/env /usr/bin/my-api backup
```

If the app reads its config from a file instead, `sudo -u my-api -H /usr/bin/my-api backup` is enough.

### Backup before every upgrade

Put this in `debian/preinst` (cargo-deb `maintainer-scripts`). `preinst` runs before the new files are unpacked, so the backup is made by the version that is currently installed. If the backup fails, the upgrade is aborted.

```sh
#!/bin/sh
set -e
if [ "$1" = "upgrade" ]; then
    systemd-run --pipe --wait --collect -p User=my-api -p EnvironmentFile=/etc/my-api/env /usr/bin/my-api backup
fi
#DEBHELPER#
```

## What a backup does

1. Checks that the bucket can be reached. It does this before any dumping, so wrong credentials fail fast.
2. Takes a lock file in `work_dir`, so only one backup or restore runs at a time.
3. Dumps each database with `pg_dump --format=custom -Z0 --no-owner --no-privileges` into `work_dir`. The dumps are left uncompressed because the archive is compressed as a whole.
4. Streams the manifest, the dumps and the folders through tar and zstd (level 3, single thread) into an S3 multipart upload. Folders are never copied to disk first. If anything fails, the upload is aborted, so a half-written archive never appears in the bucket.
5. Uploads to `s3://<bucket>/<prefix>/<hostname>/<YYYY-MM-DDTHH-MM-SSZ>.tar.zst`. Nothing is ever overwritten or deleted.

The `backup` command lowers its own CPU and disk priority (nice 19, lowest best-effort IO priority) and its child processes inherit that, so a busy API keeps priority.

Memory use is about 80 MiB (16 MiB upload parts: 3 in flight, 2 being filled or queued). With 16 MiB parts a single archive can be up to about 160 GiB after compression. Disk use in `work_dir` is about the size of the uncompressed dumps.

Files deleted while a backup runs are skipped. The dumps and the folders are captured a few moments apart, so a file uploaded during the backup can be in the archive without its database row, or the other way round.

### Archive layout

```text
manifest.json            app, app version, host, time, pg_dump version, names
databases/<name>.dump    pg_dump custom format
folders/<name>/...       folder contents (symlinks kept as symlinks)
```

## Restore

```bash
sudo systemctl stop my-api
sudo systemd-run --pipe --wait --collect -p User=my-api -p EnvironmentFile=/etc/my-api/env /usr/bin/my-api restore --list
sudo systemd-run --pipe --wait --collect -p User=my-api -p EnvironmentFile=/etc/my-api/env /usr/bin/my-api restore 2026-10-25T03-00-00Z
```

Restore never overwrites anything. It refuses to run when:

- the systemd service is running,
- any registered database has user objects (tables, views, sequences, functions, types, extra schemas, extensions). Drop the database and create it again empty.
- any registered folder exists and is not empty. Clear it first.
- the archive belongs to a different app, or contains a database or folder this app does not register,
- the local `pg_restore` is older than the `pg_dump` that made the archive.

These checks run before the download. The archive is downloaded to `work_dir`, then:

1. Each database is restored with `pg_restore --single-transaction --exit-on-error --no-owner --no-privileges`.
2. The folders are unpacked, but only after every database has been restored successfully.

`--host` restores a backup made on another machine, for example when moving to a new server.

## Scheduler

`start_scheduler()` spawns a tokio task that sleeps until the next backup is due. Due means the last successful backup plus the interval; the time is recorded in `work_dir/<app>.last-backup`. App restarts therefore don't delay backups, and a manual backup also counts.

When a backup is due, the task runs `<current exe> backup` as a separate low-priority process. The backup never shares the API's memory or threads, and it behaves exactly like a manual run. The waiting task itself costs nothing noticeable.

- After a failed run, it retries after an hour, or after the interval if that is shorter.
- Without any recorded backup, the first run starts a minute after startup.

## AWS permissions

The profile needs:

- `s3:ListBucket` on the bucket
- `s3:PutObject`, `s3:GetObject` and `s3:AbortMultipartUpload` on `<bucket>/<prefix>/*`
- `ses:SendEmail` if notifications are used

Add a lifecycle rule on the bucket that aborts incomplete multipart uploads after a day or so. A backup killed mid-upload (for example by `systemctl stop`) cannot abort its own upload.

## Notes

- Do not set `PrivateTmp=` in the unit. With it, `/var/tmp` inside the service is not the same `/var/tmp` you see from a shell, so the lock file and the scheduler's last-backup record are not shared with manual `backup` runs. If the unit must keep `PrivateTmp=`, set `work_dir` to a directory that is **not inside any registered folder**, for example `/var/lib/my-api-backup` next to (not under) a registered `/var/lib/my-api`.
- Passwords in `postgres://` connection strings are passed to the Postgres tools through `PGPASSWORD`, not on the command line.
