//! WS transport adapters — `Connection` (and, for the client, `Dialer`) over a
//! real WebSocket, so the shared session engines drive it.
//!
//! WebSocket frames are already message-delimited, so each AimX codec blob
//! rides as one WS text frame — no newline/length framing. The **server** side
//! (`WsServerConnection`) wraps axum's upgraded `WebSocket` and seeds the
//! builder's auto-subscribe patterns as synthetic AimX `sub` frames.

#[cfg(feature = "server")]
use std::collections::VecDeque;

#[cfg(any(feature = "server", feature = "client"))]
use aimdb_core::{BoxFut, Connection, PeerInfo, TransportError, TransportResult};
#[cfg(feature = "server")]
use axum::extract::ws::{Message, WebSocket};

/// A server-side [`Connection`] over an upgraded axum [`WebSocket`].
#[cfg(feature = "server")]
pub struct WsServerConnection {
    ws: WebSocket,
    peer: PeerInfo,
    /// Synthetic auto-subscribe frames, drained before reading the socket.
    pending: VecDeque<Vec<u8>>,
}

#[cfg(feature = "server")]
impl WsServerConnection {
    /// Wrap an upgraded socket with its pre-resolved peer identity.
    ///
    /// `auto_subscribe` seeds synthetic AimX `sub` frames so the engine
    /// subscribes the client to those patterns on connect, without a client
    /// message. Their ids count down from `u64::MAX` so they cannot collide
    /// with client-chosen ids (engine clients allocate from 1 upward); the
    /// resulting events carry these server-side sub ids, so engine-demuxed
    /// clients should subscribe explicitly instead (design 045 §3.6).
    pub fn new(ws: WebSocket, peer: PeerInfo, auto_subscribe: &[String]) -> Self {
        let pending = auto_subscribe
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                serde_json::to_vec(&serde_json::json!({
                    "t": "sub",
                    "id": u64::MAX - i as u64,
                    "topic": t,
                }))
                .ok()
            })
            .collect();
        Self { ws, peer, pending }
    }

    /// Read one logical frame, draining seeded auto-subscribe frames first.
    async fn next_logical(&mut self) -> TransportResult<Option<Vec<u8>>> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }
            let bytes = match self.ws.recv().await {
                Some(Ok(Message::Text(t))) => t.as_str().as_bytes().to_vec(),
                Some(Ok(Message::Binary(b))) => b.to_vec(),
                // axum answers Ping transparently; ignore Pong; Close ends the stream.
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Err(_)) => return Err(TransportError::Io),
            };
            return Ok(Some(bytes));
        }
    }
}

#[cfg(feature = "server")]
impl Connection for WsServerConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(self.next_logical())
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(async move {
            let text = String::from_utf8_lossy(frame).into_owned();
            self.ws
                .send(Message::Text(text.into()))
                .await
                .map_err(|_| TransportError::Io)
        })
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

// ════════════════════════════════════════════════════════════════════
// Client side — Dialer + Connection over tokio-tungstenite
// ════════════════════════════════════════════════════════════════════

/// A [`Dialer`](aimdb_core::Dialer) that opens a `tokio-tungstenite` connection
/// to a remote WS server. `run_client` ([`aimdb_core::session::run_client`]) calls
/// [`connect`](aimdb_core::Dialer::connect) on each (re)dial.
#[cfg(feature = "client")]
pub struct WsDialer {
    url: String,
}

#[cfg(feature = "client")]
impl WsDialer {
    /// Dial the WS server at `url` (e.g. `wss://host/ws`).
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[cfg(feature = "client")]
impl aimdb_core::Dialer for WsDialer {
    fn connect(
        &self,
    ) -> aimdb_core::BoxFut<'_, aimdb_core::TransportResult<Box<dyn aimdb_core::Connection>>> {
        Box::pin(async move {
            let (ws, _resp) = tokio_tungstenite::connect_async(&self.url)
                .await
                .map_err(|_| aimdb_core::TransportError::Io)?;
            Ok(Box::new(WsClientConnection {
                ws,
                peer: aimdb_core::PeerInfo::default(),
            }) as Box<dyn aimdb_core::Connection>)
        })
    }
}

/// A client-side [`Connection`] over a `tokio-tungstenite` stream.
#[cfg(feature = "client")]
pub struct WsClientConnection {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    peer: aimdb_core::PeerInfo,
}

#[cfg(feature = "client")]
impl Connection for WsClientConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        Box::pin(async move {
            loop {
                return match self.ws.next().await {
                    Some(Ok(Message::Text(t))) => Ok(Some(t.as_bytes().to_vec())),
                    Some(Ok(Message::Binary(b))) => Ok(Some(b.to_vec())),
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) | None => Ok(None),
                    Some(Ok(_)) => continue, // Frame — ignore
                    Some(Err(_)) => Err(TransportError::Io),
                };
            }
        })
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;
        Box::pin(async move {
            let text = String::from_utf8_lossy(frame).into_owned();
            self.ws
                .send(Message::Text(text.into()))
                .await
                .map_err(|_| TransportError::Io)
        })
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}
