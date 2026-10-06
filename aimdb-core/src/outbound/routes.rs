//! [`OutboundRoutes`]: every outbound link of one scheme, pulled by the
//! connector's transport task.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::{Context, Poll};

use super::ready::{Polled, ReadyRoutes};
use super::RouteId;
use crate::connector::SerializeError;
use crate::transport::ConnectorConfig;
use crate::{AimDb, DbError, DbResult, RuntimeContext};

/// One outbound route, for parsing per-route configuration once at build.
#[derive(Debug, Clone)]
pub struct RouteInfo {
    /// Dense index, `0..routes().len()`.
    pub id: RouteId,
    /// The topic from the link URL, used when no topic is written.
    pub default_topic: Arc<str>,
    /// The link's configuration, with `record_index` set.
    pub config: ConnectorConfig,
    /// Longest topic the link's writer produces; 0 without a writer.
    pub topic_capacity: usize,
    /// Scratch for `with_serializer_into`; 0 with an owned serializer only.
    pub payload_capacity: usize,
}

/// A message pulled from [`OutboundRoutes`].
#[derive(Debug)]
pub struct OutboundMessage<'a> {
    /// The route it came from.
    pub route: &'a RouteInfo,
    /// The written topic, or the route's default.
    pub topic: &'a str,
    /// The serialized value.
    pub payload: OutboundPayload<'a>,
}

/// Where a pulled message's bytes are.
#[derive(Debug, PartialEq, Eq)]
pub enum OutboundPayload<'a> {
    /// Serialized into `OutboundRoutes`' scratch (`with_serializer_into`).
    Borrowed(&'a [u8]),
    /// An owned serializer's bytes (`with_serializer`). Transports that take
    /// ownership move it in; others borrow it.
    Owned(Vec<u8>),
}

impl OutboundPayload<'_> {
    /// The bytes, whichever variant holds them.
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }

    /// The bytes as an owned `Vec`: moved out of `Owned`, copied from
    /// `Borrowed`.
    pub fn into_vec(self) -> Vec<u8> {
        match self {
            Self::Borrowed(bytes) => bytes.to_vec(),
            Self::Owned(bytes) => bytes,
        }
    }
}

/// Values taken from one route's buffer, by outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RouteStats {
    /// Staged and handed to the connector, including any it then rejected.
    pub sent: u64,
    /// Handed to the connector, which could not send them (for example,
    /// larger than its transport accepts). Reported with
    /// [`OutboundRoutes::reject`].
    pub rejected: u64,
    /// Missed because the reader fell behind.
    pub lagged: u64,
    /// Skipped: the written topic did not fit.
    pub topic_overflow: u64,
    /// Skipped: the serializer failed or reported an invalid length.
    pub serialize_failed: u64,
}

/// What polling one route did.
pub(crate) enum RouteOutcome {
    /// A value was serialized. `topic_len`: bytes written into the topic
    /// scratch, `None` for the default topic.
    Staged {
        topic_len: Option<usize>,
        payload: StagedPayload,
    },
    /// The reader lagged by this many values.
    Lagged(u64),
    /// A value was taken; its topic did not fit.
    TopicOverflow,
    /// A value was taken; serializing it failed.
    SerializeFailed(SerializeFailure),
    /// The buffer is gone.
    Closed(DbError),
}

/// Where a staged payload is.
pub(crate) enum StagedPayload {
    /// The first `len` bytes of the payload scratch.
    Scratch(usize),
    Owned(Vec<u8>),
}

/// Why a value that was taken could not be serialized. The caller logs it
/// with its route.
pub(crate) enum SerializeFailure {
    /// The owned serializer (`with_serializer`) failed.
    Owned(SerializeError),
    /// `with_serializer_into` failed.
    Into(SerializeError),
    /// The value did not fit `with_serializer_into`'s scratch, and the owned
    /// serializer it fell back to failed.
    Fallback(SerializeError),
    /// `with_serializer_into` reported more bytes than its scratch holds.
    InvalidLength { len: usize, capacity: usize },
}

impl core::fmt::Display for SerializeFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Owned(e) => write!(f, "serializer failed: {e:?}"),
            Self::Into(e) => write!(f, "serializer failed into scratch: {e:?}"),
            Self::Fallback(e) => write!(
                f,
                "value did not fit the scratch and the owned fallback failed: {e:?}"
            ),
            Self::InvalidLength { len, capacity } => write!(
                f,
                "serializer returned invalid length {len} for {capacity}-byte scratch"
            ),
        }
    }
}

