//! WebSocket support for Rapina.
//!
//! Gated behind the `websocket` feature flag. Provides a `WebSocketUpgrade`
//! extractor that completes the HTTP upgrade handshake and hands you a
//! bidirectional `WebSocket` connection.
//!
//! ```rust,ignore
//! use rapina::prelude::*;
//! use rapina::websocket::{WebSocketUpgrade, WebSocket, Message};
//!
//! #[get("/ws")]
//! #[public]
//! async fn ws(upgrade: WebSocketUpgrade) -> impl IntoResponse {
//!     upgrade.on_upgrade(|mut socket| async move {
//!         while let Some(Ok(msg)) = socket.recv().await {
//!             if msg.is_text() || msg.is_binary() {
//!                 socket.send(msg).await.ok();
//!             }
//!         }
//!     })
//! }
//! ```

use std::future::Future;
use std::sync::Arc;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use http::Response;
use hyper::Request;
use hyper::body::Incoming;
use hyper::upgrade::Upgraded;
use hyper_tungstenite::HyperWebsocket;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite;

use crate::error::Error;
use crate::extract::FromRequest;
use crate::extract::PathParams;
use crate::response::BoxBody;
use crate::state::AppState;

type WsStream = WebSocketStream<TokioIo<Upgraded>>;

/// An HTTP-to-WebSocket upgrade extracted from an incoming request.
///
/// Call [`on_upgrade`](Self::on_upgrade) with a callback to handle the
/// connection. The returned response completes the 101 handshake.
pub struct WebSocketUpgrade {
    response: Response<BoxBody>,
    websocket: HyperWebsocket,
}

impl FromRequest for WebSocketUpgrade {
    async fn from_request(
        req: Request<Incoming>,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let mut req = req;
        if !hyper_tungstenite::is_upgrade_request(&req) {
            return Err(Error::bad_request("Not a WebSocket upgrade request"));
        }
        let (response, websocket) = hyper_tungstenite::upgrade(&mut req, None)
            .map_err(|e| Error::internal(format!("WebSocket upgrade failed: {e}")))?;
        let (parts, _) = response.into_parts();
        let response = Response::from_parts(parts, crate::response::empty());
        Ok(Self {
            response,
            websocket,
        })
    }
}

impl WebSocketUpgrade {
    /// Completes the upgrade. Spawns `callback` once the handshake finishes
    /// and returns the 101 Switching Protocols response.
    pub fn on_upgrade<F, Fut>(self, callback: F) -> Response<BoxBody>
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let websocket = self.websocket;
        tokio::spawn(async move {
            match websocket.await {
                Ok(stream) => callback(WebSocket { inner: stream }).await,
                Err(e) => tracing::error!("WebSocket upgrade failed: {e}"),
            }
        });
        self.response
    }
}

/// A live WebSocket connection.
pub struct WebSocket {
    inner: WsStream,
}

impl WebSocket {
    /// Receive the next message, or `None` if the peer closed.
    pub async fn recv(&mut self) -> Option<Result<Message, Error>> {
        self.inner.next().await.map(|r| {
            r.map(Message::from_tungstenite)
                .map_err(|e| Error::internal(format!("WebSocket recv error: {e}")))
        })
    }

    /// Send a message.
    pub async fn send(&mut self, msg: Message) -> Result<(), Error> {
        self.inner
            .send(msg.into_tungstenite())
            .await
            .map_err(|e| Error::internal(format!("WebSocket send error: {e}")))
    }

    /// Send a close frame and flush.
    pub async fn close(mut self) -> Result<(), Error> {
        self.inner
            .send(tungstenite::Message::Close(None))
            .await
            .map_err(|e| Error::internal(format!("WebSocket close error: {e}")))
    }

    /// Split into independent sender and receiver halves.
    pub fn split(self) -> (WsSender, WsReceiver) {
        let (sink, stream) = self.inner.split();
        (WsSender { inner: sink }, WsReceiver { inner: stream })
    }
}

/// Write half of a split WebSocket.
pub struct WsSender {
    inner: SplitSink<WsStream, tungstenite::Message>,
}

impl WsSender {
    /// Send a message.
    pub async fn send(&mut self, msg: Message) -> Result<(), Error> {
        self.inner
            .send(msg.into_tungstenite())
            .await
            .map_err(|e| Error::internal(format!("WebSocket send error: {e}")))
    }
}

/// Read half of a split WebSocket.
pub struct WsReceiver {
    inner: SplitStream<WsStream>,
}

impl WsReceiver {
    /// Receive the next message, or `None` if the peer closed.
    pub async fn recv(&mut self) -> Option<Result<Message, Error>> {
        self.inner.next().await.map(|r| {
            r.map(Message::from_tungstenite)
                .map_err(|e| Error::internal(format!("WebSocket recv error: {e}")))
        })
    }
}

/// A WebSocket message.
#[derive(Debug, Clone)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(Option<CloseFrame>),
}

/// Close frame payload.
#[derive(Debug, Clone)]
pub struct CloseFrame {
    pub code: u16,
    pub reason: String,
}

impl Message {
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, Self::Binary(_))
    }

    pub fn is_close(&self) -> bool {
        matches!(self, Self::Close(_))
    }

    pub fn is_ping(&self) -> bool {
        matches!(self, Self::Ping(_))
    }

    pub fn is_pong(&self) -> bool {
        matches!(self, Self::Pong(_))
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Binary(b) => Some(b),
            _ => None,
        }
    }

    fn from_tungstenite(msg: tungstenite::Message) -> Self {
        match msg {
            tungstenite::Message::Text(s) => Self::Text(s.to_string()),
            tungstenite::Message::Binary(b) => Self::Binary(b.to_vec()),
            tungstenite::Message::Ping(b) => Self::Ping(b.to_vec()),
            tungstenite::Message::Pong(b) => Self::Pong(b.to_vec()),
            tungstenite::Message::Close(f) => Self::Close(f.map(|f| CloseFrame {
                code: f.code.into(),
                reason: f.reason.to_string(),
            })),
            // Frame is an internal tungstenite variant that should never appear in streams.
            // Treat as a no-op ping rather than panicking.
            tungstenite::Message::Frame(_) => Self::Ping(Vec::new()),
        }
    }

    pub(crate) fn into_tungstenite(self) -> tungstenite::Message {
        match self {
            Self::Text(s) => tungstenite::Message::Text(s.into()),
            Self::Binary(b) => tungstenite::Message::Binary(b.into()),
            Self::Ping(b) => tungstenite::Message::Ping(b.into()),
            Self::Pong(b) => tungstenite::Message::Pong(b.into()),
            Self::Close(f) => {
                tungstenite::Message::Close(f.map(|f| tungstenite::protocol::CloseFrame {
                    code: f.code.into(),
                    reason: f.reason.into(),
                }))
            }
        }
    }
}

impl From<String> for Message {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<&str> for Message {
    fn from(s: &str) -> Self {
        Self::Text(s.to_owned())
    }
}

impl From<Vec<u8>> for Message {
    fn from(b: Vec<u8>) -> Self {
        Self::Binary(b)
    }
}
