//! Shared per-topic broadcast bus.
//!
//! [`ClientManager`] is the **fan-out bridge** behind `Dispatch::subscribe`: one
//! record update reaches every matching subscription. Each `WsSession::subscribe`
//! registers a per-subscription channel and gets back a [`BoxStream`] of
//! topic-tagged [`SubUpdate`]s; the engine envelopes each into an AimX `event`
//! frame per connection (the payload bytes stay `Arc`-shared — only the small
//! envelope is per-subscriber). The outbound record→broadcast tasks
//! (`super::connector`) feed [`broadcast`](ClientManager::broadcast).
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
    /// Updates dropped (full channel) since the last one that got through, to be
    /// folded into the next delivered [`SubUpdate::skipped`] so a broadcast-stage
    /// drop still surfaces as a `seq` gap downstream — the same loss signal the
    /// per-record buffer lag and the connection funnel already emit. Without
    /// this, a drop *here* (upstream of where the pump assigns `seq`) would be
    /// silent, and a slow fan-out consumer would under-report its loss.
    dropped: AtomicU64,
}

/// Drop guard for SubEntry
struct SubEntryGuard {
    id: u64,
    subs: Arc<DashMap<u64, SubEntry>>,
}

impl Drop for SubEntryGuard {
    fn drop(&mut self) {
        self.subs.remove(&self.id);
    }
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
                dropped: AtomicU64::new(0),
            },
        );
        // A drop guard for RAII, thankfully self.subs is already Arc<_>
        let guard = SubEntryGuard {
            id,
            subs: self.subs.clone(),
        };
        let stream = futures_util::stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|item| (item, (rx, guard)))
        });
        (id, Box::pin(stream))
    }

    /// Fan a serialized record-value out to every subscription whose pattern
    /// matches `topic`. Dead subscriptions (dropped streams) are pruned.
    ///
    /// The payload and the topic tag are `Arc`-shared to every matching
    /// subscription (refcount bumps, no per-subscriber copies); the per-frame
    /// envelope is applied downstream by each connection's codec.
    ///
    /// A full channel drops the update (slow-client protection) but records it on
    /// the subscription's `dropped` counter, folded into the next delivered
    /// update's `skipped` so the loss still surfaces as a `seq` gap.
    pub async fn broadcast(&self, topic: &str, payload_bytes: &[u8]) {
        let payload = Payload::from(payload_bytes);
        let tag: Arc<str> = Arc::from(topic);
        let mut dead: Vec<u64> = Vec::new();
        for entry in self.subs.iter() {
            if !topic_matches(&entry.pattern, topic) {
                continue;
            }
            // Carry any drops accumulated since the last delivered update, so a
            // broadcast-stage loss rides this update's `skipped` into a `seq`
            // gap. Take them now; restore on failure so nothing is lost.
            let carried = entry.dropped.swap(0, Ordering::Relaxed);
            let update = SubUpdate::tagged(tag.clone(), payload.clone()).with_skipped(carried);
            // Bounded: drop on a full queue (slow-client protection), prune only
            // when the receiver is gone (stream dropped).
            match entry.tx.try_send(update) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // This update is dropped too: restore the carried count and
                    // add one for it, to ride the next delivered update.
                    entry.dropped.fetch_add(carried + 1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => dead.push(*entry.key()),
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
    async fn full_channel_drops_surface_as_skipped_on_next_delivery() {
        // Capacity 1: the second and third broadcasts have nowhere to go and are
        // dropped, but the loss must ride the next delivered update's `skipped`
        // so it becomes a `seq` gap downstream (not a silent hole).
        let mgr = ClientManager::new(1);
        let (_id, mut stream) = mgr.subscribe("#");

        mgr.broadcast("t", b"1").await; // fills the one slot
        mgr.broadcast("t", b"2").await; // full → dropped (counter = 1)
        mgr.broadcast("t", b"3").await; // full → dropped (counter = 2)

        // First delivery is the update that got through, lossless.
        let first = stream.next().await.expect("first update");
        assert_eq!(&first.data[..], b"1");
        assert_eq!(first.skipped, 0);

        // With the slot now free, the next broadcast is delivered and carries the
        // two drops that happened while the channel was full.
        mgr.broadcast("t", b"4").await;
        let second = stream.next().await.expect("second update");
        assert_eq!(&second.data[..], b"4");
        assert_eq!(
            second.skipped, 2,
            "the two full-channel drops must be reported"
        );
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

    // When a stream is dropped, its subscription in ClientManager
    // together with its Sender must also be removed
    #[tokio::test]
    async fn subscription_dropped_when_stream_dropped() {
        let mgr = ClientManager::new(256);
        let (_id, stream) = mgr.subscribe("quiet.topic");

        // Count before stream dropping
        assert_eq!(mgr.subscription_count(), 1);
        drop(stream);

        // Associated entry must be unsubscribed
        assert_eq!(mgr.subscription_count(), 0);
    }
}