/// A route's typed reader, topic writer and serializers, polled through one
/// erased call.
pub(crate) trait PollRoute: Send {
    /// Poll the reader once with `cx`. On a value, write its topic into
    /// `topic` and its payload into `payload` (or an owned `Vec`).
    fn poll_route(
        &mut self,
        cx: &mut Context<'_>,
        ctx: &RuntimeContext,
        topic: &mut [u8],
        payload: &mut [u8],
    ) -> Poll<RouteOutcome>;
}

/// What a link's route factory builds.
pub(crate) struct RouteParts {
    pub(crate) route: Box<dyn PollRoute>,
    pub(crate) topic_capacity: usize,
    pub(crate) payload_capacity: usize,
}

/// Builds a link's [`RouteParts`], subscribing to its record.
pub(crate) type RouteFactoryFn = Arc<dyn Fn(&AimDb) -> RouteParts + Send + Sync>;

struct Staged {
    id: RouteId,
    topic_len: Option<usize>,
    payload: StagedPayload,
}

/// Every outbound link of one scheme, read by the connector's transport task.
///
/// Built in the connector's `build()`: cursors start then, so an SPMC ring
/// keeps what is produced before the transport first polls. Only routes
/// whose readers woke are polled, in round-robin order.
pub struct OutboundRoutes {
    routes: Box<[RouteInfo]>,
    states: Box<[Box<dyn PollRoute>]>,
    stats: Box<[RouteStats]>,
    ready: ReadyRoutes,
    /// Topic region (`topic_region` bytes), then payload region.
    scratch: Box<[u8]>,
    topic_region: usize,
    staged: Option<Staged>,
    ctx: RuntimeContext,
}

// Moved into the connector's `Send` transport task.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<OutboundRoutes>();
};

impl OutboundRoutes {
    /// Subscribes every outbound link of `scheme` and allocates the scratch
    /// once: the largest topic capacity plus the largest payload capacity.
    pub fn new(db: &AimDb, scheme: &str) -> DbResult<Self> {
        let mut routes = Vec::new();
        let mut states = Vec::new();

        for (record_index, link) in db.outbound_links(scheme) {
            let parts = (link.route_factory)(db);
            let mut config = ConnectorConfig::from_query(&link.config);
            config.record_index = Some(record_index);
            routes.push(RouteInfo {
                id: routes.len(),
                default_topic: Arc::from(link.url.resource_id()),
                config,
                topic_capacity: parts.topic_capacity,
                payload_capacity: parts.payload_capacity,
            });
            states.push(parts.route);
        }

        let topic_region = routes.iter().map(|r| r.topic_capacity).max().unwrap_or(0);
        let payload_region = routes.iter().map(|r| r.payload_capacity).max().unwrap_or(0);
        Ok(Self {
            stats: alloc::vec![RouteStats::default(); routes.len()].into_boxed_slice(),
            ready: ReadyRoutes::new(routes.len()),
            routes: routes.into_boxed_slice(),
            states: states.into_boxed_slice(),
            scratch: alloc::vec![0; topic_region + payload_region].into_boxed_slice(),
            topic_region,
            staged: None,
            ctx: db.runtime_ctx(),
        })
    }

    /// Routes, for parsing per-route configuration once at build.
    pub fn routes(&self) -> &[RouteInfo] {
        &self.routes
    }

    /// Values taken from route `id`'s buffer so far, by outcome.
    pub fn stats(&self, id: RouteId) -> Option<RouteStats> {
        self.stats.get(id).copied()
    }

