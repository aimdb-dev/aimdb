//! Shared per-topic broadcast bus.
//!
//! [`ClientManager`] is the **fan-out bridge** behind `Dispatch::subscribe`: one
//! record update reaches every matching subscription. Each `WsSession::subscribe`
//! registers a per-subscription channel and gets back a [`BoxStream`] of raw
//! record-value [`Payload`]s; the per-connection [`WsCodec`](crate::codec) wraps
//! each into a `ServerMessage::Data` on encode. The outbound record→broadcast
//! tasks ([`super::connector`]) feed [`broadcast`](ClientManager::broadcast).
//!
//! Frame formatting lives in the codec; the per-connection send half is owned by
//! `run_session`.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use aimdb_core::{BoxStream, Payload};
use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::{
    codec::parse_payload,
    protocol::{now_ms, topic_matches, ServerMessage},
};

use super::auth::ClientId;

/// One live subscription: a wildcard pattern + the channel feeding its stream.
struct SubEntry {
    pattern: String,
    /// Bounded; `broadcast` drops on a full channel (slow-client protection).
    tx: mpsc::Sender<Payload>,
}

/// Shared per-topic broadcast bus. Cloning is cheap (all clones share state).
#[derive(Clone)]
pub struct ClientManager {
    /// sub-id → subscription entry.
    subs: Arc<DashMap<u64, SubEntry>>,
    /// Allocator for subscription ids.
    next_sub: Arc<AtomicU64>,
    /// Allocator for client ids (assigned at the HTTP upgrade).
    next_client: Arc<AtomicU64>,
    /// Live connection count (for the health endpoint).
    connections: Arc<AtomicU64>,
    /// Per-subscription channel bound (the builder's `with_channel_capacity`).
    sub_capacity: usize,
    /// Mirrors the builder's `with_raw_payload`: when set, `broadcast` ships the
    /// serializer bytes verbatim instead of wrapping them in a `Data` envelope.
    raw_payload: bool,
}

impl ClientManager {
    /// Create a new, empty bus. `raw_payload` mirrors the builder flag;
    /// `sub_capacity` bounds each subscription's queue.
    pub fn new(raw_payload: bool, sub_capacity: usize) -> Self {
        Self {
            subs: Arc::new(DashMap::new()),
            next_sub: Arc::new(AtomicU64::new(1)),
            next_client: Arc::new(AtomicU64::new(1)),
            connections: Arc::new(AtomicU64::new(0)),
            sub_capacity: sub_capacity.max(1),
            raw_payload,
        }
    }

    /// Allocate a new unique [`ClientId`] (called at the HTTP upgrade).
    pub fn next_client_id(&self) -> ClientId {
        ClientId(self.next_client.fetch_add(1, Ordering::Relaxed))
    }

    /// Number of live connections (informational, for `/health`).
    pub fn client_count(&self) -> usize {
        self.connections.load(Ordering::Relaxed) as usize
    }

    /// RAII guard: increments the connection count now, decrements on drop.
    pub(crate) fn connection_guard(&self) -> ConnectionGuard {
        self.connections.fetch_add(1, Ordering::Relaxed);
        ConnectionGuard {
            connections: self.connections.clone(),
        }
    }

    /// Register a subscription for `pattern`; returns its id and the stream of
    /// matching record-value payloads. Dropping the stream ends the subscription;
    /// the next matching [`broadcast`](Self::broadcast) lazily prunes the entry.
    pub fn subscribe(&self, pattern: &str) -> (u64, BoxStream<'static, Payload>) {
        let id = self.next_sub.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<Payload>(self.sub_capacity);
        self.subs.insert(
            id,
            SubEntry {
                pattern: pattern.to_string(),
                tx,
            },
        );
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        (id, Box::pin(stream))
    }

