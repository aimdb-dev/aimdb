//! Inbound entry point for connectors.
//!
//! A connector builds one [`InboundDispatch`] per scheme and calls
//! [`dispatch`](InboundDispatch::dispatch) with borrows of each message it
//! receives. Core matches the topic, deserializes and produces in place.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::router::Router;
use crate::{AimDb, DbResult, RuntimeContext, TopicGrammar};

/// Every inbound link of one scheme, compiled against the connector's grammar.
///
/// Cheap to clone: clones share the compiled routes, so a connector can
/// dispatch from several tasks into the same records.
#[derive(Clone)]
pub struct InboundDispatch {
    router: Arc<Router>,
    ctx: RuntimeContext,
}

impl InboundDispatch {
    /// Compiles every inbound link of `scheme` against `grammar`.
    ///
    /// Rejects every link the grammar or its key cannot compile, naming the
    /// record and the resolved topic.
    pub fn new(db: &AimDb, scheme: &str, grammar: &'static dyn TopicGrammar) -> DbResult<Self> {
        Ok(Self {
            router: Arc::new(db.inbound_router(scheme, grammar)?),
            ctx: db.runtime_ctx(),
        })
    }

    /// A dispatcher over an already compiled `router`, for the session tests.
    #[cfg(all(test, feature = "connector-session"))]
    pub(crate) fn from_parts(router: Router, ctx: RuntimeContext) -> Self {
        Self {
            router: Arc::new(router),
            ctx,
        }
    }

    /// Matches `topic`, deserializes `payload` and produces into every
    /// matching record.
    ///
    /// Synchronous and never blocks. Ingest failures, unmatched topics, full
    /// buffers and full key tables are logged, not returned.
    pub fn dispatch(&self, topic: &str, payload: &[u8]) {
        // `Router::route` only ever returns `Ok`.
        let _ = self.router.route(topic, payload, &self.ctx);
    }

    /// Filters to subscribe at the transport. Allocates; call it at connect
    /// time, not per message.
    pub fn subscriptions(&self) -> Vec<Arc<str>> {
        self.router.subscriptions()
    }

    /// Number of inbound routes.
    pub fn route_count(&self) -> usize {
        self.router.route_count()
    }
}
