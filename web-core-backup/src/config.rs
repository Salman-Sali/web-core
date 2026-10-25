use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use crate::{
    error::{BackupError, Result},
    postgres::{self, PgConn},
};

/// Where backups are uploaded.
pub struct S3Target {
    bucket: String,
    region: Option<String>,
    profile: Option<String>,
    prefix: Option<String>,
}

impl S3Target {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            region: None,
            profile: None,
            prefix: None,
        }
    }

    /// AWS region of the bucket. Required. Also used for SES notifications.
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Named profile from the service user's `~/.aws/config` / `~/.aws/credentials`.
    /// Without it the default AWS credential chain is used.
    pub fn profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
    }

    /// Key prefix inside the bucket. Defaults to the app name.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }
}

/// Email notification over SES, using the same AWS profile and region as [`S3Target`].
pub struct EmailNotify {
    pub(crate) from: String,
    pub(crate) to: Vec<String>,
    pub(crate) on_success: bool,
}

impl EmailNotify {
    /// `from` must be an SES verified identity. Sent on failure only, unless [`EmailNotify::on_success`] is set.
    pub fn new<I, S>(from: impl Into<String>, to: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            from: from.into(),
            to: to.into_iter().map(Into::into).collect(),
            on_success: false,
        }
    }

    pub fn on_success(mut self, on_success: bool) -> Self {
        self.on_success = on_success;
        self
    }
}

pub(crate) struct Database {
    pub name: String,
    pub conn: PgConn,
}

pub(crate) struct Folder {
    pub name: String,
    pub path: PathBuf,
}

pub(crate) struct S3Config {
    pub bucket: String,
    pub region: String,
    pub profile: Option<String>,
    pub prefix: String,
}

pub(crate) struct Config {
    pub app: String,
    pub app_version: Option<String>,
    pub databases: Vec<Database>,
    pub folders: Vec<Folder>,
    pub s3: S3Config,
    pub notify: Option<EmailNotify>,
    pub schedule: Option<Duration>,
    pub work_dir: PathBuf,
    pub service_name: String,
}

impl Config {
    pub fn lock_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}.backup.lock", self.app))
    }

    pub fn last_success_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}.last-backup", self.app))
    }
}

/// A registered backup. Cheap to clone.
#[derive(Clone)]
pub struct Backup {
    pub(crate) cfg: Arc<Config>,
}

impl Backup {
    /// `app` names the backup in S3 and on disk; usually the binary/package name.
    pub fn builder(app: impl Into<String>) -> BackupBuilder {
        BackupBuilder {
            app: app.into(),
            app_version: None,
            databases: Vec::new(),
            folders: Vec::new(),
            s3: None,
            notify: None,
            schedule: None,
            work_dir: PathBuf::from("/var/tmp"),
            service_name: None,
        }
    }
}

pub struct BackupBuilder {
    app: String,
    app_version: Option<String>,
    databases: Vec<(String, String)>,
    folders: Vec<(String, PathBuf)>,
    s3: Option<S3Target>,
    notify: Option<EmailNotify>,
    schedule: Option<Duration>,
    work_dir: PathBuf,
    service_name: Option<String>,
}

impl BackupBuilder {
    /// Recorded in the archive manifest, e.g. `env!("CARGO_PKG_VERSION")`.
    pub fn app_version(mut self, version: impl Into<String>) -> Self {
        self.app_version = Some(version.into());
        self
    }

    /// A Postgres database to back up. `name` identifies it inside the archive so restore
    /// knows where each dump goes; the connection string is never stored in the archive.
    pub fn database(
        mut self,
        name: impl Into<String>,
        connection_string: impl Into<String>,
    ) -> Self {
        self.databases.push((name.into(), connection_string.into()));
        self
    }

    /// A folder to back up. `name` identifies it inside the archive.
    pub fn folder(mut self, name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.folders.push((name.into(), path.into()));
        self
    }

    pub fn s3(mut self, target: S3Target) -> Self {
        self.s3 = Some(target);
        self
    }

    pub fn notify(mut self, notify: EmailNotify) -> Self {
        self.notify = Some(notify);
        self
    }

    /// Run a backup this often from [`Backup::start_scheduler`]. Counted from the last
    /// successful backup, so app restarts do not push it back.
    pub fn schedule_every(mut self, interval: Duration) -> Self {
        self.schedule = Some(interval);
        self
    }