    /// Fan a serialized record-value out to every subscription whose pattern
    /// matches `topic`. Dead subscriptions (dropped streams) are pruned.
    pub async fn broadcast(&self, topic: &str, payload_bytes: &[u8]) {
        // Serialize the complete wire frame **once** here — the bus is the only
        // place the real topic + value meet, and doing it once (vs once per
        // subscriber in the codec) keeps fan-out O(1). The codec writes the
        // result verbatim. The same finished bytes are `Arc`-shared to every
        // matching subscription (a refcount bump, no per-subscriber copy).
        let frame = if self.raw_payload {
            payload_bytes.to_vec()
        } else {
            match serde_json::to_vec(&ServerMessage::Data {
                topic: topic.to_string(),
                payload: Some(parse_payload(payload_bytes)),
                ts: now_ms(),
            }) {
                Ok(f) => f,
                Err(_) => return,
            }
        };
        let payload = Payload::from(frame.as_slice());
        let mut dead: Vec<u64> = Vec::new();
        for entry in self.subs.iter() {
            if !topic_matches(&entry.pattern, topic) {
                continue;
            }
            // Bounded: drop on a full queue (slow-client protection), prune only
            // when the receiver is gone (stream dropped).
            if let Err(mpsc::error::TrySendError::Closed(_)) = entry.tx.try_send(payload.clone()) {
                dead.push(*entry.key());
            }
        }
        for id in dead {
            self.subs.remove(&id);
        }
    }

    /// Number of live subscriptions (for monitoring/tests).
    pub fn subscription_count(&self) -> usize {
        self.subs.len()
    }
}

impl Default for ClientManager {
    fn default() -> Self {
        Self::new(false, 256)
    }
}

/// Decrements the connection count when dropped (held by `WsSession`).
pub(crate) struct ConnectionGuard {
    connections: Arc<AtomicU64>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn broadcast_reaches_matching_subscriptions() {
        let mgr = ClientManager::new(false, 256);
        let (_id, mut stream) = mgr.subscribe("sensors/#");

        mgr.broadcast("sensors/temp/vienna", b"22.5").await;

        // Delivery is the complete, pre-serialized `Data` frame (built once in
        // broadcast) carrying the real topic — even for the wildcard sub.
        let payload = stream.next().await.expect("should receive");
        match serde_json::from_slice::<ServerMessage>(&payload).unwrap() {
            ServerMessage::Data { topic, payload, .. } => {
                assert_eq!(topic, "sensors/temp/vienna");
                assert_eq!(payload, Some(serde_json::json!(22.5)));
            }
            _ => panic!("expected Data"),
        }
    }

    #[tokio::test]
    async fn non_matching_topic_is_not_delivered() {
        use futures_util::FutureExt;
        let mgr = ClientManager::new(false, 256);
        let (_id, mut stream) = mgr.subscribe("commands/#");
        mgr.broadcast("sensors/temp", b"22.5").await;
        // Nothing queued: the next() future is not ready.
        assert!(stream.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn fan_out_to_n_subscribers() {
        let mgr = ClientManager::new(false, 256);
        let mut streams: Vec<_> = (0..5).map(|_| mgr.subscribe("#").1).collect();
        mgr.broadcast("any/topic", b"\"v\"").await;
        for s in &mut streams {
            let frame = s.next().await.unwrap();
            assert!(matches!(
                serde_json::from_slice::<ServerMessage>(&frame).unwrap(),
                ServerMessage::Data { topic, .. } if topic == "any/topic"
            ));
        }
    }

    #[tokio::test]
    async fn dropped_stream_is_pruned() {
        let mgr = ClientManager::new(false, 256);
        let (_id, stream) = mgr.subscribe("#");
        assert_eq!(mgr.subscription_count(), 1);
        drop(stream);
        mgr.broadcast("t", b"v").await;
        assert_eq!(mgr.subscription_count(), 0);
    }

    // Layer 2.2 (#2): one broadcast → N subscribers all receive the *same*
    // pre-serialized bytes (a shared `Arc`), evidencing a single serialization
    // regardless of subscriber count (O(1) fan-out, not O(N)).
    #[tokio::test]
    async fn broadcast_serializes_once_and_shares_to_all() {
        let mgr = ClientManager::new(false, 256);
        let mut streams: Vec<_> = (0..8).map(|_| mgr.subscribe("#").1).collect();
        mgr.broadcast("t", b"123").await;
        let mut frames = Vec::new();
        for s in &mut streams {
            frames.push(s.next().await.unwrap());
        }
        // Every subscriber got byte-identical content (the one serialized frame).
        let first = &frames[0];
        assert!(frames.iter().all(|f| f.as_ref() == first.as_ref()));
    }
}
