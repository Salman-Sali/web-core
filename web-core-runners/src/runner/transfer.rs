use std::{
    io::SeekFrom,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{Client, StatusCode, header};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt},
};
use tokio_util::io::ReaderStream;

use crate::{
    hash::sha256_file,
    protocol::{InputFile, OutputSlot, UPLOAD_OFFSET_HEADER, UploadedFile},
};

const TRIES: u32 = 5;

/// Downloads inputs and uploads outputs over the signed URLs of an assignment. Both directions
/// resume after a dropped connection (`Range` for downloads, `HEAD` + `Content-Range` for
/// uploads) and are verified by SHA-256.
#[derive(Clone)]
pub struct HttpTransfer {
    client: Client,
}

impl HttpTransfer {
    /// A transfer client with sensible timeouts (no total timeout: files can be large).
    pub fn new() -> Result<Self, String> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }

    /// The underlying client (also used for the self-update download).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Downloads `file` to `dest`, resuming a partial `dest.part`, and verifies size and hash.
    pub async fn download(&self, file: &InputFile, dest: &Path) -> Result<(), String> {
        let part = part_path(dest);
        let mut last = String::new();
        for attempt in 0..TRIES {
            if attempt > 0 {
                tokio::time::sleep(backoff(attempt)).await;
            }
            if let Err(e) = self.download_once(file, &part).await {
                last = e;
                continue;
            }
            let (sha256, size) = sha256_file(&part).await.map_err(|e| e.to_string())?;
            if sha256 == file.sha256 && size == file.size {
                return fs::rename(&part, dest).await.map_err(|e| e.to_string());
            }
            let _ = fs::remove_file(&part).await;
            last = format!(
                "{}: hash or size mismatch (got {size} bytes, {sha256})",
                file.slot
            );
        }
        Err(last)
    }

    async fn download_once(&self, file: &InputFile, part: &Path) -> Result<(), String> {
        let mut offset = fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
        if offset > file.size {
            let _ = fs::remove_file(part).await;
            offset = 0;
        }
        if offset == file.size && offset > 0 {
            return Ok(());
        }
        let mut request = self.client.get(&file.url);
        if offset > 0 {
            request = request.header(header::RANGE, format!("bytes={offset}-"));
        }
        let mut response = request.send().await.map_err(|e| e.to_string())?;
        let mut out = match response.status() {
            StatusCode::PARTIAL_CONTENT => OpenOptions::new()
                .append(true)
                .open(part)
                .await
                .map_err(|e| e.to_string())?,
            StatusCode::OK => fs::File::create(part).await.map_err(|e| e.to_string())?,
            status => return Err(format!("{}: download returned {status}", file.slot)),
        };
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        out.flush().await.map_err(|e| e.to_string())
    }

    /// Uploads `path` to `slot`, resuming from the offset the app reports.
    pub async fn upload(&self, slot: &OutputSlot, path: &Path) -> Result<UploadedFile, String> {
        let (sha256, size) = sha256_file(path).await.map_err(|e| e.to_string())?;
        let mut last = String::new();
        for attempt in 0..TRIES {
            if attempt > 0 {
                tokio::time::sleep(backoff(attempt)).await;
            }
            match self.upload_once(slot, path, size).await {
                Ok(()) => {
                    return Ok(UploadedFile {
                        slot: slot.slot.clone(),
                        sha256,
                        size,
                    });
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn upload_once(&self, slot: &OutputSlot, path: &Path, size: u64) -> Result<(), String> {
        let head = self
            .client
            .head(&slot.url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !head.status().is_success() {
            return Err(format!("{}: upload offset returned {}", slot.slot, head.status()));
        }
        let mut offset = head
            .headers()
            .get(UPLOAD_OFFSET_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        if offset > size {
            offset = 0;
        }
        if offset == size && size > 0 {
            return Ok(());
        }
        let mut file = fs::File::open(path).await.map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|e| e.to_string())?;
        let range = if size == 0 {
            "bytes */0".to_string()
        } else {
            format!("bytes {offset}-{}/{size}", size - 1)
        };
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
        let response = self
            .client
            .put(&slot.url)
            .header(header::CONTENT_RANGE, range)
            .header(header::CONTENT_LENGTH, size - offset)
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("{}: upload returned {}", slot.slot, response.status()))
        }
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 * 2u64.pow(attempt.min(5)))
}
