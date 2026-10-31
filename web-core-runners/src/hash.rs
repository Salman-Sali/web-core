//! SHA-256 of files, used for manifests (app) and transfer verification (runner).

use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

/// SHA-256 (lowercase hex) and size of the file at `path`, read on the calling thread.
pub fn sha256_file_blocking(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), size))
}

/// SHA-256 (lowercase hex) and size of the file at `path`, read on a blocking thread.
pub async fn sha256_file(path: impl Into<PathBuf>) -> io::Result<(String, u64)> {
    let path = path.into();
    tokio::task::spawn_blocking(move || sha256_file_blocking(&path))
        .await
        .map_err(io::Error::other)?
}
