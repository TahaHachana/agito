//! WebSocket connections, used for Chromium on Windows, for attaching to a running browser,
//! and for Firefox (BiDi).

use async_trait::async_trait;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::socket::{SocketPair, SocketReceiver, SocketSender};
use crate::error::{Error, Result};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connects to a DevTools or BiDi WebSocket endpoint.
pub async fn connect(endpoint: &str) -> Result<SocketPair> {
    // Screenshots and large DOM snapshots arrive as single messages, so no size caps.
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    let (stream, _) = tokio_tungstenite::connect_async_with_config(endpoint, Some(config), true)
        .await
        .map_err(|e| Error::other(format!("could not connect to {endpoint}: {e}")))?;
    let (sink, stream) = stream.split();
    Ok(SocketPair::new(WsSender { sink }, WsReceiver { stream }))
}

struct WsSender {
    sink: SplitSink<Ws, Message>,
}

#[async_trait]
impl SocketSender for WsSender {
    async fn send(&mut self, message: String) -> std::io::Result<()> {
        self.sink
            .send(Message::text(message))
            .await
            .map_err(std::io::Error::other)
    }

    async fn close(&mut self) {
        let _ = self.sink.close().await;
    }
}

struct WsReceiver {
    stream: SplitStream<Ws>,
}

#[async_trait]
impl SocketReceiver for WsReceiver {
    async fn recv(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            match self.stream.next().await {
                None => return Ok(None),
                Some(Err(e)) => return Err(std::io::Error::other(e)),
                Some(Ok(Message::Text(text))) => return Ok(Some(text.as_bytes().to_vec())),
                Some(Ok(Message::Binary(bytes))) => return Ok(Some(bytes.to_vec())),
                Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(_)) => continue,
            }
        }
    }
}
