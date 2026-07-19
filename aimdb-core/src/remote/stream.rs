//! Record-update streaming helper for AimX remote access.
//!
//! [`stream_record_updates`] adapts a record's [`JsonBufferReader`] into a
//! [`Stream`] of `(JSON value, skipped)` pairs, where `skipped` is the number
//! of updates the buffer dropped immediately before this one.
//! Cancellation is by `drop`; backpressure is the underlying buffer's
//! responsibility. The handler holds the
//! returned stream inside its per-subscription future so that the
//! connection's `FuturesUnordered` is the sole owner of the subscription's
//! lifecycle.

use alloc::string::ToString;

use crate::{AimDb, DbError, DbResult};

use futures_core::Stream;
use futures_util::stream::unfold;

/// Subscribe to a record and yield each update as a `(JSON value, skipped)`
/// pair.
///
/// The returned stream owns the underlying [`JsonBufferReader`]; dropping
/// it cancels the subscription. Lag (`BufferLagged`) is logged via `tracing`
/// and its `lag_count` is accumulated into the `skipped` count carried on the
/// *next* yielded value; `BufferClosed` and other errors terminate the stream cleanly (next `poll_next` returns `None`).
///
/// # Errors
/// - [`DbError::RecordKeyNotFound`] if no record matches `record_key`.
/// - [`DbError::InvalidRecordId`] if the resolved id has no storage.
/// - Any error returned by `subscribe_json()` (e.g. the record was not
///   configured with `.with_remote_access()`).
pub(crate) fn stream_record_updates(
    db: &AimDb,
    record_key: &str,
) -> DbResult<impl Stream<Item = (serde_json::Value, u64)> + Send + 'static> {
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
    // `skipped` accumulates `BufferLagged.lag_count` across consecutive lag
    // events so it can ride the next successfully-read value; it resets to 0
    // once a value is yielded.
    let state = (reader, record_key.to_string(), 0_u64);

    Ok(unfold(state, |(mut reader, key, mut skipped)| async move {
        loop {
            match reader.recv_json().await {
                Ok(value) => return Some(((value, skipped), (reader, key, 0))),
                Err(DbError::BufferLagged { lag_count, .. }) => {
                    log_warn!(
                        "stream_record_updates: record '{}' subscription lagged by {} messages",
                        key,
                        lag_count
                    );
                    skipped = skipped.saturating_add(lag_count);
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
    async fn unfold_carries_lag_count_and_terminates_on_closed() {
        let reader = JsonReader::new(Box::new(FakeReader {
            step: Arc::new(AtomicUsize::new(0)),
        }));

        // Mirrors `stream_record_updates`'s unfold: accumulate `lag_count` into
        // `skipped` and let it ride the next yielded value.
        let stream = unfold((reader, 0_u64), |(mut reader, mut skipped)| async move {
            loop {
                match reader.recv_json().await {
                    Ok(v) => return Some(((v, skipped), (reader, 0))),
                    Err(DbError::BufferLagged { lag_count, .. }) => {
                        skipped = skipped.saturating_add(lag_count);
                        continue;
                    }
                    Err(DbError::BufferClosed { .. }) => return None,
                    Err(_) => return None,
                }
            }
        });

        let values: Vec<_> = stream.collect().await;
        assert_eq!(values.len(), 2);
        // First value delivered cleanly; the Lagged(7) between the two folds
        // into the second value's `skipped`, not swallowed.
        assert_eq!(values[0], (serde_json::json!({"v": 1}), 0));
        assert_eq!(values[1], (serde_json::json!({"v": 2}), 7));
    }
}
