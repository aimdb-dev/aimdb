//! Builder extension: `.with_persistence()` on [`AimDbBuilder`].

use std::sync::Arc;

use aimdb_core::builder::AimDbBuilder;
use aimdb_core::remote::{QueryHandlerFn, QueryHandlerParams};
use aimdb_executor::{Spawn, TimeOps};

use crate::backend::{PersistenceBackend, QueryParams};

/// State stored in the builder's [`Extensions`](aimdb_core::Extensions) TypeMap.
///
/// Both `.persist()` (on `RecordRegistrar`) and `AimDbQueryExt` (on `AimDb<R>`)
/// retrieve this via `extensions().get::<PersistenceState>()`.
pub struct PersistenceState {
    /// The configured persistence backend.
    pub backend: Arc<dyn PersistenceBackend>,
    /// How long to keep persisted values before automatic cleanup.
    pub retention_ms: u64,
}

/// Extension trait that adds `.with_persistence()` to [`AimDbBuilder`].
pub trait AimDbBuilderPersistExt<R: Spawn + TimeOps> {
    /// Configures a persistence backend with a retention window.
    ///
    /// Stores the backend in the builder's `Extensions` TypeMap (accessible to
    /// `.persist()` and `AimDbQueryExt` methods) and registers an `on_start()`
    /// task that runs an initial cleanup sweep then repeats every 24 hours.
    ///
    /// Also registers a `QueryHandlerFn` in extensions so the AimX protocol's
    /// `record.query` method can delegate to the backend without importing
    /// persistence types in `aimdb-core`.
    ///
    /// # Arguments
    /// * `backend` - The persistence backend (e.g. `SqliteBackend`)
    /// * `retention` - How long to keep values (e.g. `Duration::from_secs(7 * 24 * 3600)`)
    fn with_persistence(
        self,
        backend: Arc<dyn PersistenceBackend>,
        retention: core::time::Duration,
    ) -> Self;
}

impl<R> AimDbBuilderPersistExt<R> for AimDbBuilder<R>
where
    R: Spawn + TimeOps + 'static,
{
    fn with_persistence(
        mut self,
        backend: Arc<dyn PersistenceBackend>,
        retention: core::time::Duration,
    ) -> Self {
        let retention_ms = retention.as_millis() as u64;

        // Store backend + retention as a typed entry in the Extensions TypeMap.
        self.extensions_mut().insert(PersistenceState {
            backend: backend.clone(),
            retention_ms,
        });

        // Register a QueryHandlerFn so AimX record.query can delegate to us.
        let query_backend = backend.clone();
        let handler: QueryHandlerFn = Box::new(move |params: QueryHandlerParams| {
            let backend = query_backend.clone();
            Box::pin(async move {
                let query_params = QueryParams {
                    limit_per_record: params.limit.or(Some(1)),
                    start_time: params.start,
                    end_time: params.end,
                };

                let stored = backend
                    .query(&params.name, query_params)
                    .await
                    .map_err(|e| e.to_string())?;

                let values: Vec<serde_json::Value> = stored
                    .into_iter()
                    .map(|sv| {
                        serde_json::json!({
                            "record": sv.record_name,
                            "value": sv.value,
                            "stored_at": sv.stored_at,
                        })
                    })
                    .collect();

                let count = values.len();
                Ok(serde_json::json!({
                    "values": values,
                    "count": count,
                }))
            })
        });
        self.extensions_mut().insert(handler);

        // Register a startup task for periodic retention cleanup.
        let backend_task = backend.clone();
        self.on_start(move |runtime: Arc<R>| async move {
            loop {
                // Calculate the cutoff: now minus retention window.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let cutoff = now.saturating_sub(retention_ms);

                match backend_task.cleanup(cutoff).await {
                    Ok(_deleted) => {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            "Persistence cleanup: deleted {} rows older than {}ms",
                            _deleted,
                            cutoff
                        );
                    }
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Persistence cleanup failed: {}", e);
                        #[cfg(not(feature = "tracing"))]
                        eprintln!("[aimdb-persistence] retention cleanup failed: {e}");
                    }
                }

                // Sleep 24 hours using the runtime's TimeOps.
                let day = runtime.secs(24 * 3600);
                runtime.sleep(day).await;
            }
        });

        self
    }
}
