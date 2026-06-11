//! Record registration extension: `.persist()` on [`RecordRegistrar`].

use std::sync::Arc;

use aimdb_core::typed_api::RecordRegistrar;

use crate::backend::PersistenceBackend;
use crate::builder_ext::PersistenceState;

/// Extension trait that adds `.persist()` to [`RecordRegistrar`].
///
/// `T: Serialize` is required so values can be converted to JSON for storage.
/// `.with_remote_access()` is **not** required — persistence subscribes to the
/// typed buffer directly.
pub trait RecordRegistrarPersistExt<'a, T>
where
    T: serde::Serialize + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Opt this record into persistence.
    ///
    /// Spawns a background subscriber (via `.tap()`) that serializes each
    /// value to JSON and writes it to the configured backend. Retention is
    /// managed by the cleanup task registered during `with_persistence()`.
    fn persist(&mut self, record_name: impl Into<String>) -> &mut RecordRegistrar<'a, T>;
}

impl<'a, T> RecordRegistrarPersistExt<'a, T> for RecordRegistrar<'a, T>
where
    T: serde::Serialize + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn persist(&mut self, record_name: impl Into<String>) -> &mut RecordRegistrar<'a, T> {
        let record_name: String = record_name.into();
        // Retrieve the backend from the builder's Extensions TypeMap, if configured.
        let backend: Option<Arc<dyn PersistenceBackend>> = self
            .extensions()
            .get::<PersistenceState>()
            .map(|s| s.backend.clone());

        // If no backend is configured, treat `.persist()` as a no-op so that
        // persistence remains optional and does not cause runtime panics.
        let Some(backend) = backend else {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Record '{}' marked for persistence, but no backend is configured via with_persistence(); .persist() will be a no-op",
                record_name
            );
            return self;
        };
        // Subscribe to the typed buffer as a tap (side-effect observer).
        // The runtime context isn't needed — persistence is runtime-agnostic.
        self.tap(move |_ctx, consumer| async move {
            let mut reader = consumer.subscribe();

            loop {
                let value = match reader.recv().await {
                    Ok(v) => v,
                    Err(aimdb_core::DbError::BufferLagged { .. }) => continue,
                    Err(_) => break,
                };

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
