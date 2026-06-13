//! Data-plane pump helpers.
//!
//! Two free functions that own the boilerplate a data-plane connector used to
//! hand-roll. The author writes only the pure I/O adapter — a
//! [`Connector`](crate::transport::Connector) (outbound) and a [`Source`]
//! (inbound) — and composes the helpers in `build()`
//! (illustrative — `sink()`/`subscription()` are the author's own constructors):
//!
//! ```rust,ignore
//! let mut f = pump_sink(db, "redis", self.sink().await?);          // outbound
//! f.extend(pump_source(db, "redis", self.subscription().await?));  // inbound
//! Ok(f)
//! ```
//!
//! Both are `no_std + alloc`-native (boxed futures, no `tokio`).

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::Source;
use crate::builder::{AimDb, BoxFuture};
use crate::router::RouterBuilder;
use crate::transport::{Connector, ConnectorConfig};

/// Outbound pump: one publisher future per outbound route on `scheme`.
///
/// Extracts the consume-and-publish loop a data-plane connector used to write by
/// hand. For each route from [`collect_outbound_routes`](AimDb::collect_outbound_routes),
/// the returned future subscribes to the route's fused
/// [`SerializedSource`](crate::connector::SerializedSource) — whose readers
/// yield destination + serialized payload directly (no `Box<dyn Any>` per
/// message, design 036 W1) — and publishes through `sink`. Per-route
/// configuration (`qos`/`retain`/…) is built once from the route's URL query
/// via [`ConnectorConfig::from_query`].
///
/// The publisher future terminates when its subscription yields an error (e.g. the
/// record buffer closed), matching the legacy hand-rolled loop.
pub fn pump_sink(db: &AimDb, scheme: &str, sink: Arc<dyn Connector>) -> Vec<BoxFuture> {
    let routes = db.collect_outbound_routes(scheme);
    let mut futures: Vec<BoxFuture> = Vec::with_capacity(routes.len());

    for crate::OutboundRoute {
        topic: default_topic,
        source,
        config,
    } in routes
    {
        let sink = sink.clone();
        let runtime_ctx = db.runtime_ctx();
        let cfg = ConnectorConfig::from_query(&config);

        futures.push(Box::pin(async move {
            // Subscribe inside the pump future (not at collect time), so the
            // ring-buffer cursor starts when the publisher actually runs.
            let mut reader = source.subscribe();

            log_info!(
                "pump_sink: publisher started for destination: {}",
                default_topic
            );

            loop {
                let msg = match reader.recv(&runtime_ctx).await {
                    Ok(m) => m,
                    // SPMC-ring overflow: messages were missed, but the reader
                    // recovers (cursor resets to the oldest live value). Skip the
                    // gap and keep pumping — a transient lag must not permanently
                    // kill the publisher.
                    Err(crate::DbError::BufferLagged { .. }) => {
                        log_warn!("pump_sink: consumer lagged for '{}'", default_topic);
                        continue;
                    }
                    // Buffer closed / fatal — the record is gone; end the publisher.
                    Err(_e) => {
                        log_info!(
                            "pump_sink: publisher stopping for '{}': {:?}",
                            default_topic,
                            _e
                        );
                        break;
                    }
                };
                // Destination: dynamic (resolved by the source) or default (from URL).
                let dest = msg.dest.unwrap_or_else(|| default_topic.clone());

                // Publish through the connector's pure I/O adapter.
                if let Err(_e) = sink.publish(&dest, &cfg, &msg.payload).await {
                    log_error!("pump_sink: failed to publish to '{}': {:?}", dest, _e);
                } else {
                    log_debug!("pump_sink: published to: {}", dest);
                }
            }

            log_info!(
                "pump_sink: publisher stopped for destination: {}",
                default_topic
            );
        }));
    }

    futures
}

/// Inbound pump: a single multiplexed reader future for `scheme`.
///
/// Drives one [`Source`] (never one task per topic), fanning each
/// `(topic, payload)` out to the matching producers via a [`Router`] built from
/// [`collect_inbound_routes`](AimDb::collect_inbound_routes).
///
/// Backpressure: [`Router::route`] drops + logs on a full producer buffer rather
/// than blocking, so one slow record never stalls the shared source. Route errors
/// are non-fatal.
///
/// [`Router`]: crate::router::Router
/// [`Router::route`]: crate::router::Router::route
pub fn pump_source(db: &AimDb, scheme: &str, mut src: impl Source + 'static) -> Vec<BoxFuture> {
    let routes = db.collect_inbound_routes(scheme);
    let router = Arc::new(RouterBuilder::from_routes(routes).build());
    let ctx = db.runtime_ctx();

    vec![Box::pin(async move {
        log_info!(
            "pump_source: reader started ({} topics)",
            router.resource_ids().len()
        );

        while let Some((topic, payload)) = src.next().await {
            // `route` deserializes and fans out to producers (synchronously —
            // the fused ingest path never awaits); it drops + logs on a full
            // producer buffer and never returns a fatal error.
            if let Err(_e) = router.route(&topic, &payload, &ctx) {
                log_error!(
                    "pump_source: failed to route message on '{}': {}",
                    topic,
                    _e
                );
            }
        }

        log_info!("pump_source: reader stopped");
    })]
}
