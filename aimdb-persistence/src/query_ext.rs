//! Query extensions on [`AimDb`] — `query_latest` and `query_range`.

use std::sync::Arc;

use aimdb_core::builder::AimDb;
use aimdb_executor::Spawn;
use serde::de::DeserializeOwned;

use crate::backend::{BoxFuture, PersistenceBackend, QueryParams};
use crate::builder_ext::PersistenceState;
use crate::error::PersistenceError;

/// Extension trait that adds persistence query methods to [`AimDb`].
///
/// Import `use aimdb_persistence::AimDbQueryExt;` to call `.query_latest()` /
/// `.query_range()` on a live `AimDb<R>` handle.
pub trait AimDbQueryExt {
    /// Query the latest N values per matching record.
    ///
    /// Pattern support: `"accuracy::*"` returns latest N from each matching
    /// record. Single record: `"accuracy::vienna"` returns latest N from that
    /// record only.
    ///
    /// Rows that fail to deserialize as `T` are **skipped** with a tracing
    /// warning rather than failing the entire query.
    fn query_latest<T: DeserializeOwned>(
        &self,
        record_pattern: &str,
        limit_per_record: usize,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>>;

    /// Query values within a time range for a single record or pattern.
    ///
    /// Pass `None` for `limit_per_record` to return all matching rows, or
    /// `Some(n)` to cap results per matching record name.
    ///
    /// Rows that fail to deserialize as `T` are **skipped** with a tracing
    /// warning.
    fn query_range<T: DeserializeOwned>(
        &self,
        record_pattern: &str,
        start_ts: u64,
        end_ts: u64,
        limit_per_record: Option<usize>,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>>;

    /// Query raw stored values (untyped, returns JSON).
    ///
    /// This is the non-generic variant used by the AimX `record.query` handler
    /// which doesn't know the concrete Rust type.
    fn query_raw(
        &self,
        record_pattern: &str,
        params: QueryParams,
    ) -> BoxFuture<'_, Result<Vec<crate::backend::StoredValue>, PersistenceError>>;
}

/// Helper: extract the backend from the `AimDb` extensions.
fn get_backend<R: Spawn + 'static>(
    db: &AimDb<R>,
) -> Result<Arc<dyn PersistenceBackend>, PersistenceError> {
    db.extensions()
        .get::<PersistenceState>()
        .map(|s| s.backend.clone())
        .ok_or(PersistenceError::NotConfigured)
}

impl<R: Spawn + 'static> AimDbQueryExt for AimDb<R> {
    fn query_latest<T: DeserializeOwned>(
        &self,
        record_pattern: &str,
        limit_per_record: usize,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>> {
        let pattern = record_pattern.to_string();
        Box::pin(async move {
            let backend = get_backend(self)?;

            let stored = backend
                .query(
                    &pattern,
                    QueryParams {
                        limit_per_record: Some(limit_per_record),
                        ..Default::default()
                    },
                )
                .await?;

            Ok(stored
                .into_iter()
                .filter_map(|sv| {
                    let crate::backend::StoredValue {
                        record_name: _record_name,
                        value,
                        ..
                    } = sv;
                    serde_json::from_value(value)
                        .map_err(|_e| {
                            #[cfg(feature = "tracing")]
                            tracing::warn!(
                                "Skipping persisted row for '{}': deserialization failed: {}",
                                _record_name,
                                _e
                            );
                        })
                        .ok()
                })
                .collect())
        })
    }

    fn query_range<T: DeserializeOwned>(
        &self,
        record_pattern: &str,
        start_ts: u64,
        end_ts: u64,
        limit_per_record: Option<usize>,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>> {
        let pattern = record_pattern.to_string();
        Box::pin(async move {
            let backend = get_backend(self)?;

            let stored = backend
                .query(
                    &pattern,
                    QueryParams {
                        start_time: Some(start_ts),
                        end_time: Some(end_ts),
                        limit_per_record,
                    },
                )
                .await?;

            Ok(stored
                .into_iter()
                .filter_map(|sv| {
                    let crate::backend::StoredValue {
                        record_name: _record_name,
                        value,
                        ..
                    } = sv;
                    serde_json::from_value(value)
                        .map_err(|_e| {
                            #[cfg(feature = "tracing")]
                            tracing::warn!(
                                "Skipping persisted row for '{}': deserialization failed: {}",
                                _record_name,
                                _e
                            );
                        })
                        .ok()
                })
                .collect())
        })
    }

    fn query_raw(
        &self,
        record_pattern: &str,
        params: QueryParams,
    ) -> BoxFuture<'_, Result<Vec<crate::backend::StoredValue>, PersistenceError>> {
        let pattern = record_pattern.to_string();
        Box::pin(async move {
            let backend = get_backend(self)?;
            backend.query(&pattern, params).await
        })
    }
}
