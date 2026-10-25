use std::{
    io::{self, Write},
    path::Path,
};

use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_s3::{
    Client,
    error::DisplayErrorContext,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
};
use tokio::{io::AsyncWriteExt, sync::mpsc, task::JoinSet};

use crate::{
    config::S3Config,
    error::{BackupError, Result},
};

/// 16 MiB parts allow archives up to ~160 GiB (S3 allows 10,000 parts per upload).
pub(crate) const PART_SIZE: usize = 16 * 1024 * 1024;
const MAX_PARTS: i32 = 10_000;
const CONCURRENT_PARTS: usize = 3;
pub(crate) const ARCHIVE_SUFFIX: &str = ".tar.zst";

pub(crate) async fn sdk_config(s3: &S3Config) -> SdkConfig {
    let mut loader =
        aws_config::defaults(BehaviorVersion::latest()).region(Region::new(s3.region.clone()));
    if let Some(profile) = &s3.profile {
        loader = loader.profile_name(profile);
    }
    loader.load().await
}

pub(crate) fn host_prefix(s3: &S3Config, host: &str) -> String {
    format!("{}/{host}/", s3.prefix)
}

pub(crate) fn object_key(s3: &S3Config, host: &str, timestamp: &str) -> String {
    format!("{}{timestamp}{ARCHIVE_SUFFIX}", host_prefix(s3, host))
}

fn s3_error(what: &str, e: impl std::error::Error) -> BackupError {
    BackupError::S3(format!("{what}: {}", DisplayErrorContext(e)))
}

/// Blocking writer that cuts the stream into upload parts and hands them to [`upload`].
pub(crate) struct PartWriter {
    tx: mpsc::Sender<Vec<u8>>,
    buffer: Vec<u8>,
    sent_any: bool,
}

impl PartWriter {
    pub fn new(tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            tx,
            buffer: Vec::with_capacity(PART_SIZE),
            sent_any: false,
        }
    }

    fn send(&mut self) -> io::Result<()> {
        let part = std::mem::replace(&mut self.buffer, Vec::with_capacity(PART_SIZE));
        self.sent_any = true;
        self.tx
            .blocking_send(part)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "upload stopped"))
    }

    /// Sends the last (possibly short) part.
    pub fn finish(mut self) -> io::Result<()> {
        if !self.buffer.is_empty() || !self.sent_any {
            self.send()?;
        }
        Ok(())
    }
}

impl Write for PartWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(PART_SIZE - self.buffer.len());
        self.buffer.extend_from_slice(&buf[..n]);
        if self.buffer.len() == PART_SIZE {
            self.send()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Multipart-uploads the parts received on `parts`. The upload is only completed if
/// `producer` (the task writing the archive) succeeds; otherwise it is aborted, so a
/// half-written archive never shows up in the bucket. Returns the uploaded size.
pub(crate) async fn upload(
    client: &Client,
    bucket: &str,
    key: &str,
    mut parts: mpsc::Receiver<Vec<u8>>,
    producer: tokio::task::JoinHandle<Result<()>>,
) -> Result<u64> {
    let created = client
        .create_multipart_upload()
        .bucket(bucket)
        .key(key)
        .content_type("application/zstd")
        .send()
        .await
        .map_err(|e| s3_error("could not start upload", e))?;
    let upload_id = created
        .upload_id()
        .ok_or_else(|| BackupError::S3("S3 returned no upload id".into()))?
        .to_string();

    let result = async {
        let mut completed = Vec::new();
        let mut in_flight = JoinSet::new();
        let mut part_number = 0;
        let mut size = 0u64;

        while let Some(part) = parts.recv().await {
            part_number += 1;
            if part_number > MAX_PARTS {
                return Err(BackupError::S3(format!(
                    "archive is larger than {} GiB, the most one upload can hold",
                    PART_SIZE as u64 * MAX_PARTS as u64 / (1024 * 1024 * 1024)
                )));
            }
            size += part.len() as u64;
            while in_flight.len() >= CONCURRENT_PARTS {
                completed.push(join_part(in_flight.join_next().await)?);
            }
            let request = client
                .upload_part()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .body(ByteStream::from(part));
            in_flight.spawn(async move {
                let response = request
                    .send()
                    .await
                    .map_err(|e| s3_error(&format!("could not upload part {part_number}"), e))?;
                Ok(CompletedPart::builder()
                    .part_number(part_number)
                    .set_e_tag(response.e_tag().map(str::to_string))
                    .build())
            });
        }
        while let Some(joined) = in_flight.join_next().await {
            completed.push(join_part(Some(joined))?);
        }

        producer
            .await
            .map_err(|e| BackupError::Archive(format!("archive task failed: {e}")))??;

        completed.sort_by_key(|p| p.part_number());
        client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(completed))
                    .build(),
            )
            .send()
            .await
            .map_err(|e| s3_error("could not complete upload", e))?;
        Ok(size)
    }
    .await;

    if result.is_err() {
        // dropping the receiver stops the archive writer if it is still running
        drop(parts);
        if let Err(e) = client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .send()
            .await
        {
            eprintln!(
                "backup: could not abort multipart upload {upload_id}: {}",
                DisplayErrorContext(e)
            );
        }
    }
    result
}

fn join_part(
    joined: Option<std::result::Result<Result<CompletedPart>, tokio::task::JoinError>>,
) -> Result<CompletedPart> {
    joined
        .expect("join_next is only called with parts in flight")
        .map_err(|e| BackupError::S3(format!("upload task failed: {e}")))?
}

/// Timestamps of the archives stored for `host`, oldest first.
pub(crate) async fn list(client: &Client, s3: &S3Config, host: &str) -> Result<Vec<String>> {
    let prefix = host_prefix(s3, host);
    let mut pages = client
        .list_objects_v2()
        .bucket(&s3.bucket)
        .prefix(&prefix)
        .into_paginator()
        .send();
    let mut timestamps = Vec::new();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|e| s3_error("could not list backups", e))?;
        for object in page.contents() {
            if let Some(timestamp) = object
                .key()
                .and_then(|k| k.strip_prefix(&prefix))
                .and_then(|k| k.strip_suffix(ARCHIVE_SUFFIX))
            {
                timestamps.push(timestamp.to_string());
            }
        }
    }
    timestamps.sort();
    Ok(timestamps)
}

pub(crate) async fn download(
    client: &Client,
    bucket: &str,
    key: &str,
    target: &Path,
) -> Result<u64> {
    let object = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| s3_error(&format!("could not download {key}"), e))?;
    let mut body = object.body;
    let mut file = tokio::fs::File::create(target).await?;
    let mut size = 0u64;
    while let Some(chunk) = body
        .try_next()
        .await
        .map_err(|e| s3_error(&format!("download of {key} failed"), e))?
    {
        size += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(size)
}
