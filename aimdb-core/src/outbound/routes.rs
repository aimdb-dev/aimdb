//! [`OutboundRoutes`]: every outbound link of one scheme, pulled by the
//! connector's transport task.

use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::{Context, Poll};

use super::ready::{Polled, ReadyRoutes};
use super::RouteId;
use crate::transport::ConnectorConfig;
use crate::{AimDb, ConfigError, DbError, DbResult, RuntimeContext};

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
    /// Staged for the connector.
    pub sent: u64,
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
    /// A value was taken; serializing it failed (already logged).
    SerializeFailed,
    /// The buffer is gone.
    Closed,
}

/// Where a staged payload is.
pub(crate) enum StagedPayload {
    /// The first `len` bytes of the payload scratch.
    Scratch(usize),
    Owned(Vec<u8>),
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
    /// The link uses `with_topic_provider`, which this path does not support.
    pub(crate) topic_provider: bool,
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

impl OutboundRoutes {
    /// Subscribes every outbound link of `scheme` and allocates the scratch
    /// once: the largest topic capacity plus the largest payload capacity.
    ///
    /// Rejects every link that uses `with_topic_provider`.
    pub fn new(db: &AimDb, scheme: &str) -> DbResult<Self> {
        let mut routes = Vec::new();
        let mut states = Vec::new();
        let mut errors = Vec::new();

        for (record_index, record_key, link) in db.outbound_links(scheme) {
            let Some(factory) = &link.route_factory else {
                errors.push(ConfigError::new(
                    record_key,
                    Some(link.url.to_string()),
                    "link was not registered through `link_to`",
                ));
                continue;
            };
            let parts = factory(db);
            if parts.topic_provider {
                errors.push(ConfigError::new(
                    record_key,
                    Some(link.url.to_string()),
                    "`with_topic_provider` is not supported here; use `with_topic_writer` or `with_topic_fn`",
                ));
                continue;
            }
            let mut query = link.config.clone();
            query.push(("record_index".to_string(), record_index.to_string()));
            routes.push(RouteInfo {
                id: routes.len(),
                default_topic: Arc::from(link.url.resource_id()),
                config: ConnectorConfig::from_query(&query),
                topic_capacity: parts.topic_capacity,
                payload_capacity: parts.payload_capacity,
            });
            states.push(parts.route);
        }

        if !errors.is_empty() {
            return Err(DbError::InvalidConfiguration { errors });
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
                Poll::Ready(RouteOutcome::SerializeFailed) => {
                    stats.serialize_failed += 1;
                    Polled::Skipped
                }
                Poll::Ready(RouteOutcome::Closed) => {
                    log_debug!("outbound route '{}' closed", info.default_topic);
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
}
