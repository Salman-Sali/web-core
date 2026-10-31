use std::time::Duration;

use async_trait::async_trait;
use tokio::{net::TcpStream, process::Command, time::Instant};

/// A systemd unit a task type needs (forge: `clamav-daemon`), started when the runner accepts
/// work and stopped when it drains or exits, so an idle runner costs no RAM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// The systemd unit, e.g. `clamav-daemon.service`.
    pub unit: String,
    /// How to tell the service is ready after `systemctl start`.
    pub ready: ReadyCheck,
}

impl ServiceSpec {
    /// A service with a readiness check.
    pub fn new(unit: impl Into<String>, ready: ReadyCheck) -> Self {
        Self {
            unit: unit.into(),
            ready,
        }
    }
}

/// Readiness check of a [`ServiceSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyCheck {
    /// Ready as soon as `systemctl start` returns.
    None,
    /// Ready when `127.0.0.1:<port>` accepts a TCP connection.
    TcpPort(u16),
    /// Ready when this command (program + args) exits with status 0.
    Command(Vec<String>),
}

/// Starts and stops managed services. [`Systemctl`] is the real one; tests use
/// [`NoopServices`] or their own recording implementation.
#[async_trait]
pub trait ServiceManager: Send + Sync + 'static {
    /// Starts `spec` and waits until it is ready.
    async fn start(&self, spec: &ServiceSpec) -> Result<(), String>;
    /// Stops `spec`.
    async fn stop(&self, spec: &ServiceSpec) -> Result<(), String>;
}

/// Does nothing; for runners without services and for tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopServices;

#[async_trait]
impl ServiceManager for NoopServices {
    async fn start(&self, _spec: &ServiceSpec) -> Result<(), String> {
        Ok(())
    }

    async fn stop(&self, _spec: &ServiceSpec) -> Result<(), String> {
        Ok(())
    }
}

/// Runs `systemctl start|stop <unit>`, through `sudo -n` when the runner is not root (the
/// runner's sudoers rule allows exactly its units), then polls the readiness check.
#[derive(Debug, Clone)]
pub struct Systemctl {
    /// Prefix the command with `sudo -n`.
    pub sudo: bool,
    /// How long to wait for readiness.
    pub ready_timeout: Duration,
}

impl Default for Systemctl {
    fn default() -> Self {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        Self {
            sudo: !root,
            ready_timeout: Duration::from_secs(120),
        }
    }
}

impl Systemctl {
    async fn systemctl(&self, verb: &str, unit: &str) -> Result<(), String> {
        let mut cmd = if self.sudo {
            let mut c = Command::new("sudo");
            c.arg("-n").arg("systemctl");
            c
        } else {
            Command::new("systemctl")
        };
        let output = cmd
            .arg(verb)
            .arg(unit)
            .output()
            .await
            .map_err(|e| format!("systemctl {verb} {unit}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "systemctl {verb} {unit} failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

#[async_trait]
impl ServiceManager for Systemctl {
    async fn start(&self, spec: &ServiceSpec) -> Result<(), String> {
        self.systemctl("start", &spec.unit).await?;
        wait_ready(&spec.ready, self.ready_timeout)
            .await
            .map_err(|e| format!("{} not ready: {e}", spec.unit))
    }

    async fn stop(&self, spec: &ServiceSpec) -> Result<(), String> {
        self.systemctl("stop", &spec.unit).await
    }
}

/// Polls `check` every 500 ms until it passes or `timeout` elapses.
pub async fn wait_ready(check: &ReadyCheck, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let last = match check {
            ReadyCheck::None => return Ok(()),
            ReadyCheck::TcpPort(port) => match TcpStream::connect(("127.0.0.1", *port)).await {
                Ok(_) => return Ok(()),
                Err(e) => e.to_string(),
            },
            ReadyCheck::Command(argv) => {
                let Some((program, args)) = argv.split_first() else {
                    return Err("empty readiness command".into());
                };
                match Command::new(program).args(args).output().await {
                    Ok(out) if out.status.success() => return Ok(()),
                    Ok(out) => format!("exit status {}", out.status),
                    Err(e) => e.to_string(),
                }
            }
        };
        if Instant::now() >= deadline {
            return Err(last);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ready_checks() {
        assert!(wait_ready(&ReadyCheck::None, Duration::ZERO).await.is_ok());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert!(
            wait_ready(&ReadyCheck::TcpPort(port), Duration::ZERO)
                .await
                .is_ok()
        );
        drop(listener);
        assert!(
            wait_ready(&ReadyCheck::Command(vec!["true".into()]), Duration::ZERO)
                .await
                .is_ok()
        );
        assert!(
            wait_ready(&ReadyCheck::Command(vec!["false".into()]), Duration::ZERO)
                .await
                .is_err()
        );
    }
}
