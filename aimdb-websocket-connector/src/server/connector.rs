//! WebSocket outbound sink — the [`Connector`] adapter that `pump_sink` drives.
//!
//! Outbound record updates (`link_to("ws://…")`) fan out to subscribed clients
//! through the [`ClientManager`] bus. The shared
//! [`pump_sink`](aimdb_core::pump_sink) helper owns the consume → serialize →
//! publish loop (the same one MQTT uses); this adapter just routes each
//! serialized value to [`broadcast`](ClientManager::broadcast) and, when
//! late-join is enabled, caches it for snapshots.
//!
//! Inbound writes from WebSocket clients do **not** go through here — they ride
//! the session `Dispatch` (`WsSession::write` → the shared `Router`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};

use super::client_manager::ClientManager;

/// Shared late-join cache: topic → last serialized bytes.
pub(crate) type SnapshotCache = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Outbound sink: feeds each serialized record value into the broadcast bus.
pub(crate) struct WsBusSink {
    pub(crate) client_mgr: ClientManager,
    /// Late-join cache — `Some` only when late-join is on, so a disabled
    /// late-join does zero per-message snapshot work.
    pub(crate) snapshot: Option<SnapshotCache>,
}

impl Connector for WsBusSink {
    fn publish(
        &self,
        destination: &str,
        _config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Own the args so the returned future borrows only `&self` (the trait
        // binds the future's lifetime to the receiver, not the arguments).
        let dest = destination.to_string();
        let bytes = payload.to_vec();
        Box::pin(async move {
            if let Some(map) = &self.snapshot {
                map.lock().unwrap().insert(dest.clone(), bytes.clone());
            }
            // The bus carries raw record-value bytes tagged with the topic; the
            // per-connection AimX codec applies the `event` envelope downstream.
            self.client_mgr.broadcast(&dest, &bytes).await;
            Ok(())
        })
    }
}
