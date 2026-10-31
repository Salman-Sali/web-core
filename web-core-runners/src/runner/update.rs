use std::{os::unix::fs::PermissionsExt, os::unix::process::CommandExt, path::PathBuf};

use async_trait::async_trait;
use reqwest::{Client, header};
use tokio::{fs, io::AsyncWriteExt};

use crate::hash::sha256_file;

/// Replaces the runner binary with the app's current one and restarts into it.
#[async_trait]
pub trait SelfUpdater: Send + Sync + 'static {
    /// Downloads `binary_url` (authenticated with `token`), verifies `sha256`, swaps the
    /// executable and re-executes. Returns only on failure (or, for test updaters, after
    /// recording the call).
    async fn update(&self, binary_url: &str, sha256: &str, token: &str) -> Result<(), String>;
}

/// The real updater: download next to the executable, verify, `chmod 755`, atomic rename over
/// the executable, `exec` with the same arguments. The executable's directory must be writable
/// by the runner's user.
#[derive(Clone)]
pub struct ExecUpdater {
    client: Client,
}

impl ExecUpdater {
    /// An updater using `client` for the download.
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl SelfUpdater for ExecUpdater {
    async fn update(&self, binary_url: &str, sha256: &str, token: &str) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
        let dir = exe
            .parent()
            .ok_or("executable has no parent directory")?
            .to_path_buf();
        let name = exe
            .file_name()
            .ok_or("executable has no file name")?
            .to_string_lossy()
            .into_owned();
        let tmp: PathBuf = dir.join(format!(".{name}.new"));

        let mut response = self
            .client
            .get(binary_url)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| format!("download: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("download returned {}", response.status()));
        }
        let mut out = fs::File::create(&tmp)
            .await
            .map_err(|e| format!("create {}: {e}", tmp.display()))?;
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        out.flush().await.map_err(|e| e.to_string())?;
        drop(out);

        let (actual, _) = sha256_file(&tmp).await.map_err(|e| e.to_string())?;
        if actual != sha256 {
            let _ = fs::remove_file(&tmp).await;
            return Err(format!("hash mismatch: expected {sha256}, got {actual}"));
        }
        fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &exe)
            .await
            .map_err(|e| format!("replace {}: {e}", exe.display()))?;

        tracing::info!("runner binary updated, restarting");
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        let err = std::process::Command::new(&exe).args(args).exec();
        Err(format!("exec {}: {err}", exe.display()))
    }
}

/// Never updates; used in local mode and by tests (records nothing).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoUpdate;

#[async_trait]
impl SelfUpdater for NoUpdate {
    async fn update(&self, _binary_url: &str, _sha256: &str, _token: &str) -> Result<(), String> {
        Err("self-update disabled".into())
    }
}
