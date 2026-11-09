//! [`OutboundLatest`]: each outbound link's current value, rendered on demand.
//!
//! A connector that sends late-join snapshots needs "what would this link send
//! for the record's current value". [`OutboundRoutes`](super::OutboundRoutes)
//! is owned by the connector's transport task and only sees new writes, so
//! this is a separate, cheap-to-clone view that reads the record's latest value
//! and runs it through the link's topic writer and serializer. It keeps no
//! state of its own.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::RouteId;
use crate::builder::AimDb;
use crate::RuntimeContext;

/// One link's current value, as the link would send it.
pub(crate) struct Rendered {
    /// Written by the link's topic writer; `None` for the link's default topic.
    pub(crate) topic: Option<String>,
    pub(crate) payload: Vec<u8>,
    pub(crate) metadata: crate::WriteMetadata,
}

/// Renders a link's current value. The topic is resolved first and passed to
/// `accept` (`None`: the link's default topic); the value is serialized only
/// if `accept` returns `true`. `None` if the record has no current value (none
/// written yet), is an `SpmcRing` (a stream of events, so no late-join
/// snapshot), `accept` rejects the topic, or the
/// topic does not fit or serializing fails.
pub(crate) type LatestFn = Arc<
    dyn Fn(&AimDb, &RuntimeContext, &dyn Fn(Option<&str>) -> bool) -> Option<Rendered>
        + Send
        + Sync,
>;

/// A link's current value, rendered by [`OutboundLatest::render`].
#[derive(Debug, Clone)]
pub struct LatestMessage {
    /// Index of the record the link belongs to (its `RecordId`), as in
    /// [`RouteInfo::record_index`](super::RouteInfo::record_index).
    pub record_index: usize,
    /// The topic the link sends this value on.
    pub topic: String,
    /// The serialized value.
    pub payload: Vec<u8>,
    /// Metadata of the write that stored the value.
    pub metadata: crate::WriteMetadata,
}

impl LatestMessage {
    /// When the value was written, on the instance clock.
    pub fn stamp(&self) -> crate::Stamp {
        self.metadata.stamp()
    }
}

struct LatestRoute {
    record_index: usize,
    default_topic: Arc<str>,
    has_topic_writer: bool,
    render: LatestFn,
}

/// The current value of every outbound link of one scheme, rendered on
/// demand. Route ids are the same as in an
/// [`OutboundRoutes`](super::OutboundRoutes) built for the same scheme.
///
/// Only records with a current value render: an `SpmcRing` buffer has none,
/// so its links never do.
#[derive(Clone)]
pub struct OutboundLatest {
    db: AimDb,
    ctx: RuntimeContext,
    routes: Arc<[LatestRoute]>,
}

// Shared by every session of a connector.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OutboundLatest>();
};

impl OutboundLatest {
    /// Every outbound link of `scheme`, in route order.
    pub fn new(db: &AimDb, scheme: &str) -> Self {
        let routes: Vec<LatestRoute> = db
            .outbound_links(scheme)
            .map(|(record_index, _, link)| LatestRoute {
                record_index,
                default_topic: Arc::from(link.url.resource_id()),
                has_topic_writer: link.has_topic_writer,
                render: link.latest_fn.clone(),
            })
            .collect();
        Self {
            db: db.clone(),
            ctx: db.runtime_ctx(),
            routes: routes.into(),
        }
    }

    /// Number of routes; ids are `0..len()`.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Whether the scheme has no outbound links.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Index of the record route `id` belongs to.
    pub fn record_index(&self, id: RouteId) -> Option<usize> {
        self.routes.get(id).map(|r| r.record_index)
    }

    /// Route `id`'s current value, through its topic writer and serializer.
    /// `None` if the record has no current value, or the value's topic does
    /// not fit or it fails to serialize (the live path reports those).
    pub fn render(&self, id: RouteId) -> Option<LatestMessage> {
        self.render_if(id, |_| true)
    }

    /// [`render`](Self::render), but only if `accept` takes the value's topic.
    /// The value is serialized only after `accept` returns `true`, and a link
    /// without a topic writer is checked before its value is even read, so
    /// rejected routes cost next to nothing.
    pub fn render_if(&self, id: RouteId, accept: impl Fn(&str) -> bool) -> Option<LatestMessage> {
        let route = self.routes.get(id)?;
        if !route.has_topic_writer && !accept(&route.default_topic) {
            return None;
        }
        let accept_topic = |topic: Option<&str>| accept(topic.unwrap_or(&route.default_topic));
        let rendered = (route.render)(&self.db, &self.ctx, &accept_topic)?;
        Some(LatestMessage {
            record_index: route.record_index,
            topic: rendered
                .topic
                .unwrap_or_else(|| String::from(&*route.default_topic)),
            payload: rendered.payload,
            metadata: rendered.metadata,
        })
    }
}