    /// Take the next ready value from a route that woke and serialize it into
    /// the scratch. Lends nothing.
    ///
    /// - `Ready(Some(id))`: a message from route `id` is staged; take it with
    ///   [`take_staged`](Self::take_staged). A staged message that was never
    ///   taken is returned again, not replaced.
    /// - `Ready(None)`: every route is closed, or there were none. Final: a
    ///   `select` arm must be disarmed after it.
    /// - `Pending`: no woken route had a value. On Tokio this can also mean
    ///   the task's budget is spent; the task is woken to try again.
    ///
    /// A value leaves its buffer only in a call that returns `Ready`, so this
    /// is safe as a `select` arm. Skipped values (topic overflow, serializer
    /// error) and lag are logged and counted in [`stats`](Self::stats).
    pub fn poll_stage(&mut self, cx: &mut Context<'_>) -> Poll<Option<RouteId>> {
        if let Some(staged) = &self.staged {
            return Poll::Ready(Some(staged.id));
        }
        let Self {
            routes,
            states,
            stats,
            ready,
            scratch,
            topic_region,
            staged,
            ctx,
        } = self;
        let (topic_buf, payload_buf) = scratch.split_at_mut(*topic_region);
        ready.poll_ready(cx, |id, route_cx| {
            let info = &routes[id];
            let topic = &mut topic_buf[..info.topic_capacity];
            let payload = &mut payload_buf[..info.payload_capacity];
            let stats = &mut stats[id];
            match states[id].poll_route(route_cx, ctx, topic, payload) {
                Poll::Pending => Polled::Pending,
                Poll::Ready(RouteOutcome::Staged { topic_len, payload }) => {
                    stats.sent += 1;
                    *staged = Some(Staged {
                        id,
                        topic_len,
                        payload,
                    });
                    Polled::Staged
                }
                Poll::Ready(RouteOutcome::Lagged(n)) => {
                    stats.lagged += n;
                    log_warn!("outbound route '{}' lagged by {}", info.default_topic, n);
                    Polled::Skipped
                }
                Poll::Ready(RouteOutcome::TopicOverflow) => {
                    stats.topic_overflow += 1;
                    log_warn!(
                        "outbound route '{}': topic does not fit in {} bytes, value skipped",
                        info.default_topic,
                        info.topic_capacity
                    );
                    Polled::Skipped
                }
                Poll::Ready(RouteOutcome::SerializeFailed(_failure)) => {
                    stats.serialize_failed += 1;
                    log_error!(
                        "outbound route '{}': {}, value skipped",
                        info.default_topic,
                        _failure
                    );
                    Polled::Skipped
                }
                Poll::Ready(RouteOutcome::Closed(_e)) => {
                    log_info!("outbound route '{}' closed: {:?}", info.default_topic, _e);
                    Polled::Closed
                }
            }
        })
    }

    /// Lend the staged message and clear it. `None` if nothing is staged.
    pub fn take_staged(&mut self) -> Option<OutboundMessage<'_>> {
        let staged = self.staged.take()?;
        let route = &self.routes[staged.id];
        let (topic_buf, payload_buf) = self.scratch.split_at(self.topic_region);
        let topic = match staged.topic_len {
            // Written through `TopicBuf`, which only ever holds whole `&str`s.
            Some(len) => core::str::from_utf8(&topic_buf[..len]).unwrap_or_default(),
            None => &route.default_topic,
        };
        let payload = match staged.payload {
            StagedPayload::Scratch(len) => OutboundPayload::Borrowed(&payload_buf[..len]),
            StagedPayload::Owned(bytes) => OutboundPayload::Owned(bytes),
        };
        Some(OutboundMessage {
            route,
            topic,
            payload,
        })
    }

    /// Count a message from route `id` that the connector took but could not
    /// send. The connector logs why; this keeps the count beside the route's
    /// other outcomes.
    pub fn reject(&mut self, id: RouteId) {
        if let Some(stats) = self.stats.get_mut(id) {
            stats.rejected += 1;
        }
    }

    /// [`poll_stage`](Self::poll_stage), then [`take_staged`](Self::take_staged),
    /// for hand-written `poll` code.
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<OutboundMessage<'_>>> {
        match self.poll_stage(cx) {
            Poll::Ready(Some(_)) => Poll::Ready(self.take_staged()),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    /// The next message, or `None` once every route is closed.
    ///
    /// Cancel-safe: dropping the future before it completes takes nothing.
    pub async fn next(&mut self) -> Option<OutboundMessage<'_>> {
        poll_fn(|cx| self.poll_stage(cx)).await?;
        self.take_staged()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_helpers_cover_both_variants() {
        let scratch = [1u8, 2, 3];
        assert_eq!(OutboundPayload::Borrowed(&scratch).as_slice(), [1, 2, 3]);
        assert_eq!(OutboundPayload::Borrowed(&scratch).into_vec(), [1, 2, 3]);

        let owned = alloc::vec![4u8, 5];
        let ptr = owned.as_ptr();
        let payload = OutboundPayload::Owned(owned);
        assert_eq!(payload.as_slice(), [4, 5]);
        let moved = payload.into_vec();
        assert_eq!(moved.as_ptr(), ptr, "moved, not copied");
    }

    #[tokio::test]
    async fn reject_counts_beside_the_routes_other_outcomes() {
        let (db, _runner) = crate::AimDbBuilder::new()
            .runtime(Arc::new(crate::executor::test_support::NoopRuntimeOps))
            .build()
            .await
            .expect("empty database");
        let mut routes = OutboundRoutes::new(&db, "mqtt").unwrap();
        // No routes: an unknown id is ignored rather than a panic.
        routes.reject(0);
        assert_eq!(routes.stats(0), None);
    }
}
