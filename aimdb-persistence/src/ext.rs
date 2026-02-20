//! Record registration extension: `.persist()` on [`RecordRegistrar`].

use std::sync::Arc;

use aimdb_core::typed_api::RecordRegistrar;
use aimdb_executor::Spawn;

use crate::backend::PersistenceBackend;
use crate::builder_ext::PersistenceState;

/// Extension trait that adds `.persist()` to [`RecordRegistrar`].
///
/// `T: Serialize` is required so values can be converted to JSON for storage.
/// `.with_remote_access()` is **not** required — persistence subscribes to the
/// typed buffer directly.
pub trait RecordRegistrarPersistExt<'a, T, R>
where
    T: serde::Serialize + Send + Sync + Clone + core::fmt::Debug + 'static,
    R: Spawn + 'static,
{
    /// Opt this record into persistence.
    ///
    /// Spawns a background subscriber (via `tap_raw`) that serializes each
    /// value to JSON and writes it to the configured backend. Retention is
    /// managed by the cleanup task registered during `with_persistence()`.
    fn persist(&'a mut self, record_name: String) -> &'a mut RecordRegistrar<'a, T, R>;
}

impl<'a, T, R> RecordRegistrarPersistExt<'a, T, R> for RecordRegistrar<'a, T, R>
where
    T: serde::Serialize + Send + Sync + Clone + core::fmt::Debug + 'static,
    R: Spawn + 'static,
{
    fn persist(&'a mut self, record_name: String) -> &'a mut RecordRegistrar<'a, T, R> {
        // Retrieve the backend from the builder's Extensions TypeMap.
        let backend: Arc<dyn PersistenceBackend> = self
            .extensions()
            .get::<PersistenceState>()
            .map(|s| s.backend.clone())
            .expect(".persist() called but no backend configured via with_persistence()");

        // Subscribe to the typed buffer as a tap (side-effect observer).
        // The second closure argument is the runtime context (Arc<dyn Any>),
        // which we don't need — persistence is runtime-agnostic.
        self.tap_raw(move |consumer, _ctx| async move {
            let mut reader = match consumer.subscribe() {
                Ok(r) => r,
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        "Persistence subscriber for '{}' failed to subscribe: {:?}",
                        record_name,
                        e
                    );
                    let _ = e;
                    return;
                }
            };

            while let Ok(value) = reader.recv().await {
                // T is known here — serialize directly, no with_remote_access() needed.
                let json = match serde_json::to_value(&value) {
                    Ok(v) => v,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Persistence: failed to serialize '{}': {}", record_name, e);
                        let _ = e;
                        continue;
                    }
                };

                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                if let Err(e) = backend.store(&record_name, &json, timestamp).await {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Persistence: failed to store '{}': {}", record_name, e);
                    let _ = e;
                }
            }

            #[cfg(feature = "tracing")]
            tracing::debug!(
                "Persistence subscriber for '{}' stopping (buffer closed)",
                record_name
            );
        })
    }
}
