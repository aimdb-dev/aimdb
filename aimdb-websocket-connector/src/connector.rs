//! WebSocket connector implementation (`Connector` trait).
//!
//! [`WebSocketConnectorImpl`] is the live connector instance built by
//! [`crate::builder::WebSocketConnectorBuilder`].
//!
//! # Outbound publishing
//!
//! Each outbound route (`link_to("ws://…")`) gets a dedicated Tokio task:
//!
//! ```text
//! consumer.subscribe_any() → recv_any() → serializer() → ClientManager::broadcast()
//! ```
//!
//! # Inbound routing
//!
//! Inbound writes from WebSocket clients go through the shared [`Router`]
//! (same infrastructure as MQTT).  The `Connector::publish()` impl is a
//! no-op because WebSocket inbound happens via the session receive loop instead
//! of the standard publish path.

use std::{collections::HashMap, pin::Pin, sync::Arc};

use aimdb_core::OutboundRoute;

use crate::client_manager::ClientManager;

type BoxFuture = Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>>;

/// Live WebSocket connector returned by `build()` — owns the outbound publisher
/// tasks that feed the [`ClientManager`] bus. (Envelope vs raw-payload framing is
/// now the per-connection [`WsCodec`](crate::codec)'s job, not the broadcaster's.)
pub struct WebSocketConnectorImpl {
    pub(crate) client_mgr: ClientManager,
}

impl WebSocketConnectorImpl {
    pub(crate) fn new(client_mgr: ClientManager) -> Self {
        Self { client_mgr }
    }

    /// Collects one outbound publisher future per route.
    ///
    /// Each future:
    /// 1. Calls `consumer.subscribe_any()` to get a type-erased reader.
    /// 2. Loops calling `reader.recv_any()`.
    /// 3. Runs the serializer.
    /// 4. Broadcasts the bytes via `ClientManager::broadcast()`.
    pub(crate) fn collect_outbound_futures<R>(
        &self,
        db: &aimdb_core::builder::AimDb<R>,
        outbound_routes: Vec<OutboundRoute>,
        snapshot_map: Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>,
    ) -> Vec<BoxFuture>
    where
        R: aimdb_executor::RuntimeAdapter + 'static,
    {
        let runtime_ctx: Arc<dyn core::any::Any + Send + Sync> = db.runtime_any();
        let mut futures: Vec<BoxFuture> = Vec::with_capacity(outbound_routes.len());

        for (default_topic, consumer, serializer, _config, topic_provider) in outbound_routes {
            let client_mgr = self.client_mgr.clone();
            let snap = snapshot_map.clone();
            let default_topic_clone = default_topic.clone();
            let runtime_ctx = runtime_ctx.clone();

            futures.push(Box::pin(async move {
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "WS outbound: failed to subscribe for '{}': {:?}",
                            default_topic_clone,
                            _e
                        );
                        return;
                    }
                };

                #[cfg(feature = "tracing")]
                tracing::info!(
                    "WS outbound publisher started for topic: {}",
                    default_topic_clone
                );

                while let Ok(value_any) = reader.recv_any().await {
                    // Resolve topic (dynamic or static)
                    let topic = topic_provider
                        .as_ref()
                        .and_then(|p| p.topic_any(&*value_any))
                        .unwrap_or_else(|| default_topic_clone.clone());

                    // Serialize
                    let bytes = match &serializer {
                        aimdb_core::connector::SerializerKind::Raw(ser) => match ser(&*value_any) {
                            Ok(b) => b,
                            Err(_e) => {
                                #[cfg(feature = "tracing")]
                                tracing::error!(
                                    "WS outbound: serialize error for '{}': {:?}",
                                    topic,
                                    _e
                                );
                                continue;
                            }
                        },
                        aimdb_core::connector::SerializerKind::Context(ser) => {
                            match ser(runtime_ctx.clone(), &*value_any) {
                                Ok(b) => b,
                                Err(_e) => {
                                    #[cfg(feature = "tracing")]
                                    tracing::error!(
                                        "WS outbound: serialize error for '{}': {:?}",
                                        topic,
                                        _e
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    // Update snapshot cache for late-join
                    {
                        let mut map = snap.lock().unwrap();
                        map.insert(topic.clone(), bytes.clone());
                    }

                    // Fan-out to subscribed clients via the bus. The per-connection
                    // `WsCodec` applies the `Data` envelope (or, in raw mode, sends
                    // the bytes verbatim) — so the bus always carries raw bytes.
                    client_mgr.broadcast(&topic, &bytes).await;
                }

                #[cfg(feature = "tracing")]
                tracing::info!(
                    "WS outbound publisher stopped for topic: {}",
                    default_topic_clone
                );
            }));
        }

        futures
    }
}
