//! Record-update streaming helper for AimX remote access.
//!
//! [`stream_record_updates`] adapts a record's [`JsonBufferReader`] into a
//! plain [`Stream`] of owned JSON [`Payload`](crate::session::Payload)s.
//! Cancellation is by `drop`; backpressure is the underlying buffer's
//! responsibility. The handler holds the returned stream inside its
//! per-subscription future so that the connection's `FuturesUnordered` is the
//! sole owner of the subscription's lifecycle.

use alloc::string::ToString;

use crate::{AimDb, DbError, DbResult};

use futures_core::Stream;
use futures_util::stream::unfold;

/// Subscribe to a record and yield each update as an opaque JSON payload.
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
pub(crate) fn stream_record_updates(
    db: &AimDb,
    record_key: &str,
) -> DbResult<impl Stream<Item = crate::session::Payload> + Send + 'static> {
    let inner = db.inner();
    let id = inner
        .resolve_str(record_key)
        .ok_or_else(|| DbError::record_key_not_found(record_key.to_string()))?;
    let record = inner
        .storage(id)
        .ok_or(DbError::InvalidRecordId { id: id.raw() })?;
    let reader = crate::buffer::JsonReader::new(
        record
            .json_access()
            .ok_or_else(|| {
                DbError::runtime_error(alloc::format!(
                    "Record '{record_key}' does not support JSON remote access"
                ))
            })?
            .subscribe_json()?,
    );

    // Pair the reader with an owned copy of the record key so lag/error
    // logs identify which record fell behind — the previous mpsc-based
    // path carried this via `record_metadata.name`, and dropping it
    // hides which subscription is misbehaving in mixed-record traces.
    let state = (reader, record_key.to_string());

    Ok(unfold(state, |(mut reader, key)| async move {
        loop {
            match reader.recv_json_bytes().await {
                Ok(bytes) => {
                    return Some((crate::session::Payload::from(bytes), (reader, key)));
                }
                Err(DbError::BufferLagged { lag_count, .. }) => {
                    log_warn!(
                        "stream_record_updates: record '{}' subscription lagged by {} messages",
                        key,
                        lag_count
                    );
                    continue;
                }
                Err(DbError::BufferClosed { .. }) => return None,
                Err(e) => {
                    log_error!(
                        "stream_record_updates: record '{}' terminating on error: {:?}",
                        key,
                        e
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
    use crate::buffer::{JsonBufferReader, JsonReader};
    use core::task::{Context, Poll};
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Fake reader that yields a configurable sequence: Ok, Lagged, Ok, Closed.
    /// Lets us exercise the skip-and-continue and end-of-stream branches
    /// without standing up an actual record + buffer.
    struct FakeReader {
        step: Arc<AtomicUsize>,
    }

    impl JsonBufferReader for FakeReader {
        fn poll_recv_json(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<serde_json::Value, DbError>> {
            let s = self.step.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(match s {
                0 => Ok(serde_json::json!({"v": 1})),
                1 => Err(DbError::BufferLagged {
                    buffer_name: "test".to_string(),
                    lag_count: 7,
                }),
                2 => Ok(serde_json::json!({"v": 2})),
                _ => Err(DbError::BufferClosed {
                    buffer_name: "test".to_string(),
                }),
            })
        }

        fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError> {
            Err(DbError::BufferEmpty)
        }
    }

    #[tokio::test]
    async fn unfold_skips_lag_and_terminates_on_closed() {
        let reader = JsonReader::new(Box::new(FakeReader {
            step: Arc::new(AtomicUsize::new(0)),
        }));

        let stream = unfold(reader, |mut reader| async move {
            loop {
                match reader.recv_json_bytes().await {
                    Ok(bytes) => return Some((bytes, reader)),
                    Err(DbError::BufferLagged { .. }) => continue,
                    Err(DbError::BufferClosed { .. }) => return None,
                    Err(_) => return None,
                }
            }
        });

        let values: Vec<_> = stream.collect().await;
        assert_eq!(values.len(), 2);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&values[0]).unwrap(),
            serde_json::json!({"v": 1})
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&values[1]).unwrap(),
            serde_json::json!({"v": 2})
        );
    }
}
