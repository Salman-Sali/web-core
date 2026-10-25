use std::time::Duration;

use chrono::Utc;
use tokio::{process::Command, time::Instant};

use crate::{config::Backup, notify, s3, sys};

/// Gives the app time to finish starting up before an overdue backup kicks in.
const STARTUP_DELAY: Duration = Duration::from_secs(60);
/// After a failed run, wait at least this long (or the interval, if shorter) before retrying.
const RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

impl Backup {
    /// Starts a background task that runs `<current exe> backup` as a child process every
    /// [`schedule_every`](crate::BackupBuilder::schedule_every) interval, counted from the last
    /// successful backup. The child runs at low priority and in its own process, so the
    /// backup does not share the app's memory or threads. Does nothing without a schedule.
    ///
    /// Must be called inside a tokio runtime.
    pub fn start_scheduler(&self) {
        let Some(interval) = self.cfg.schedule else {
            return;
        };
        let this = self.clone();
        tokio::spawn(async move { this.schedule_loop(interval).await });
    }

    async fn schedule_loop(self, interval: Duration) {
        let retry = RETRY_DELAY.min(interval);
        let mut last_attempt: Option<Instant> = None;
        loop {
            let wait = match last_attempt {
                None => self.until_due(interval).max(STARTUP_DELAY),
                Some(at) => self
                    .until_due(interval)
                    .max(retry.saturating_sub(at.elapsed())),
            };
            tokio::time::sleep(wait).await;
            // a manual backup may have happened while sleeping
            if !self.until_due(interval).is_zero() {
                continue;
            }
            last_attempt = Some(Instant::now());
            self.run_child().await;
        }
    }

    fn until_due(&self, interval: Duration) -> Duration {
        let Some(last) = sys::read_last_success(&self.cfg.last_success_path()) else {
            return Duration::ZERO;
        };
        let due = last + interval;
        (due - Utc::now()).to_std().unwrap_or(Duration::ZERO)
    }

    async fn run_child(&self) {
        let failure = match std::env::current_exe() {
            Err(e) => Some(format!("could not find the app executable: {e}")),
            Ok(exe) => match Command::new(&exe).arg("backup").status().await {
                Ok(status) if status.success() => None,
                // the child reports its own failures, including the email
                Ok(status) if status.code().is_some() => {
                    eprintln!("backup: scheduled backup failed ({status})");
                    None
                }
                Ok(status) => Some(format!("the backup process was killed ({status})")),
                Err(e) => Some(format!("could not start {} backup: {e}", exe.display())),
            },
        };
        if let Some(failure) = failure {
            eprintln!("backup: scheduled backup failed: {failure}");
            let host = sys::hostname();
            let sdk = s3::sdk_config(&self.cfg.s3).await;
            let subject = format!("[{}] backup FAILED on {host}", self.cfg.app);
            notify::send_or_log(&sdk, &self.cfg, subject, failure).await;
        }
    }
}
