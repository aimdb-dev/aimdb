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

use aimdb_core::{
    transport::{ConnectorConfig, PublishError},
    OutboundRoute,
};

use crate::client_manager::ClientManager;

/// Live WebSocket connector returned by `build()`.
pub struct WebSocketConnectorImpl {
    pub(crate) client_mgr: ClientManager,
    /// When `true`, outbound data bypasses the `ServerMessage::Data` envelope
    /// and sends the serializer bytes directly as a WebSocket text frame.
    pub(crate) raw_payload: bool,
}

impl WebSocketConnectorImpl {
    pub(crate) fn new(client_mgr: ClientManager, raw_payload: bool) -> Self {
        Self {
            client_mgr,
            raw_payload,
        }
    }

    /// Spawn one Tokio task per outbound route.
    ///
    /// Each task:
    /// 1. Calls `consumer.subscribe_any()` to get a type-erased reader.
    /// 2. Loops calling `reader.recv_any()`.
    /// 3. Runs the serializer.
    /// 4. Broadcasts the bytes via `ClientManager::broadcast()`.
    pub(crate) fn spawn_outbound_publishers<R>(
        &self,
        db: &aimdb_core::builder::AimDb<R>,
        outbound_routes: Vec<OutboundRoute>,
        snapshot_map: Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>,
    ) -> aimdb_core::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let runtime = db.runtime();
        let raw_payload = self.raw_payload;

        for (default_topic, consumer, serializer, _config, topic_provider) in outbound_routes {
            let client_mgr = self.client_mgr.clone();
            let snap = snapshot_map.clone();
            let default_topic_clone = default_topic.clone();

            runtime.spawn(async move {
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
                    let bytes = match serializer(&*value_any) {
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
                    };

                    // Update snapshot cache for late-join
                    {
                        let mut map = snap.lock().unwrap();
                        map.insert(topic.clone(), bytes.clone());
                    }

                    // Fan-out to subscribed clients
                    if raw_payload {
                        client_mgr.broadcast_raw(&topic, &bytes).await;
                    } else {
                        client_mgr.broadcast(&topic, &bytes).await;
                    }
                }

                #[cfg(feature = "tracing")]
                tracing::info!(
                    "WS outbound publisher stopped for topic: {}",
                    default_topic_clone
                );
            })?;
        }

        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════
// Connector trait
// ════════════════════════════════════════════════════════════════════

impl aimdb_core::transport::Connector for WebSocketConnectorImpl {
    /// WebSocket inbound is driven by the session receive loop, not by
    /// `publish()`.  This implementation exists only to satisfy the trait and
    /// will never be called in normal operation.
    fn publish(
        &self,
        _destination: &str,
        _config: &ConnectorConfig,
        _payload: &[u8],
    ) -> Pin<Box<dyn core::future::Future<Output = Result<(), PublishError>> + Send + '_>> {
        Box::pin(async move { Ok(()) })
    }
}
