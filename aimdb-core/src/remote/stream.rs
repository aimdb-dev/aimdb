//! Record-update streaming helper for AimX remote access.
//!
//! [`stream_record_updates`] adapts a record's [`JsonBufferReader`] into a
//! plain [`Stream`] of JSON values. Cancellation is by `drop`; backpressure
//! is the underlying buffer's responsibility. The handler holds the
//! returned stream inside its per-subscription future so that the
//! connection's `FuturesUnordered` is the sole owner of the subscription's
//! lifecycle.

#[cfg(feature = "std")]
use crate::{AimDb, DbError, DbResult};

#[cfg(feature = "std")]
use futures_core::Stream;
#[cfg(feature = "std")]
use futures_util::stream::unfold;

/// Subscribe to a record and yield each update as a JSON value.
///
/// The returned stream owns the underlying [`JsonBufferReader`]; dropping
/// it cancels the subscription. Lag (`BufferLagged`) is logged via
/// `tracing` and skipped; `BufferClosed` and other errors terminate the
/// stream cleanly (next `poll_next` returns `None`).
///
/// # Errors
/// - [`DbError::RecordKeyNotFound`] if no record matches `record_key`.
/// - [`DbError::InvalidRecordId`] if the resolved id has no storage.
/// - Any error returned by `subscribe_json()` (e.g. the record was not
///   configured with `.with_remote_access()`).
#[cfg(feature = "std")]
pub(crate) fn stream_record_updates<R>(
    db: &AimDb<R>,
    record_key: &str,
) -> DbResult<impl Stream<Item = serde_json::Value> + Send + 'static>
where
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    let inner = db.inner();
    let id = inner
        .resolve_str(record_key)
        .ok_or_else(|| DbError::RecordKeyNotFound {
            key: record_key.to_string(),
        })?;
    let record = inner
        .storage(id)
        .ok_or(DbError::InvalidRecordId { id: id.raw() })?;
    let reader = record.subscribe_json()?;

    // Pair the reader with an owned copy of the record key so lag/error
    // logs identify which record fell behind — the previous mpsc-based
    // path carried this via `record_metadata.name`, and dropping it
    // hides which subscription is misbehaving in mixed-record traces.
    let state = (reader, record_key.to_string());

    Ok(unfold(state, |(mut reader, _key)| async move {
        loop {
            match reader.recv_json().await {
                Ok(value) => return Some((value, (reader, _key))),
                Err(DbError::BufferLagged {
                    lag_count: _lag_count,
                    ..
                }) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        record = %_key,
                        "stream_record_updates: subscription lagged by {} messages",
                        _lag_count
                    );
                    continue;
                }
                Err(DbError::BufferClosed { .. }) => return None,
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        record = %_key,
                        "stream_record_updates: terminating on error: {:?}",
                        _e
                    );
                    return None;
                }
            }
        }
    }))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::buffer::JsonBufferReader;
    use futures_util::StreamExt;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Fake reader that yields a configurable sequence: Ok, Lagged, Ok, Closed.
    /// Lets us exercise the skip-and-continue and end-of-stream branches
    /// without standing up an actual record + buffer.
    struct FakeReader {
        step: Arc<AtomicUsize>,
    }

    impl JsonBufferReader for FakeReader {
        fn recv_json(
            &mut self,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<serde_json::Value, DbError>> + Send + '_>,
        > {
            let step = self.step.clone();
            Box::pin(async move {
                let s = step.fetch_add(1, Ordering::SeqCst);
                match s {
                    0 => Ok(serde_json::json!({"v": 1})),
                    1 => Err(DbError::BufferLagged {
                        buffer_name: "test".to_string(),
                        lag_count: 7,
                    }),
                    2 => Ok(serde_json::json!({"v": 2})),
                    _ => Err(DbError::BufferClosed {
                        buffer_name: "test".to_string(),
                    }),
                }
            })
        }

        fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError> {
            Err(DbError::BufferEmpty)
        }
    }

    #[tokio::test]
    async fn unfold_skips_lag_and_terminates_on_closed() {
        let reader: Box<dyn JsonBufferReader + Send> = Box::new(FakeReader {
            step: Arc::new(AtomicUsize::new(0)),
        });

        let stream = unfold(reader, |mut reader| async move {
            loop {
                match reader.recv_json().await {
                    Ok(v) => return Some((v, reader)),
                    Err(DbError::BufferLagged { .. }) => continue,
                    Err(DbError::BufferClosed { .. }) => return None,
                    Err(_) => return None,
                }
            }
        });

        let values: Vec<_> = stream.collect().await;
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], serde_json::json!({"v": 1}));
        assert_eq!(values[1], serde_json::json!({"v": 2}));
    }
}
