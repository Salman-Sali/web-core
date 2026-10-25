use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use aws_config::SdkConfig;
use aws_sdk_s3::{Client, error::DisplayErrorContext};
use chrono::Utc;
use tokio::{sync::mpsc, task::spawn_blocking};

use crate::{
    archive::{self, Manifest},
    config::Config,
    error::{BackupError, Result},
    postgres,
    s3::{self, PartWriter},
    sys::{self, Lock, Staging},
};

pub(crate) struct Report {
    pub key: String,
    pub size: u64,
    pub elapsed: Duration,
}

pub(crate) async fn run(cfg: Arc<Config>, sdk: &SdkConfig) -> Result<Report> {
    let started = Instant::now();
    let started_at = Utc::now();
    let host = sys::hostname();
    let timestamp = sys::timestamp(started_at);
    let key = s3::object_key(&cfg.s3, &host, &timestamp);

    // fail before spending time on dumps if the bucket or credentials are wrong
    let client = Client::new(sdk);
    client
        .head_bucket()
        .bucket(&cfg.s3.bucket)
        .send()
        .await
        .map_err(|e| {
            BackupError::S3(format!(
                "bucket `{}` is not reachable: {}",
                cfg.s3.bucket,
                DisplayErrorContext(e)
            ))
        })?;

    let prepared = {
        let cfg = cfg.clone();
        let timestamp = timestamp.clone();
        spawn_blocking(move || -> Result<_> {
            let lock = Lock::acquire(&cfg.lock_path())?;
            let staging =
                Staging::create(cfg.work_dir.join(format!("{}-backup-{timestamp}", cfg.app)))?;
            let pg_dump_version = dump_databases(&cfg, &staging.path)?;
            Ok((lock, staging, pg_dump_version))
        })
    };
    let (_lock, staging, pg_dump_version) = prepared
        .await
        .map_err(|e| BackupError::Archive(format!("dump task failed: {e}")))??;

    let manifest = Manifest::new(
        cfg.app.clone(),
        cfg.app_version.clone(),
        host,
        started_at.to_rfc3339(),
        pg_dump_version,
        cfg.databases.iter().map(|d| d.name.clone()).collect(),
        cfg.folders.iter().map(|f| f.name.clone()).collect(),
    );
    let dumps: Vec<_> = cfg
        .databases
        .iter()
        .map(|d| (d.name.clone(), dump_path(&staging.path, &d.name)))
        .collect();
    let folders: Vec<_> = cfg
        .folders
        .iter()
        .map(|f| (f.name.clone(), f.path.clone()))
        .collect();

    println!("backup: uploading to s3://{}/{key}", cfg.s3.bucket);
    let (tx, rx) = mpsc::channel(1);
    let producer = spawn_blocking(move || -> Result<()> {
        archive::write(PartWriter::new(tx), &manifest, &dumps, &folders)?.finish()?;
        Ok(())
    });
    let size = s3::upload(&client, &cfg.s3.bucket, &key, rx, producer).await?;

    if let Err(e) = sys::write_last_success(&cfg.last_success_path(), started_at) {
        eprintln!("backup: could not record the backup time for the scheduler: {e}");
    }
    drop(staging);

    Ok(Report {
        key,
        size,
        elapsed: started.elapsed(),
    })
}

pub(crate) fn dump_path(dir: &Path, name: &str) -> std::path::PathBuf {
    dir.join(format!("{name}.dump"))
}

/// Dumps every registered database into `dir`. Returns the pg_dump version used.
pub(crate) fn dump_databases(cfg: &Config, dir: &Path) -> Result<Option<String>> {
    if cfg.databases.is_empty() {
        return Ok(None);
    }
    let version = postgres::tool_version("pg_dump")?;
    let major = postgres::tool_major("pg_dump")?;
    for db in &cfg.databases {
        let server = postgres::server_major(&db.conn).map_err(|e| for_database(&db.name, e))?;
        if major < server {
            return Err(BackupError::Postgres(format!(
                "pg_dump {major} cannot dump database `{}` on a Postgres {server} server; \
                 install postgresql-client-{server}",
                db.name
            )));
        }
    }
    for db in &cfg.databases {
        println!("backup: dumping database `{}`", db.name);
        postgres::dump(&db.conn, &dump_path(dir, &db.name))
            .map_err(|e| for_database(&db.name, e))?;
    }
    Ok(Some(version))
}

pub(crate) fn for_database(name: &str, e: BackupError) -> BackupError {
    match e {
        BackupError::Postgres(message) => {
            BackupError::Postgres(format!("database `{name}`: {message}"))
        }
        other => other,
    }
}
