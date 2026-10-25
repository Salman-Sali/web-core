use std::{collections::HashMap, fs, io, path::Path, sync::Arc};

use aws_config::SdkConfig;
use aws_sdk_s3::Client;
use tokio::task::spawn_blocking;

use crate::{
    archive,
    backup::for_database,
    config::Config,
    error::{BackupError, Result},
    postgres, s3,
    sys::{self, Lock, Staging},
};

pub(crate) async fn list(cfg: &Config, sdk: &SdkConfig, host: &str) -> Result<Vec<String>> {
    s3::list(&Client::new(sdk), &cfg.s3, host).await
}

pub(crate) async fn run(
    cfg: Arc<Config>,
    sdk: &SdkConfig,
    timestamp: &str,
    host: &str,
) -> Result<()> {
    match sys::service_is_active(&cfg.service_name) {
        Some(true) => {
            return Err(BackupError::Refused(format!(
                "{0} is running; stop it first with `systemctl stop {0}`",
                cfg.service_name
            )));
        }
        Some(false) => {}
        None => eprintln!(
            "restore: systemctl is not available, so it cannot be checked whether {} is running",
            cfg.service_name
        ),
    }

    let prepared = {
        let cfg = cfg.clone();
        let timestamp = timestamp.to_string();
        spawn_blocking(move || -> Result<_> {
            let lock = Lock::acquire(&cfg.lock_path())?;
            check_targets_empty(&cfg)?;
            let staging = Staging::create(
                cfg.work_dir
                    .join(format!("{}-restore-{timestamp}", cfg.app)),
            )?;
            Ok((lock, staging))
        })
    };
    let (_lock, staging) = prepared
        .await
        .map_err(|e| BackupError::Archive(format!("restore task failed: {e}")))??;

    let key = s3::object_key(&cfg.s3, host, timestamp);
    println!("restore: downloading s3://{}/{key}", cfg.s3.bucket);
    let archive_path = staging.path.join("archive.tar.zst");
    let size = s3::download(&Client::new(sdk), &cfg.s3.bucket, &key, &archive_path).await?;
    println!(
        "restore: downloaded {:.1} MiB",
        size as f64 / (1024.0 * 1024.0)
    );

    let dump_dir = staging.path.clone();
    spawn_blocking(move || restore_archive(&cfg, &archive_path, &dump_dir))
        .await
        .map_err(|e| BackupError::Archive(format!("restore task failed: {e}")))??;
    drop(staging);
    Ok(())
}

