//! Builder extension: `.with_persistence()` on [`AimDbBuilder`].

use std::sync::Arc;

use aimdb_core::builder::AimDbBuilder;
use aimdb_core::remote::{QueryHandlerFn, QueryHandlerParams};

use crate::backend::{PersistenceBackend, QueryParams};

/// State stored in the builder's [`Extensions`](aimdb_core::Extensions) TypeMap.
///
/// Both `.persist()` (on `RecordRegistrar`) and `AimDbQueryExt` (on `AimDb`)
/// retrieve this via `extensions().get::<PersistenceState>()`.
pub struct PersistenceState {
    /// The configured persistence backend.
    pub backend: Arc<dyn PersistenceBackend>,
    /// How long to keep persisted values before automatic cleanup.
    pub retention_ms: u64,
}

/// Extension trait that adds `.with_persistence()` to [`AimDbBuilder`].
pub trait AimDbBuilderPersistExt {
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

impl AimDbBuilderPersistExt for AimDbBuilder {
    fn with_persistence(
        mut self,
        backend: Arc<dyn PersistenceBackend>,
        retention: core::time::Duration,
    ) -> Self {
        let retention_ms = u64::try_from(retention.as_millis()).unwrap_or(u64::MAX);

        // Store backend + retention as a typed entry in the Extensions TypeMap.
        self.extensions_mut().insert(PersistenceState {
            backend: backend.clone(),
            retention_ms,
        });

        // Register a QueryHandlerFn so AimX record.query can delegate to us.
        // Result shape is the shared `{records, total}` vocabulary (design 045
        // §3.4) — one row type (`QueryRecord`) for every transport.
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

                let mut records: Vec<aimdb_core::remote::QueryRecord> = stored
                    .into_iter()
                    .map(|sv| aimdb_core::remote::QueryRecord {
                        topic: sv.record_name,
                        payload: sv.value,
                        ts: sv.stored_at,
                    })
                    .collect();
                records.sort_by_key(|r| r.ts);

                let total = records.len();
                Ok(serde_json::json!({
                    "records": records,
                    "total": total,
                }))
            })
        });
        self.extensions_mut().insert(handler);

        // Register a startup task for periodic retention cleanup.
        let backend_task = backend.clone();
        self.on_start(move |ctx: aimdb_core::RuntimeContext| async move {
            loop {
                // Calculate the cutoff: now minus retention window.
                let now = u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(u64::MAX);
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

                // Sleep 24 hours using the runtime's clock.
                ctx.time().sleep_secs(24 * 3600).await;
            }
        });

        self
    }
}
