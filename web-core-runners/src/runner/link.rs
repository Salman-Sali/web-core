use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

use crate::protocol::{AppMessage, RunnerMessage, WS_PATH};

/// One live connection to the app.
#[async_trait]
pub trait Link: Send {
    /// Sends a message; an error means the connection is gone.
    async fn send(&mut self, message: RunnerMessage) -> Result<(), String>;
    /// The next message; `None` when the connection closed. Must be cancel-safe.
    async fn recv(&mut self) -> Option<AppMessage>;
}

/// Opens connections to the app (WebSocket for real, channels in tests).
#[async_trait]
pub trait Connector: Send + Sync + 'static {
    /// Connects and authenticates.
    async fn connect(&self) -> Result<Box<dyn Link>, String>;
}

/// Connects to `<app url>/runners/ws` with `Authorization: Bearer <token>`.
#[derive(Debug, Clone)]
pub struct WsConnector {
    url: String,
    token: String,
}

impl WsConnector {
    /// A connector for the app at `app_url` (`http(s)://host[:port]`).
    pub fn new(app_url: &str, token: impl Into<String>) -> Self {
        Self {
            url: ws_url(app_url),
            token: token.into(),
        }
    }
}

/// `http(s)://host/base` → `ws(s)://host/base/runners/ws`.
pub fn ws_url(app_url: &str) -> String {
    let base = app_url.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}{WS_PATH}")
}

#[async_trait]
impl Connector for WsConnector {
    async fn connect(&self) -> Result<Box<dyn Link>, String> {
        let mut request = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| e.to_string())?;
        let auth = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| e.to_string())?;
        request.headers_mut().insert("authorization", auth);
        let (stream, _) = connect_async(request).await.map_err(|e| e.to_string())?;
        Ok(Box::new(WsLink { stream }))
    }
}

struct WsLink {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

#[async_trait]
impl Link for WsLink {
    async fn send(&mut self, message: RunnerMessage) -> Result<(), String> {
        let text = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        self.stream
            .send(Message::text(text))
            .await
            .map_err(|e| e.to_string())
    }

    async fn recv(&mut self) -> Option<AppMessage> {
        loop {
            match self.stream.next().await? {
                Ok(Message::Text(text)) => match serde_json::from_str(text.as_str()) {
                    Ok(message) => return Some(message),
                    Err(e) => tracing::warn!("unreadable message from app: {e}"),
                },
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_urls() {
        assert_eq!(
            ws_url("https://api.example.com/"),
            "wss://api.example.com/runners/ws"
        );
        assert_eq!(
            ws_url("http://127.0.0.1:8080"),
            "ws://127.0.0.1:8080/runners/ws"
        );
    }
}