    /// Where database dumps, downloads and the lock file live. Defaults to `/var/tmp`.
    /// Needs free space of roughly the size of the uncompressed database dumps
    /// (plus the archive size during restore).
    pub fn work_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.work_dir = path.into();
        self
    }

    /// systemd unit that `restore` refuses to run alongside. Defaults to `<app>.service`.
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = Some(name.into());
        self
    }

    pub fn build(self) -> Result<Backup> {
        validate_name("app", &self.app)?;
        if self.databases.is_empty() && self.folders.is_empty() {
            return Err(BackupError::Config(
                "register at least one database or folder".into(),
            ));
        }

        let mut seen = HashSet::new();
        for (name, _) in &self.databases {
            validate_name("database", name)?;
            if !seen.insert(name) {
                return Err(BackupError::Config(format!(
                    "database name `{name}` is used twice"
                )));
            }
        }
        let mut seen = HashSet::new();
        for (name, _) in &self.folders {
            validate_name("folder", name)?;
            if !seen.insert(name) {
                return Err(BackupError::Config(format!(
                    "folder name `{name}` is used twice"
                )));
            }
        }

        let s3 = self
            .s3
            .ok_or_else(|| BackupError::Config("an S3 target is required".into()))?;
        if s3.bucket.is_empty() {
            return Err(BackupError::Config("S3 bucket name is empty".into()));
        }
        let region = s3
            .region
            .ok_or_else(|| BackupError::Config("S3 region is required".into()))?;
        let prefix = s3
            .prefix
            .map(|p| p.trim_matches('/').to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| self.app.clone());

        if let Some(notify) = &self.notify
            && (notify.from.is_empty() || notify.to.is_empty())
        {
            return Err(BackupError::Config(
                "email notification needs a from address and at least one recipient".into(),
            ));
        }

        if self.schedule.is_some_and(|d| d.is_zero()) {
            return Err(BackupError::Config(
                "schedule interval must be greater than zero".into(),
            ));
        }

        if !self.databases.is_empty() {
            for tool in ["pg_dump", "pg_restore", "psql"] {
                postgres::tool_major(tool)?;
            }
        }

        let service_name = self
            .service_name
            .unwrap_or_else(|| format!("{}.service", self.app));

        Ok(Backup {
            cfg: Arc::new(Config {
                app: self.app,
                app_version: self.app_version,
                databases: self
                    .databases
                    .into_iter()
                    .map(|(name, url)| Database {
                        name,
                        conn: PgConn::parse(&url),
                    })
                    .collect(),
                folders: self
                    .folders
                    .into_iter()
                    .map(|(name, path)| Folder { name, path })
                    .collect(),
                s3: S3Config {
                    bucket: s3.bucket,
                    region,
                    profile: s3.profile,
                    prefix,
                },
                notify: self.notify,
                schedule: self.schedule,
                work_dir: self.work_dir,
                service_name,
            }),
        })
    }
}

/// Names end up in file names, S3 keys and archive paths.
fn validate_name(kind: &str, name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(BackupError::Config(format!(
            "{kind} name `{name}` must be 1-64 characters of A-Z, a-z, 0-9, `-` or `_`"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> BackupBuilder {
        Backup::builder("my-api")
            .folder("uploads", "/tmp/uploads")
            .s3(S3Target::new("bucket").region("eu-west-1"))
    }

    #[test]
    fn builds_with_defaults() {
        let backup = base().build().unwrap();
        assert_eq!(backup.cfg.s3.prefix, "my-api");
        assert_eq!(backup.cfg.service_name, "my-api.service");
        assert_eq!(backup.cfg.work_dir, PathBuf::from("/var/tmp"));
    }

    #[test]
    fn trims_prefix_slashes() {
        let backup = Backup::builder("my-api")
            .folder("uploads", "/tmp/uploads")
            .s3(S3Target::new("bucket")
                .region("eu-west-1")
                .prefix("/backups/api/"))
            .build()
            .unwrap();
        assert_eq!(backup.cfg.s3.prefix, "backups/api");
    }

    #[test]
    fn rejects_bad_config() {
        let cases = [
            Backup::builder("my api")
                .folder("uploads", "/tmp")
                .s3(S3Target::new("b").region("r")),
            Backup::builder("my-api").s3(S3Target::new("b").region("r")),
            base().folder("uploads", "/tmp/other"),
            base().folder("../etc", "/tmp/other"),
            Backup::builder("my-api").folder("uploads", "/tmp"),
            Backup::builder("my-api")
                .folder("uploads", "/tmp")
                .s3(S3Target::new("b")),
            base().notify(EmailNotify::new("from@example.com", Vec::<String>::new())),
            base().schedule_every(Duration::ZERO),
        ];
        for builder in cases {
            assert!(matches!(builder.build(), Err(BackupError::Config(_))));
        }
    }
}
