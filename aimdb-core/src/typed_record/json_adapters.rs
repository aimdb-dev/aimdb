use alloc::{boxed::Box, vec::Vec};
use core::task::{Context, Poll};

use super::record::RecordCodec;

/// Type-erased JSON read/subscribe/write for remote access.
///
/// Internal to the remote-access protocol — application code reads values
/// via `record.latest()?.as_json()` instead. Obtained from a record through
/// [`AnyRecord::json_access`](crate::AnyRecord::json_access), which is where the `remote` cfg-gate
/// lives for consumers.
///
/// Consumers: `AimDbInner::try_latest_as_json` / `set_record_from_json`
/// (`record.get` / `record.set`), the AimX session dispatch (`record.subscribe`
/// value drain), and `remote::stream::stream_record_updates`.
pub trait JsonRecordAccess {
    /// Returns JSON for type-erased remote access
    ///
    /// Used internally by remote access protocol. **Users should use `record.latest()?.as_json()`.**
    fn latest_json(&self) -> Option<crate::Stamped<serde_json::Value>>;

    /// Returns owned JSON bytes for type-erased remote access.
    ///
    /// Existing implementations inherit a compatibility path through
    /// [`latest_json`](Self::latest_json), keeping its write metadata.
    /// `TypedRecord<T>` overrides this and asks its codec to encode `T`
    /// directly.
    fn latest_json_bytes(&self) -> Option<crate::Stamped<Vec<u8>>> {
        crate::buffer::json_tree_to_bytes(self.latest_json()?).ok()
    }

    /// Subscribe to record updates as JSON stream
    ///
    /// Creates a type-erased subscription that emits `serde_json::Value` instead of
    /// the concrete type `T`. This enables subscribing to a record without knowing
    /// its type at compile time.
    ///
    /// Used internally by remote access protocol for `record.subscribe` functionality.
    ///
    /// # Returns
    /// - `Ok(Box<dyn JsonBufferReader>)` - Successfully created subscription
    /// - `Err(DbError)` - If serialization not configured or buffer subscription failed
    ///
    /// # Errors
    /// Returns error if:
    /// - Record not configured with `.with_remote_access()`
    /// - Buffer subscription fails (shouldn't happen in practice)
    fn subscribe_json(&self) -> crate::DbResult<Box<dyn crate::buffer::JsonBufferReader + Send>>;

    /// Sets a record value from JSON
    ///
    /// Deserializes JSON and produces the value to the record's buffer.
    ///
    /// **SAFETY:** This method enforces the "No Producer Override" rule:
    /// - Returns error if `producer_count > 0` (prevents overriding application logic)
    /// - Only configuration records (no producers) should be settable via remote access
    ///
    /// Used internally by remote access protocol for `record.set` functionality.
    ///
    /// # Arguments
    /// * `json_value` - JSON representation of the value to set
    ///
    /// # Returns
    /// - `Ok(())` - Successfully set the value
    /// - `Err(DbError)` - If deserialization fails, producers exist, or no buffer
    ///
    /// # Errors
    /// Returns error if:
    /// - Record has active producers (`producer_count > 0`) - **safety check**
    /// - JSON deserialization fails (schema mismatch)
    /// - Record not configured with buffer
    /// - Record not configured with `.with_remote_access()`
    fn set_from_json(&self, json_value: serde_json::Value) -> crate::DbResult<()>;

    /// Sets a record from one complete JSON value encoded as bytes.
    ///
    /// Existing implementations inherit a parser-backed compatibility path
    /// through [`set_from_json`](Self::set_from_json). This accepts the exact
    /// record value, not an AimX request/write wrapper; the wire dispatch keeps
    /// wrapper extraction on its existing tree path.
    fn set_from_json_bytes(&self, bytes: &[u8]) -> crate::DbResult<()> {
        let value = serde_json::from_slice(bytes)
            .map_err(|_| crate::DbError::runtime_error("Failed to parse JSON value"))?;
        self.set_from_json(value)
    }
}

/// Adapter that wraps a typed `BufferReader` and serializes values to JSON.
///
/// Bridges the gap between typed buffers and type-erased JSON streaming for
/// remote access subscriptions. Each receive:
/// 1. Receives one `Stamped<T>` from the buffer
/// 2. Serializes its value through the configured codec
/// 3. Returns either a JSON tree (compatibility API) or owned JSON bytes,
///    with the write metadata of that same sample
///
/// Used internally by `TypedRecord::subscribe_json()`.
pub(super) struct JsonReaderAdapter<T: Clone + Send + 'static> {
    /// The underlying typed buffer reader
    inner: Box<dyn crate::buffer::BufferReader<crate::Stamped<T>> + Send>,
    /// JSON codec (from .with_remote_access())
    codec: RecordCodec<T>,
}

impl<T: Clone + Send + 'static> JsonReaderAdapter<T> {
    /// Wraps a typed buffer reader; values are encoded through `codec`.
    pub(super) fn new(
        inner: Box<dyn crate::buffer::BufferReader<crate::Stamped<T>> + Send>,
        codec: RecordCodec<T>,
    ) -> Self {
        Self { inner, codec }
    }

    /// Encodes one received sample, keeping its write metadata whole.
    fn encode<U>(
        &self,
        sample: crate::Stamped<T>,
        encode: impl FnOnce(&RecordCodec<T>, &T) -> Option<U>,
    ) -> Result<crate::Stamped<U>, crate::DbError> {
        sample.as_ref().try_map(|value| {
            encode(&self.codec, value)
                .ok_or_else(|| crate::DbError::runtime_error("Failed to serialize value to JSON"))
        })
    }
}

impl<T: Clone + Send + 'static> crate::buffer::JsonBufferReader for JsonReaderAdapter<T> {
    fn poll_recv_json(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<crate::Stamped<serde_json::Value>, crate::DbError>> {
        // Poll the inner typed reader (allocation-free), then serialize on the
        // ready value — no per-message boxing.
        match self.inner.poll_recv(cx) {
            Poll::Ready(Ok(sample)) => Poll::Ready(self.encode(sample, |c, v| c.encode(v))),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn try_recv_json(&mut self) -> Result<crate::Stamped<serde_json::Value>, crate::DbError> {
        let sample = self.inner.try_recv()?;
        self.encode(sample, |c, v| c.encode(v))
    }

    fn poll_recv_json_bytes(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<crate::Stamped<Vec<u8>>, crate::DbError>> {
        match self.inner.poll_recv(cx) {
            Poll::Ready(Ok(sample)) => Poll::Ready(self.encode(sample, |c, v| c.encode_to_vec(v))),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn try_recv_json_bytes(&mut self) -> Result<crate::Stamped<Vec<u8>>, crate::DbError> {
        let sample = self.inner.try_recv()?;
        self.encode(sample, |c, v| c.encode_to_vec(v))
    }
}
