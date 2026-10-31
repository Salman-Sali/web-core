use std::{io::SeekFrom, path::Path as FsPath};

use axum::{
    Router,
    body::Body,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};
use tokio_util::io::ReaderStream;
use web_core::error::Error;

use super::pool::{FileAccess, RunnerPool, RunnerSession};
use crate::protocol::{
    AttemptId, BINARY_PATH, CONTRACT_HASH_HEADER, RunnerMessage, SHA256_HEADER,
    UPLOAD_OFFSET_HEADER, WS_PATH,
};

const MAX_MESSAGE: usize = 16 * 1024 * 1024;

impl RunnerPool {
    /// The runner routes, to merge into the app's router (outside its user auth layer; every
    /// route authenticates on its own):
    ///
    /// - `GET /runners/ws`: the runner WebSocket (`Authorization: Bearer <token>`).
    /// - `GET|HEAD|PUT /runners/files/{attempt}/{slot}`: signed, expiring file URLs.
    /// - `GET /runners/binary`: the runner binary (token-authenticated).
    pub fn router(&self) -> Router {
        Router::new()
            .route(WS_PATH, get(ws_handler))
            .route(
                "/runners/files/{attempt}/{slot}",
                get(download).head(upload_offset).put(upload),
            )
            .route(BINARY_PATH, get(binary))
            .with_state(self.clone())
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

async fn ws_handler(
    State(pool): State<RunnerPool>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Error> {
    let token = bearer(&headers).ok_or_else(|| Error::new_unauthorized("missing runner token"))?;
    let session = pool.attach(token).await?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .on_upgrade(move |socket| serve_socket(socket, session)))
}

async fn serve_socket(socket: WebSocket, mut session: RunnerSession) {
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            outgoing = session.recv() => match outgoing {
                Some(message) => {
                    let Ok(text) = serde_json::to_string(&message) else { continue };
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<RunnerMessage>(text.as_str()) {
                        Ok(message) => session.handle(message),
                        Err(e) => tracing::warn!("unreadable runner message: {e}"),
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

#[derive(Deserialize)]
struct Signed {
    dir: String,
    exp: u64,
    sig: String,
}

fn access(
    pool: &RunnerPool,
    attempt: &str,
    slot: &str,
    q: &Signed,
) -> Result<FileAccess, StatusCode> {
    let attempt: AttemptId = attempt.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    pool.file_access(attempt, slot, &q.dir, q.exp, &q.sig)
        .ok_or(StatusCode::FORBIDDEN)
}

/// `bytes=N-` or `bytes=N-M` → (start, inclusive end).
fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = match end.trim() {
        "" => size.checked_sub(1)?,
        e => e.parse::<u64>().ok()?.min(size.checked_sub(1)?),
    };
    (start <= end).then_some((start, end))
}

async fn download(
    State(pool): State<RunnerPool>,
    Path((attempt, slot)): Path<(String, String)>,
    Query(q): Query<Signed>,
    headers: HeaderMap,
) -> Response {
    let path = match access(&pool, &attempt, &slot, &q) {
        Ok(FileAccess::Download(path)) => path,
        Ok(FileAccess::Upload(_)) => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
        Err(status) => return status.into_response(),
    };
    serve_file(&path, headers.get(header::RANGE), &[]).await
}

async fn serve_file(
    path: &FsPath,
    range: Option<&HeaderValue>,
    extra: &[(&'static str, String)],
) -> Response {
    let mut file = match fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let size = match file.metadata().await {
        Ok(m) => m.len(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, "application/octet-stream");
    for (name, value) in extra {
        builder = builder.header(*name, value);
    }
    let range = range.and_then(|v| v.to_str().ok());
    let response = match range {
        Some(value) => {
            let Some((start, end)) = parse_range(value, size) else {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                    .body(Body::empty())
                    .unwrap_or_else(|_| StatusCode::RANGE_NOT_SATISFIABLE.into_response());
            };
            if file.seek(SeekFrom::Start(start)).await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            let len = end - start + 1;
            builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
                .header(header::CONTENT_LENGTH, len)
                .body(Body::from_stream(ReaderStream::new(file.take(len))))
        }
        None => builder
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, size)
            .body(Body::from_stream(ReaderStream::new(file))),
    };
    response.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn upload_offset(
    State(pool): State<RunnerPool>,
    Path((attempt, slot)): Path<(String, String)>,
    Query(q): Query<Signed>,
) -> Response {
    let part = match access(&pool, &attempt, &slot, &q) {
        Ok(FileAccess::Upload(part)) => part,
        Ok(FileAccess::Download(_)) => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
        Err(status) => return status.into_response(),
    };
    let offset = fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    (StatusCode::OK, [(UPLOAD_OFFSET_HEADER, offset.to_string())]).into_response()
}

/// `bytes A-B/T` → A; `bytes */T` → 0 (whole, empty file).
fn parse_content_range(value: &str) -> Option<u64> {
    let spec = value.strip_prefix("bytes ")?;
    if spec.starts_with('*') {
        return Some(0);
    }
    let (range, _total) = spec.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    start.trim().parse().ok()
}

async fn upload(
    State(pool): State<RunnerPool>,
    Path((attempt, slot)): Path<(String, String)>,
    Query(q): Query<Signed>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let part = match access(&pool, &attempt, &slot, &q) {
        Ok(FileAccess::Upload(part)) => part,
        Ok(FileAccess::Download(_)) => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
        Err(status) => return status.into_response(),
    };
    let start = match headers.get(header::CONTENT_RANGE) {
        Some(v) => match v.to_str().ok().and_then(parse_content_range) {
            Some(start) => start,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => 0,
    };
    if let Some(parent) = part.parent()
        && fs::create_dir_all(parent).await.is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let current = fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    let mut file = if start == 0 {
        fs::File::create(&part).await
    } else if start == current {
        OpenOptions::new().append(true).open(&part).await
    } else {
        return (
            StatusCode::CONFLICT,
            [(UPLOAD_OFFSET_HEADER, current.to_string())],
        )
            .into_response();
    }
    .map_err(|e| tracing::error!("cannot open {}: {e}", part.display()));
    let Ok(file) = file.as_mut() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            let _ = file.flush().await;
            return StatusCode::BAD_REQUEST.into_response();
        };
        if file.write_all(&chunk).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    if file.flush().await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn binary(State(pool): State<RunnerPool>, headers: HeaderMap) -> Result<Response, Error> {
    let token = bearer(&headers).ok_or_else(|| Error::new_unauthorized("missing runner token"))?;
    pool.authenticate(token).await?;
    let Some(info) = pool.inner.binary.clone() else {
        return Err(Error::new_not_found("No runner binary configured"));
    };
    Ok(serve_file(
        &info.path,
        None,
        &[
            (SHA256_HEADER, info.sha256),
            (CONTRACT_HASH_HEADER, pool.config().contract_hash.clone()),
        ],
    )
    .await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 10), Some((0, 9)));
        assert_eq!(parse_range("bytes=4-", 10), Some((4, 9)));
        assert_eq!(parse_range("bytes=2-5", 10), Some((2, 5)));
        assert_eq!(parse_range("bytes=2-50", 10), Some((2, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
        assert_eq!(parse_content_range("bytes 5-9/10"), Some(5));
        assert_eq!(parse_content_range("bytes */0"), Some(0));
        assert_eq!(parse_content_range("garbage"), None);
    }
}