/// Restore never overwrites: every registered database must have no user objects and
/// every registered folder must be missing or empty.
pub(crate) fn check_targets_empty(cfg: &Config) -> Result<()> {
    for db in &cfg.databases {
        let count = postgres::user_object_count(&db.conn).map_err(|e| for_database(&db.name, e))?;
        if count > 0 {
            return Err(BackupError::Refused(format!(
                "database `{}` is not empty ({count} objects); drop and recreate it empty first",
                db.name
            )));
        }
    }
    for folder in &cfg.folders {
        match fs::read_dir(&folder.path) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(BackupError::Refused(format!(
                        "folder `{}` ({}) is not empty; clear it first",
                        folder.name,
                        folder.path.display()
                    )));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(BackupError::Refused(format!(
                    "folder `{}` ({}) cannot be read: {e}",
                    folder.name,
                    folder.path.display()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn restore_archive(cfg: &Config, archive_path: &Path, dump_dir: &Path) -> Result<()> {
    let manifest = archive::read_manifest(archive_path)?;
    if manifest.app != cfg.app {
        return Err(BackupError::Refused(format!(
            "the archive belongs to app `{}`, not `{}`",
            manifest.app, cfg.app
        )));
    }
    for name in &manifest.databases {
        if !cfg.databases.iter().any(|d| &d.name == name) {
            return Err(BackupError::Refused(format!(
                "the archive contains database `{name}`, which this app does not register"
            )));
        }
    }
    for name in &manifest.folders {
        if !cfg.folders.iter().any(|f| &f.name == name) {
            return Err(BackupError::Refused(format!(
                "the archive contains folder `{name}`, which this app does not register"
            )));
        }
    }
    if !manifest.databases.is_empty()
        && let Some(dump_major) = manifest
            .pg_dump_version
            .as_deref()
            .and_then(postgres::parse_tool_major)
    {
        let restore_major = postgres::tool_major("pg_restore")?;
        if restore_major < dump_major {
            return Err(BackupError::Refused(format!(
                "the archive was dumped with pg_dump {dump_major}, but pg_restore here is {restore_major}; \
                 install postgresql-client-{dump_major}"
            )));
        }
    }
    for db in &cfg.databases {
        if !manifest.databases.contains(&db.name) {
            println!(
                "restore: database `{}` is not in this archive, skipped",
                db.name
            );
        }
    }
    for folder in &cfg.folders {
        if !manifest.folders.contains(&folder.name) {
            println!(
                "restore: folder `{}` is not in this archive, skipped",
                folder.name
            );
        }
    }

    println!(
        "restore: archive from {} created {} (app version {})",
        manifest.hostname,
        manifest.created_at,
        manifest.app_version.as_deref().unwrap_or("unknown")
    );

    let folders: HashMap<_, _> = cfg
        .folders
        .iter()
        .map(|f| (f.name.clone(), f.path.clone()))
        .collect();
    archive::extract(archive_path, dump_dir, &folders, |dumps| {
        for (name, path) in dumps {
            let db = cfg
                .databases
                .iter()
                .find(|d| &d.name == name)
                .ok_or_else(|| {
                    BackupError::Archive(format!("database `{name}` is not registered in this app"))
                })?;
            println!("restore: restoring database `{name}`");
            postgres::restore(&db.conn, path).map_err(|e| for_database(name, e))?;
            fs::remove_file(path)?;
        }
        if !folders.is_empty() {
            println!("restore: restoring folders");
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::{fs::File, process::Command};

    use super::*;
    use crate::{
        archive::Manifest,
        backup::{dump_databases, dump_path},
        config::{Backup, S3Target},
    };

    /// Runs against a real server when `WEB_CORE_BACKUP_TEST_PG` is set to a connection
    /// string (without a database) of a role that may create databases, e.g.
    /// `postgresql://postgres@localhost:5432`. Skipped otherwise.
    #[test]
    fn backs_up_and_restores_a_real_database() {
        let Ok(server) = std::env::var("WEB_CORE_BACKUP_TEST_PG") else {
            eprintln!("WEB_CORE_BACKUP_TEST_PG not set, skipped");
            return;
        };
        let server = server.trim_end_matches('/');
        let psql = |db: &str, sql: &str| {
            let output = Command::new("psql")
                .args(["-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", sql])
                .arg(format!("--dbname={server}/{db}"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        for db in ["wcb_source", "wcb_target"] {
            psql("postgres", &format!("DROP DATABASE IF EXISTS {db}"));
            psql("postgres", &format!("CREATE DATABASE {db}"));
        }
        psql(
            "wcb_source",
            "CREATE SCHEMA billing;
             CREATE TYPE billing.status AS ENUM ('open', 'paid');
             CREATE TABLE billing.invoices (id serial PRIMARY KEY, status billing.status NOT NULL);
             INSERT INTO billing.invoices (status) VALUES ('open'), ('paid'), ('paid');
             CREATE TABLE public.users (id serial PRIMARY KEY, email text UNIQUE);
             INSERT INTO public.users (email) SELECT 'user' || g || '@example.com' FROM generate_series(1, 500) g;
             CREATE FUNCTION public.user_count() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM public.users';",
        );

        let dir = tempfile::tempdir().unwrap();
        let uploads = dir.path().join("uploads");
        fs::create_dir_all(uploads.join("2026")).unwrap();
        fs::write(uploads.join("2026/avatar.png"), [1u8, 2, 3]).unwrap();
        let build = |db: &str, uploads: &Path| {
            Backup::builder("my-api")
                .database("main", format!("{server}/{db}"))
                .folder("uploads", uploads)
                .s3(S3Target::new("unused").region("eu-west-1"))
                .work_dir(dir.path().join("work"))
                .build()
                .unwrap()
        };

        let source = build("wcb_source", &uploads);
        let staging = dir.path().join("staging");
        fs::create_dir_all(&staging).unwrap();
        let pg_dump_version = dump_databases(&source.cfg, &staging).unwrap();
        let manifest = Manifest::new(
            "my-api".into(),
            None,
            "test-host".into(),
            chrono::Utc::now().to_rfc3339(),
            pg_dump_version,
            vec!["main".into()],
            vec!["uploads".into()],
        );
        let archive_path = dir.path().join("backup.tar.zst");
        archive::write(
            File::create(&archive_path).unwrap(),
            &manifest,
            &[("main".into(), dump_path(&staging, "main"))],
            &[("uploads".into(), uploads.clone())],
        )
        .unwrap();

        // the source is not empty, so restoring onto it is refused
        assert!(matches!(
            check_targets_empty(&source.cfg),
            Err(BackupError::Refused(_))
        ));

        let restored_uploads = dir.path().join("restored-uploads");
        let target = build("wcb_target", &restored_uploads);
        check_targets_empty(&target.cfg).unwrap();
        let dump_dir = dir.path().join("restore");
        fs::create_dir_all(&dump_dir).unwrap();
        restore_archive(&target.cfg, &archive_path, &dump_dir).unwrap();

        assert_eq!(
            psql(
                "wcb_target",
                "SELECT count(*) FROM billing.invoices WHERE status = 'paid'"
            ),
            "2"
        );
        assert_eq!(psql("wcb_target", "SELECT public.user_count()"), "500");
        assert_eq!(
            fs::read(restored_uploads.join("2026/avatar.png")).unwrap(),
            [1u8, 2, 3]
        );
        assert!(matches!(
            check_targets_empty(&target.cfg),
            Err(BackupError::Refused(_))
        ));

        for db in ["wcb_source", "wcb_target"] {
            psql("postgres", &format!("DROP DATABASE {db}"));
        }
    }
}
