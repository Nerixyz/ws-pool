use std::{str::FromStr, time::Duration};

use fastwebsockets::{CloseCode, Frame, OpCode, Payload};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use url::Url;

use crate::{handler::WorkerEvent, ws};

/// Context for a worker (one `WebSocket` connection)
pub struct WsContext<E> {
    id: usize,
    sock: ws::Socket,
    timeout: Duration,
    tx: mpsc::Sender<WorkerEvent<E>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WsReadError<E> {
    #[error("Read timed out")]
    Timeout,
    #[error("Failed to parse WebSocket message")]
    Parse,
    #[error("WebSocket message wasn't text")]
    NotText,
    #[error("WebSocket message wasn't valid UTF8")]
    NotUtf8,
    #[error("Failed to deserialize message: {0}")]
    Deserialize(E),
}

impl<E> WsContext<E> {
    pub(crate) fn new(id: usize, sock: ws::Socket, tx: mpsc::Sender<WorkerEvent<E>>) -> Self {
        Self {
            id,
            sock,
            timeout: Duration::from_secs(60),
            tx,
        }
    }

    /// Get the ID of the current worker
    pub fn id(&self) -> usize {
        self.id
    }

    /// Reconnect to the specified `url`.
    pub async fn reconnect(&mut self, url: &Url) -> Result<(), ws::StartError> {
        debug!(%url, "Reconnect");
        let _ = self
            .sock
            .write_frame(Frame::close(CloseCode::Normal.into(), b""))
            .await;
        self.sock = ws::connect(url).await?;
        Ok(())
    }

    pub(crate) async fn read<M>(&mut self) -> Result<M, WsReadError<M::Err>>
    where
        M: FromStr,
    {
        let frame = tokio::time::timeout(self.timeout, self.sock.read_frame()).await;
        let Ok(frame) = frame else {
            warn!("Read timed out");
            let _ = self
                .sock
                .write_frame(Frame::close(CloseCode::Normal.into(), b""))
                .await;
            return Err(WsReadError::Timeout);
        };

        let msg = match frame {
            Ok(msg) => msg,
            Err(error) => {
                warn!(%error, "Failed to parse websocket message");
                let _ = self
                    .sock
                    .write_frame(Frame::close(CloseCode::Normal.into(), b""))
                    .await;
                return Err(WsReadError::Parse);
            }
        };
        if msg.opcode != OpCode::Text {
            return Err(WsReadError::NotText);
        }

        std::str::from_utf8(&msg.payload)
            .map_err(|_| WsReadError::NotUtf8)?
            .parse::<M>()
            .map_err(WsReadError::Deserialize)
    }

    /// Set the read timeout.
    /// If a read takes longer than the specified duration, the connection is considered dead and is closed.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Emit an event to the [Handler][crate::Handler].
    pub async fn emit(&mut self, event: E) -> Result<(), crate::SendError> {
        self.tx
            .send(WorkerEvent::User(self.id, event))
            .await
            .map_err(From::from)
    }

    /// Send a text message on the `WebSocket`.
    pub async fn send(&mut self, msg: &str) -> Result<(), fastwebsockets::WebSocketError> {
        self.sock
            .write_frame(Frame::text(Payload::Borrowed(msg.as_bytes())))
            .await
    }

    pub(crate) async fn close(mut self) {
        let _ = self
            .sock
            .write_frame(Frame::close(CloseCode::Normal.into(), b""))
            .await;
        let _ = self.tx.send(WorkerEvent::Closed(self.id)).await;
    }
}
