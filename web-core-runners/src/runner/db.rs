use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use rusqlite::{Connection, params};

use crate::protocol::AttemptId;

const LOG_KEEP: i64 = 1000;

/// The runner's bookkeeping database (`<work dir>/runner.db`): in-flight attempts, work dirs to
/// clean and a local log. Never holds task content.
pub struct RunnerDb {
    conn: Mutex<Connection>,
}

impl RunnerDb {
    /// Opens (or creates) the database at `path`.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        Self::init(conn)
    }

    /// An in-memory database (tests).
    pub fn open_in_memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS in_flight (
                 attempt_id TEXT PRIMARY KEY,
                 task_type TEXT NOT NULL,
                 started_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE TABLE IF NOT EXISTS work_dirs (path TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS log (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 at TEXT NOT NULL DEFAULT (datetime('now')),
                 level TEXT NOT NULL,
                 message TEXT NOT NULL
             );",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.conn.lock().map_err(|_| "runner db lock poisoned")?;
        f(&conn).map_err(|e| e.to_string())
    }

    /// Boot cleanup (a restarted runner resumes nothing): removes every recorded work dir and
    /// `attempts_root`, and clears the in-flight table. Returns how many attempts were dropped.
    pub fn boot_cleanup(&self, attempts_root: &Path) -> Result<usize, String> {
        let dirs: Vec<String> = self.with(|c| {
            let mut stmt = c.prepare("SELECT path FROM work_dirs")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        })?;
        for dir in &dirs {
            remove_dir(Path::new(dir));
        }
        remove_dir(attempts_root);
        let dropped = self.with(|c| {
            let n = c.execute("DELETE FROM in_flight", [])?;
            c.execute("DELETE FROM work_dirs", [])?;
            Ok(n)
        })?;
        Ok(dropped)
    }

    /// Records an attempt as in flight, with its work dir.
    pub fn begin_attempt(
        &self,
        attempt: AttemptId,
        task_type: &str,
        work_dir: &Path,
    ) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO in_flight (attempt_id, task_type) VALUES (?1, ?2)",
                params![attempt.to_string(), task_type],
            )?;
            c.execute(
                "INSERT OR REPLACE INTO work_dirs (path) VALUES (?1)",
                params![work_dir.to_string_lossy()],
            )?;
            Ok(())
        })
    }

    /// Forgets an attempt and its work dir (the caller has removed the directory).
    pub fn end_attempt(&self, attempt: AttemptId, work_dir: &Path) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "DELETE FROM in_flight WHERE attempt_id = ?1",
                params![attempt.to_string()],
            )?;
            c.execute(
                "DELETE FROM work_dirs WHERE path = ?1",
                params![work_dir.to_string_lossy()],
            )?;
            Ok(())
        })
    }

    /// Attempts recorded as in flight.
    pub fn in_flight(&self) -> Result<Vec<AttemptId>, String> {
        let ids: Vec<String> = self.with(|c| {
            let mut stmt = c.prepare("SELECT attempt_id FROM in_flight")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        })?;
        Ok(ids.iter().filter_map(|s| s.parse().ok()).collect())
    }

    /// Recorded work dirs.
    pub fn work_dirs(&self) -> Result<Vec<PathBuf>, String> {
        let dirs: Vec<String> = self.with(|c| {
            let mut stmt = c.prepare("SELECT path FROM work_dirs")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        })?;
        Ok(dirs.into_iter().map(PathBuf::from).collect())
    }

    /// Appends a line to the local log, keeping the last 1000.
    pub fn log(&self, level: &str, message: &str) {
        let result = self.with(|c| {
            c.execute(
                "INSERT INTO log (level, message) VALUES (?1, ?2)",
                params![level, message],
            )?;
            c.execute(
                "DELETE FROM log WHERE id <= (SELECT MAX(id) FROM log) - ?1",
                params![LOG_KEEP],
            )?;
            Ok(())
        });
        if let Err(e) = result {
            tracing::warn!("runner log write failed: {e}");
        }
    }

    /// The last `limit` log lines, newest first, as `(at, level, message)`.
    pub fn recent_log(&self, limit: usize) -> Result<Vec<(String, String, String)>, String> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT at, level, message FROM log ORDER BY id DESC LIMIT ?1")?;
            let rows = stmt.query_map(params![limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect()
        })
    }
}

fn remove_dir(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("cannot remove {}: {e}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_cleanup_resumes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("attempts");
        let attempt = AttemptId::new();
        let dir = root.join(attempt.to_string());
        std::fs::create_dir_all(dir.join("in")).expect("mkdir");
        std::fs::write(dir.join("in/upload"), b"secret").expect("write");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).expect("mkdir");

        let db = RunnerDb::open(&tmp.path().join("runner.db")).expect("open");
        db.begin_attempt(attempt, "t", &dir).expect("begin");
        db.begin_attempt(AttemptId::new(), "t", &outside)
            .expect("begin");
        assert_eq!(db.in_flight().expect("list").len(), 2);
        drop(db);

        // simulated kill: reopen and boot
        let db = RunnerDb::open(&tmp.path().join("runner.db")).expect("reopen");
        assert_eq!(db.boot_cleanup(&root).expect("cleanup"), 2);
        assert!(db.in_flight().expect("list").is_empty());
        assert!(db.work_dirs().expect("dirs").is_empty());
        assert!(!dir.exists());
        assert!(!outside.exists());
    }

    #[test]
    fn log_is_capped() {
        let db = RunnerDb::open_in_memory().expect("open");
        for i in 0..(LOG_KEEP + 5) {
            db.log("info", &format!("line {i}"));
        }
        let lines = db.recent_log(5000).expect("log");
        assert_eq!(lines.len() as i64, LOG_KEEP);
        assert_eq!(lines[0].2, format!("line {}", LOG_KEEP + 4));
    }
}
