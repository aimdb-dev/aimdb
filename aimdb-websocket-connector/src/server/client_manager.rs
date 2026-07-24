//! Shared per-topic broadcast bus.
//!
//! [`ClientManager`] is the **fan-out bridge** behind `Dispatch::subscribe`: one
//! record update reaches every matching subscription. Each `WsSession::subscribe`
//! registers a per-subscription channel and gets back a [`BoxStream`] of
//! topic-tagged [`SubUpdate`]s; the engine envelopes each into an AimX `event`
//! frame per connection (the payload bytes stay `Arc`-shared — only the small
//! envelope is per-subscriber). The outbound record→broadcast tasks
//! ([`super::connector`]) feed [`broadcast`](ClientManager::broadcast).
//!
//! Frame formatting lives in the codec; the per-connection send half is owned by
//! `run_session`.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use aimdb_core::{topic_matches, BoxStream, Payload, SubUpdate};
use dashmap::DashMap;
use tokio::sync::mpsc;

use super::auth::ClientId;

/// One live subscription: a wildcard pattern + the channel feeding its stream.
struct SubEntry {
    pattern: String,
    /// Bounded; `broadcast` drops on a full channel (slow-client protection).
    tx: mpsc::Sender<SubUpdate>,
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
}

impl ClientManager {
    /// Create a new, empty bus. `sub_capacity` bounds each subscription's queue.
    pub fn new(sub_capacity: usize) -> Self {
        Self {
            subs: Arc::new(DashMap::new()),
            next_sub: Arc::new(AtomicU64::new(1)),
            next_client: Arc::new(AtomicU64::new(1)),
            connections: Arc::new(AtomicU64::new(0)),
            sub_capacity: sub_capacity.max(1),
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
    /// topic-tagged record-value updates. Dropping the stream ends the
    /// subscription; the next matching [`broadcast`](Self::broadcast) lazily
    /// prunes the entry.
    pub fn subscribe(&self, pattern: &str) -> (u64, BoxStream<'static, SubUpdate>) {
        let id = self.next_sub.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<SubUpdate>(self.sub_capacity);
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
    ///
    /// The payload and the topic tag are `Arc`-shared to every matching
    /// subscription (refcount bumps, no per-subscriber copies); the per-frame
    /// envelope is applied downstream by each connection's codec.
    pub async fn broadcast(&self, topic: &str, payload_bytes: &[u8]) {
        let payload = Payload::from(payload_bytes);
        let tag: Arc<str> = Arc::from(topic);
        let mut dead: Vec<u64> = Vec::new();
        for entry in self.subs.iter() {
            if !topic_matches(&entry.pattern, topic) {
                continue;
            }
            // Bounded: drop on a full queue (slow-client protection), prune only
            // when the receiver is gone (stream dropped).
            if let Err(mpsc::error::TrySendError::Closed(_)) = entry
                .tx
                .try_send(SubUpdate::tagged(tag.clone(), payload.clone()))
            {
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
        Self::new(256)
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
        let mgr = ClientManager::new(256);
        let (_id, mut stream) = mgr.subscribe("sensors.#");

        mgr.broadcast("sensors.temp.vienna", b"22.5").await;

        // Delivery is the raw payload tagged with the real topic — even for the
        // wildcard sub; the envelope is the per-connection codec's job.
        let update = stream.next().await.expect("should receive");
        assert_eq!(update.topic.as_deref(), Some("sensors.temp.vienna"));
        assert_eq!(&update.data[..], b"22.5");
    }

    #[tokio::test]
    async fn non_matching_topic_is_not_delivered() {
        use futures_util::FutureExt;
        let mgr = ClientManager::new(256);
        let (_id, mut stream) = mgr.subscribe("commands.#");
        mgr.broadcast("sensors.temp", b"22.5").await;
        // Nothing queued: the next() future is not ready.
        assert!(stream.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn fan_out_to_n_subscribers() {
        let mgr = ClientManager::new(256);
        let mut streams: Vec<_> = (0..5).map(|_| mgr.subscribe("#").1).collect();
        mgr.broadcast("any/topic", b"\"v\"").await;
        for s in &mut streams {
            let update = s.next().await.unwrap();
            assert_eq!(update.topic.as_deref(), Some("any/topic"));
        }
    }

    #[tokio::test]
    async fn dropped_stream_is_pruned() {
        let mgr = ClientManager::new(256);
        let (_id, stream) = mgr.subscribe("#");
        assert_eq!(mgr.subscription_count(), 1);
        drop(stream);
        mgr.broadcast("t", b"v").await;
        assert_eq!(mgr.subscription_count(), 0);
    }

    // One broadcast → N subscribers all receive the *same* payload allocation
    // (a shared `Arc`), evidencing O(1) fan-out regardless of subscriber count.
    #[tokio::test]
    async fn broadcast_shares_one_payload_to_all() {
        let mgr = ClientManager::new(256);
        let mut streams: Vec<_> = (0..8).map(|_| mgr.subscribe("#").1).collect();
        mgr.broadcast("t", b"123").await;
        let mut updates = Vec::new();
        for s in &mut streams {
            updates.push(s.next().await.unwrap());
        }
        let first = updates[0].data.as_ptr();
        assert!(
            updates.iter().all(|u| u.data.as_ptr() == first),
            "every subscriber shares the one payload Arc"
        );
    }
}
