//! Data-plane pump helpers.
//!
//! Two free functions that own the boilerplate a data-plane connector used to
//! hand-roll. The author writes only the pure I/O adapter — a
//! [`Connector`](crate::transport::Connector) (outbound) and a [`Source`]
//! (inbound) — and composes the helpers in `build()`:
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
use crate::connector::SerializerKind;
use crate::router::RouterBuilder;
use crate::transport::{Connector, ConnectorConfig};

/// Outbound pump: one publisher future per outbound route on `scheme`.
///
/// Extracts the consume-and-publish loop a data-plane connector used to write by
/// hand. For each route from [`collect_outbound_routes`](AimDb::collect_outbound_routes),
/// the returned future subscribes to the record (type-erased), serializes each
/// value with the route's [`SerializerKind`], resolves the destination via the
/// route's optional topic provider (falling back to the URL-derived default), and
/// publishes through `sink`. Per-route configuration (`qos`/`retain`/…) is built
/// once from the route's URL query via [`ConnectorConfig::from_query`].
///
/// The publisher future terminates when its subscription yields an error (e.g. the
/// record buffer closed), matching the legacy hand-rolled loop.
pub fn pump_sink<R>(db: &AimDb<R>, scheme: &str, sink: Arc<dyn Connector>) -> Vec<BoxFuture>
where
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    let routes = db.collect_outbound_routes(scheme);
    let mut futures: Vec<BoxFuture> = Vec::with_capacity(routes.len());

    for (default_topic, consumer, serializer, config, topic_provider) in routes {
        let sink = sink.clone();
        let runtime_ctx = db.runtime_any();
        let cfg = ConnectorConfig::from_query(&config);

        futures.push(Box::pin(async move {
            // Subscribe to typed values (type-erased).
            let mut reader = match consumer.subscribe_any().await {
                Ok(r) => r,
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        "pump_sink: failed to subscribe for destination '{}': {:?}",
                        default_topic,
                        _e
                    );
                    return;
                }
            };

            #[cfg(feature = "tracing")]
            tracing::info!(
                "pump_sink: publisher started for destination: {}",
                default_topic
            );

            loop {
                let value_any = match reader.recv_any().await {
                    Ok(v) => v,
                    // SPMC-ring overflow: messages were missed, but the reader
                    // recovers (cursor resets to the oldest live value). Skip the
                    // gap and keep pumping — a transient lag must not permanently
                    // kill the publisher.
                    Err(crate::DbError::BufferLagged { .. }) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("pump_sink: consumer lagged for '{}'", default_topic);
                        continue;
                    }
                    // Buffer closed / fatal — the record is gone; end the publisher.
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::info!(
                            "pump_sink: publisher stopping for '{}': {:?}",
                            default_topic,
                            _e
                        );
                        break;
                    }
                };
                // Resolve destination: dynamic (from provider) or default (from URL).
                let dest = topic_provider
                    .as_ref()
                    .and_then(|provider| provider.topic_any(&*value_any))
                    .unwrap_or_else(|| default_topic.clone());

                // Serialize the type-erased value.
                let bytes = match &serializer {
                    SerializerKind::Raw(ser) => match ser(&*value_any) {
                        Ok(b) => b,
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(
                                "pump_sink: failed to serialize for destination '{}': {:?}",
                                dest,
                                _e
                            );
                            continue;
                        }
                    },
                    SerializerKind::Context(ser) => match ser(runtime_ctx.clone(), &*value_any) {
                        Ok(b) => b,
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(
                                "pump_sink: failed to serialize for destination '{}': {:?}",
                                dest,
                                _e
                            );
                            continue;
                        }
                    },
                };

                // Publish through the connector's pure I/O adapter.
                if let Err(_e) = sink.publish(&dest, &cfg, &bytes).await {
                    #[cfg(feature = "tracing")]
                    tracing::error!("pump_sink: failed to publish to '{}': {:?}", dest, _e);
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("pump_sink: published to: {}", dest);
                }
            }

            #[cfg(feature = "tracing")]
            tracing::info!(
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
pub fn pump_source<R>(db: &AimDb<R>, scheme: &str, mut src: impl Source + 'static) -> Vec<BoxFuture>
where
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    let routes = db.collect_inbound_routes(scheme);
    let router = Arc::new(RouterBuilder::from_routes(routes).build());
    let ctx = db.runtime_any();

    vec![Box::pin(async move {
        #[cfg(feature = "tracing")]
        tracing::info!(
            "pump_source: reader started ({} topics)",
            router.resource_ids().len()
        );

        while let Some((topic, payload)) = src.next().await {
            // `route` deserializes and fans out to producers; it drops + logs on a
            // full producer buffer and never returns a fatal error.
            if let Err(_e) = router.route(&topic, &payload, Some(&ctx)).await {
                #[cfg(feature = "tracing")]
                tracing::error!(
                    "pump_source: failed to route message on '{}': {}",
                    topic,
                    _e
                );
            }
        }

        #[cfg(feature = "tracing")]
        tracing::info!("pump_source: reader stopped");
    })]
}
