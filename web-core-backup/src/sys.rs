//! Host-level helpers: locking, process priority, hostname, systemd and timestamps.

use std::{
    fs::{self, File, TryLockError},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::error::{BackupError, Result};

const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H-%M-%SZ";

/// Exclusive lock shared by backup and restore. Released when dropped.
pub(crate) struct Lock {
    _file: File,
}

impl Lock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = File::create(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(BackupError::Locked(path.to_path_buf())),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// A directory removed (with its contents) when dropped.
pub(crate) struct Staging {
    pub path: PathBuf,
}

impl Staging {
    pub fn create(path: PathBuf) -> Result<Self> {
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.path) {
            eprintln!("backup: could not remove {}: {e}", self.path.display());
        }
    }
}

/// Lowers CPU (nice 19) and IO (best-effort, lowest level) priority of every thread of this
/// process, so child processes such as pg_dump inherit it too. Linux sets both per thread,
/// and the async runtime's threads already exist by the time this runs.
#[cfg(target_os = "linux")]
pub(crate) fn lower_priority() {
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_BE: libc::c_long = 2;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
    const IOPRIO_LOWEST_LEVEL: libc::c_long = 7;

    let Ok(tasks) = fs::read_dir("/proc/self/task") else {
        return;
    };
    for task in tasks.flatten() {
        let Some(tid) = task
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<libc::id_t>().ok())
        else {
            continue;
        };
        // SAFETY: plain syscalls on our own thread ids; failures are ignored
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS as _, tid, 19);
            libc::syscall(
                libc::SYS_ioprio_set,
                IOPRIO_WHO_PROCESS,
                tid as libc::c_long,
                (IOPRIO_CLASS_BE << IOPRIO_CLASS_SHIFT) | IOPRIO_LOWEST_LEVEL,
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn lower_priority() {}

pub(crate) fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// `None` when systemd is not available, e.g. on a development machine.
pub(crate) fn service_is_active(unit: &str) -> Option<bool> {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .stdin(Stdio::null())
        .status()
        .ok()
        .map(|status| status.success())
}

/// Used in S3 keys: sortable and without `:`.
pub(crate) fn timestamp(at: DateTime<Utc>) -> String {
    at.format(TIMESTAMP_FORMAT).to_string()
}

pub(crate) fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, TIMESTAMP_FORMAT)
        .ok()
        .map(|t| t.and_utc())
}

pub(crate) fn read_last_success(path: &Path) -> Option<DateTime<Utc>> {
    let content = fs::read_to_string(path).ok()?;
    DateTime::parse_from_rfc3339(content.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

pub(crate) fn write_last_success(path: &Path, at: DateTime<Utc>) -> Result<()> {
    fs::write(path, at.to_rfc3339())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip() {
        let at = DateTime::parse_from_rfc3339("2026-10-25T03:04:05Z")
            .unwrap()
            .with_timezone(&Utc);
        let s = timestamp(at);
        assert_eq!(s, "2026-10-25T03-04-05Z");
        assert_eq!(parse_timestamp(&s), Some(at));
        assert_eq!(parse_timestamp("latest"), None);
    }

    #[test]
    fn lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.backup.lock");
        let lock = Lock::acquire(&path).unwrap();
        assert!(matches!(Lock::acquire(&path), Err(BackupError::Locked(_))));
        drop(lock);
        Lock::acquire(&path).unwrap();
    }
}
