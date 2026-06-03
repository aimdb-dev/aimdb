//! WS transport adapters — `Connection` (and, for the client, `Dialer`) over a
//! real WebSocket, so the shared session engines drive it.
//!
//! The **server** side (`WsServerConnection`) wraps axum's upgraded `WebSocket`
//! and performs the **multi-topic split**: a `Subscribe`/`Unsubscribe` carrying N
//! topics is yielded as N single-topic frames, so the codec's `decode` stays 1→1.

#[cfg(feature = "server")]
use std::collections::VecDeque;

#[cfg(any(feature = "server", feature = "client"))]
use aimdb_core::{BoxFut, Connection, PeerInfo, TransportError, TransportResult};
#[cfg(feature = "server")]
use axum::extract::ws::{Message, WebSocket};

#[cfg(feature = "server")]
use crate::protocol::ClientMessage;

/// A server-side [`Connection`] over an upgraded axum [`WebSocket`].
#[cfg(feature = "server")]
pub struct WsServerConnection {
    ws: WebSocket,
    peer: PeerInfo,
    /// Single-topic frames split off a multi-topic `Subscribe`/`Unsubscribe`,
    /// drained before reading the next WS message.
    pending: VecDeque<Vec<u8>>,
}

#[cfg(feature = "server")]
impl WsServerConnection {
    /// Wrap an upgraded socket with its pre-resolved peer identity.
    ///
    /// `auto_subscribe` seeds synthetic single-topic `Subscribe` frames so the
    /// engine subscribes the client to those patterns on connect, without a
    /// client message.
    pub fn new(ws: WebSocket, peer: PeerInfo, auto_subscribe: &[String]) -> Self {
        let pending = auto_subscribe
            .iter()
            .filter_map(|t| {
                serde_json::to_vec(&ClientMessage::Subscribe {
                    topics: vec![t.clone()],
                })
                .ok()
            })
            .collect();
        Self { ws, peer, pending }
    }

    /// Read one logical frame, expanding a multi-topic `Subscribe`/`Unsubscribe`
    /// into one single-topic frame per call (the rest are queued in `pending`).
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
            if let Some(split) = split_multi_topic(&bytes) {
                self.pending.extend(split);
                continue;
            }
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

/// If `bytes` is a multi-topic `Subscribe`/`Unsubscribe`, return one re-serialized
/// single-topic frame per topic; otherwise `None` (the frame passes through).
#[cfg(feature = "server")]
fn split_multi_topic(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let msg: ClientMessage = serde_json::from_slice(bytes).ok()?;
    let (topics, is_sub) = match msg {
        ClientMessage::Subscribe { topics } if topics.len() > 1 => (topics, true),
        ClientMessage::Unsubscribe { topics } if topics.len() > 1 => (topics, false),
        _ => return None,
    };
    Some(
        topics
            .into_iter()
            .filter_map(|t| {
                let one = if is_sub {
                    ClientMessage::Subscribe { topics: vec![t] }
                } else {
                    ClientMessage::Unsubscribe { topics: vec![t] }
                };
                serde_json::to_vec(&one).ok()
            })
            .collect(),
    )
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

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

    #[test]
    fn splits_multi_topic_subscribe() {
        let frame = serde_json::to_vec(&ClientMessage::Subscribe {
            topics: vec!["a".into(), "b".into(), "c".into()],
        })
        .unwrap();
        let split = split_multi_topic(&frame).expect("should split");
        assert_eq!(split.len(), 3);
        for (i, t) in ["a", "b", "c"].iter().enumerate() {
            match serde_json::from_slice::<ClientMessage>(&split[i]).unwrap() {
                ClientMessage::Subscribe { topics } => assert_eq!(topics, vec![t.to_string()]),
                _ => panic!("expected single-topic Subscribe"),
            }
        }
    }

    #[test]
    fn single_topic_passes_through() {
        let frame = serde_json::to_vec(&ClientMessage::Subscribe {
            topics: vec!["only".into()],
        })
        .unwrap();
        assert!(split_multi_topic(&frame).is_none());
    }

    #[test]
    fn non_subscribe_passes_through() {
        let frame = serde_json::to_vec(&ClientMessage::Ping).unwrap();
        assert!(split_multi_topic(&frame).is_none());
    }
}
