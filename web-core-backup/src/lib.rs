//! Backs up Postgres databases and storage folders of an app into one `.tar.zst` archive on
//! S3, and restores it. The app's own binary gets two subcommands:
//!
//! ```text
//! my-api backup
//! my-api restore --list [--host <hostname>]
//! my-api restore <timestamp> [--host <hostname>]
//! ```
//!
//! ```no_run
//! use std::time::Duration;
//! use web_core_backup::{Backup, EmailNotify, S3Target};
//!
//! #[tokio::main]
//! async fn main() {
//!     let backup = Backup::builder("my-api")
//!         .app_version(env!("CARGO_PKG_VERSION"))
//!         .database("main", std::env::var("DATABASE_URL").unwrap())
//!         .folder("uploads", "/var/lib/my-api/uploads")
//!         .s3(S3Target::new("my-backups").region("eu-west-1").profile("backup"))
//!         .notify(EmailNotify::new("backups@example.com", ["ops@example.com"]))
//!         .schedule_every(Duration::from_secs(24 * 60 * 60))
//!         .build()
//!         .expect("invalid backup config");
//!
//!     // before connecting to databases, running migrations or binding a port
//!     backup.handle_cli().await;
//!     backup.start_scheduler();
//!
//!     // ... start the app
//! }
//! ```
//!
//! `pg_dump`, `pg_restore` and `psql` must be installed; add `postgresql-client` to the
//! package's `depends`.

mod archive;
mod backup;
mod cli;
mod config;
mod error;
mod notify;
mod postgres;
mod restore;
mod s3;
mod scheduler;
mod sys;

pub use config::{Backup, BackupBuilder, EmailNotify, S3Target};
pub use error::{BackupError, Result};
