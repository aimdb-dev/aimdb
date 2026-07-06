//! Runtime-agnostic buffer traits
//!
//! Defines `Buffer<T>` (static trait) and `DynBuffer<T>` (trait object) for
//! buffer implementations. Adapters (tokio, embassy) provide concrete types.
//!
//! See `aimdb-tokio-adapter` and `aimdb-embassy-adapter` for implementations.

use core::task::{Context, Poll};

use alloc::boxed::Box;

use super::BufferCfg;
use crate::DbError;

/// Static buffer trait for concrete implementations
///
/// Provides push/subscribe operations for typed buffers. Readers are owned
/// and can outlive the subscription call (required for spawned tasks).
///
/// Trait bounds ensure thread-safety and `'static` lifetime for async runtimes.
///
/// See `aimdb_tokio_adapter::TokioRingBuffer` for implementation example.
pub trait Buffer<T: Clone + Send>: Send + Sync + 'static {
    /// Reader type for consuming values
    ///
    /// Each `subscribe()` call returns an independent owned reader.
    type Reader: BufferReader<T> + 'static;

    /// Creates a new buffer with the given configuration
    ///
    /// # Panics
    /// May panic if configuration is invalid (call `cfg.validate()` first)
    fn new(cfg: &BufferCfg) -> Self
    where
        Self: Sized;

    /// Push a value into the buffer (non-blocking)
    ///
    /// Behavior depends on buffer type:
    /// - **SPMC Ring**: Overwrites oldest value if full
    /// - **SingleLatest**: Overwrites previous value
    /// - **Mailbox**: Overwrites pending value if not consumed
    fn push(&self, value: T);

    /// Create a new independent reader for a consumer
    ///
    /// Each reader maintains its own position and can consume at its own pace.
    /// The returned reader is owned and can outlive this reference.
    ///
    /// # Fresh-subscriber semantics (SingleLatest)
    ///
    /// For a [`SingleLatest`](BufferCfg::SingleLatest) buffer, a fresh
    /// subscriber observes the buffer's current value once, as if it had been
    /// pushed right after subscription; if nothing has been produced yet, it
    /// waits. A late subscriber to state wants the current state, not just
    /// future changes. This rule holds identically across every runtime adapter
    /// and is enforced by
    /// [`test_support::assert_single_latest_contract`](super::test_support::assert_single_latest_contract).
    ///
    /// `SpmcRing` and `Mailbox` readers have no such replay: a fresh subscriber
    /// only sees values produced after it subscribes.
    fn subscribe(&self) -> Self::Reader;
}

/// Dynamic buffer trait for trait objects (object-safe)
///
/// Type-erased interface for buffers that can be stored as trait objects.
/// Automatically implemented for all `Buffer<T>` types via blanket impl.
///
/// Used when storing heterogeneous buffer types (e.g., in `TypedRecord`).
pub trait DynBuffer<T: Clone + Send>: Send + Sync {
    /// Push a value into the buffer (non-blocking)
    fn push(&self, value: T);

    /// Create a boxed reader for consuming values
    ///
    /// Returns a type-erased reader. Each reader maintains its own position.
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send>;

    /// Returns self as Any for downcasting to concrete buffer types
    fn as_any(&self) -> &dyn core::any::Any;

    /// Non-destructive read of the buffer's current value.
    ///
    /// Returns `Some(T)` if the buffer holds a current value that can be read
    /// without affecting any consumer's position. Returns `None` if the buffer
    /// type has no canonical "current value" concept (e.g., SPMC Ring) or if
    /// no value has been produced yet.
    ///
    /// This is the buffer-native point-in-time read used by AimX `record.get`
    ///. Implementations must not advance any reader position.
    ///
    /// The default returns `None`, which is the correct behaviour for buffers
    /// without a canonical latest value.
    fn peek(&self) -> Option<T> {
        None
    }

    /// Get buffer metrics snapshot (metrics feature only)
    ///
    /// Returns `Some(snapshot)` if the buffer implementation supports metrics,
    /// `None` otherwise. Default implementation returns `None`.
    #[cfg(feature = "observability")]
    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        None
    }

    /// Reset buffer metrics counters (metrics feature only)
    ///
    /// Default implementation is a no-op so buffers without metrics support are
    /// safe to call. Implementations that track counters should override this
    /// to zero them.
    #[cfg(feature = "observability")]
    fn reset_metrics(&self) {}
}

/// Non-blocking push error — carries the rejected value back to the caller.
///
/// Returned by [`Producer::try_produce`](crate::typed_api::Producer::try_produce).
/// Both variants own the value so the caller can retry, escalate, or drop it.
#[derive(Debug)]
pub enum TryProduceError<T> {
    /// Buffer is at capacity and configured not to overwrite. Transient.
    Full(T),
    /// Buffer / record has been torn down (e.g. shutdown). Terminal.
    Closed(T),
}

// `Display` describes the failure mode only; the rejected value is the payload,
// not part of the message, so this carries no `T: Display` bound.
impl<T> core::fmt::Display for TryProduceError<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Full(_) => f.write_str("buffer is at capacity and configured not to overwrite"),
            Self::Closed(_) => f.write_str("buffer or record has been torn down"),
        }
    }
}

// `Error: Debug + Display`; the derived `Debug` needs `T: Debug`, which every
// `Producer<T>` already requires. Gated on `std` to match the crate's other
// local error types (`PublishError`, `SerializeError`).
#[cfg(feature = "std")]
impl<T: core::fmt::Debug> std::error::Error for TryProduceError<T> {}

/// Write-side handle for a single record.
///
/// `Producer<T>` holds an `Arc<dyn WriteHandle<T>>` so it can be parameterised
/// over `T` alone — no runtime adapter `R` and no per-call record-key string
/// lookup on the produce hot path. The implementor (`RecordWriter<T>`)
/// pre-binds the underlying buffer, the latest-snapshot slot, and the metadata
/// tracker at build time.
///
/// Crate-private on purpose. `Producer<T>::new` is the only construction path;
/// external test code that needs a fake Producer should go through a future
/// `Producer::for_testing(...)` helper rather than implementing `WriteHandle`
/// directly.
pub(crate) trait WriteHandle<T: Clone + Send + 'static>: Send + Sync {
    /// Push a value into the buffer, update the latest-snapshot cache, and
    /// (when a buffer is present) mark the metadata `last_update` timestamp.
    /// Infallible — all three operations are synchronous and lock-free or
    /// spin-locked.
    fn push(&self, value: T);

    /// Default: delegate to `push` and return `Ok(())`. Overwriting buffers
    /// cannot fail, so the value is always accepted. Bounded / non-overwriting
    /// buffers override this to return `Full(value)` or `Closed(value)`.
    fn try_push(&self, value: T) -> Result<(), TryProduceError<T>> {
        self.push(value);
        Ok(())
    }
}

/// Reader trait for consuming values from a buffer
///
/// This is the object-safe **service-provider interface** that runtime adapters
/// implement. It is poll-based — and therefore object-safe and zero-allocation —
/// rather than `async`: an `async fn` on an erased trait forces a
/// `Pin<Box<dyn Future>>` heap allocation on every call.
/// Consumers do not call this directly; they use the [`Reader<T>`](super::Reader)
/// handle returned by `Consumer::subscribe`, whose `recv()` is `async` and wraps
/// [`poll_recv`](BufferReader::poll_recv) via `core::future::poll_fn` with no
/// allocation.
///
/// Each reader is independent with its own state.
///
/// # Error Handling
/// - `Ok(value)` - Successfully received a value
/// - `Err(BufferLagged)` - Missed messages (SPMC ring only, can continue)
/// - `Err(BufferClosed)` - Buffer closed (graceful shutdown)
///
/// # Contract (poll/try_recv interleaving and cancellation)
///
/// Every implementation in this workspace (tokio/embassy/wasm adapters)
/// conforms — this section makes the contract explicit for future
/// implementors.
///
/// 1. `try_recv` MAY be called between `Pending` `poll_recv` calls;
///    implementations MUST NOT lose values or wakeups across the
///    interleaving.
/// 2. A value claimed internally just before caller cancellation MUST be
///    delivered on the next `poll_recv`/`try_recv` call — this is why, e.g.,
///    the tokio broadcast/watch readers keep a persistent `ReusableBoxFuture`
///    across calls rather than dropping and recreating it per call.
/// 3. `poll_recv` MUST register the *latest* waker on every `Pending`
///    return; spurious wakes are permitted (a registered waker firing with
///    no corresponding new value is not a bug — callers must re-poll and
///    tolerate a no-op).
/// 4. Lag MUST surface as `Err(DbError::BufferLagged)` plus a metrics
///    `add_dropped` call on **both** `poll_recv` and `try_recv`.
pub trait BufferReader<T: Clone + Send>: Send {
    /// Poll for the next value.
    ///
    /// Returns `Poll::Ready(Ok(value))` when a value is available,
    /// `Poll::Ready(Err(..))` on lag/closure, or `Poll::Pending` after
    /// registering `cx.waker()` to be woken when the next value arrives.
    ///
    /// # Behavior by Buffer Type
    /// - **SPMC Ring**: Returns next value, or `Lagged(n)` if fell behind
    /// - **SingleLatest**: Waits for value change, returns most recent
    /// - **Mailbox**: Waits for slot value, takes and clears it
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>>;

    /// Non-blocking receive — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    ///
    /// # Behavior by Buffer Type
    /// - **SPMC Ring**: Returns next buffered value, or `BufferEmpty` if caught up
    /// - **SingleLatest**: Returns value if changed since last read, or `BufferEmpty`
    /// - **Mailbox**: Takes and returns slot value, or `BufferEmpty` if empty
    fn try_recv(&mut self) -> Result<T, DbError>;
}

/// Reader trait for consuming JSON-serialized values from a buffer
///
/// Type-erased reader that subscribes to a typed buffer and emits values as
/// `serde_json::Value`. Used by remote access protocol for subscriptions.
///
/// This trait enables subscribing to a buffer without knowing the concrete type `T`
/// at compile time, by serializing values to JSON on each poll.
///
/// Object-safe and poll-based for the same reason as [`BufferReader`].
/// Consumers use the [`JsonReader`](super::JsonReader) handle, whose
/// `recv_json()` is `async` and wraps [`poll_recv_json`](JsonBufferReader::poll_recv_json)
/// with no allocation.
///
/// # Requirements
/// - Record must be configured with `.with_remote_access()`
/// - Only available with the `remote` feature (requires serde_json)
#[cfg(feature = "remote")]
pub trait JsonBufferReader: Send {
    /// Poll for the next value, serialized to JSON.
    ///
    /// Returns `Poll::Ready(Ok(json))` when a value is available and
    /// serializes successfully, `Poll::Ready(Err(..))` on lag/closure/serialize
    /// failure, or `Poll::Pending` after registering `cx.waker()`.
    ///
    /// # Returns
    /// - `Ok(JsonValue)` - Successfully received and serialized value
    /// - `Err(BufferLagged)` - Missed messages (can continue reading)
    /// - `Err(BufferClosed)` - Buffer closed (graceful shutdown)
    /// - `Err(SerializationFailed)` - Failed to serialize value to JSON
    fn poll_recv_json(&mut self, cx: &mut Context<'_>) -> Poll<Result<serde_json::Value, DbError>>;

    /// Non-blocking receive as JSON — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError>;
}

/// Snapshot of buffer metrics at a point in time
///
/// Used for introspection and diagnostics. All counters are monotonically
/// increasing (except after reset).
#[cfg(feature = "observability")]
#[derive(Debug, Clone, Default)]
pub struct BufferMetricsSnapshot {
    /// Total items pushed to this buffer since creation
    pub produced_count: u64,

    /// Total items successfully consumed from this buffer (aggregate across all readers)
    pub consumed_count: u64,

    /// Total items dropped due to overflow/lag (SPMC ring only)
    ///
    /// **Note**: When multiple readers lag simultaneously on a broadcast buffer,
    /// each reader reports its own dropped count independently. This means the
    /// aggregate dropped_count may exceed the actual number of unique items that
    /// overflowed from the ring buffer (each lagged reader adds its own lag count).
    /// This is intentional: it reflects total "missed reads" across all consumers,
    /// which is useful for diagnosing per-consumer backpressure issues.
    pub dropped_count: u64,

    /// Current buffer occupancy: (items_in_buffer, capacity)
    /// Returns (0, 0) for SingleLatest/Mailbox where occupancy is not meaningful
    pub occupancy: (usize, usize),
}

/// Optional buffer metrics for introspection (std only, feature-gated)
///
/// Implemented by buffer types when the `observability` feature is enabled.
/// Provides counters for diagnosing producer-consumer imbalances.
#[cfg(feature = "observability")]
pub trait BufferMetrics {
    /// Get a snapshot of current buffer metrics
    ///
    /// Returns counters for produced, consumed, and dropped items,
    /// plus current buffer occupancy.
    fn metrics(&self) -> BufferMetricsSnapshot;

    /// Reset all metrics counters to zero
    ///
    /// Useful for windowed metrics collection. Note that this affects
    /// all observers of this buffer's metrics.
    fn reset_metrics(&self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    // Mock implementation for testing trait bounds
    struct MockBuffer<T: Clone + Send + Sync> {
        _phantom: core::marker::PhantomData<T>,
    }

    struct MockReader<T: Clone + Send> {
        _phantom: core::marker::PhantomData<T>,
    }

    impl<T: Clone + Send + Sync + 'static> Buffer<T> for MockBuffer<T> {
        type Reader = MockReader<T>;

        fn new(_cfg: &BufferCfg) -> Self {
            Self {
                _phantom: core::marker::PhantomData,
            }
        }

        fn push(&self, _value: T) {
            // No-op for testing
        }

        fn subscribe(&self) -> Self::Reader {
            MockReader {
                _phantom: core::marker::PhantomData,
            }
        }
    }

    // Explicit DynBuffer implementation for MockBuffer
    // (no blanket impl - adapters provide their own)
    impl<T: Clone + Send + Sync + 'static> DynBuffer<T> for MockBuffer<T> {
        fn push(&self, value: T) {
            <Self as Buffer<T>>::push(self, value)
        }

        fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
            Box::new(self.subscribe())
        }

        fn as_any(&self) -> &dyn core::any::Any {
            self
        }

        #[cfg(feature = "observability")]
        fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
            None // Mock doesn't track metrics
        }
    }

    impl<T: Clone + Send> BufferReader<T> for MockReader<T> {
        fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Result<T, DbError>> {
            // Return closed for testing
            Poll::Ready(Err(DbError::BufferClosed {
                buffer_name: "mock".to_string(),
            }))
        }

        fn try_recv(&mut self) -> Result<T, DbError> {
            Err(DbError::BufferEmpty)
        }
    }

    #[test]
    fn test_buffer_trait_bounds() {
        // Verify trait bounds compile
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<MockBuffer<i32>>();
        assert_sync::<MockBuffer<i32>>();
        assert_send::<MockReader<i32>>();
    }

    #[test]
    fn test_dyn_buffer_impl() {
        // Verify DynBuffer can be used as trait object
        let buffer = MockBuffer::<i32> {
            _phantom: core::marker::PhantomData,
        };

        // Should be able to use as DynBuffer
        let _: &dyn DynBuffer<i32> = &buffer;
    }

    // WriteHandle impl that always rejects with the buffer full. Used to verify that when
    // try_push fails, it returns the value that it failed to push.
    struct FullWriteHandle;

    impl WriteHandle<i32> for FullWriteHandle {
        fn push(&self, _value: i32) {}
        fn try_push(&self, value: i32) -> Result<(), TryProduceError<i32>> {
            Err(TryProduceError::Full(value))
        }
    }

    // WriteHandle impl that always rejects because the record is closed. Mirrors
    // `FullWriteHandle` for the terminal arm.
    struct ClosedWriteHandle;

    impl WriteHandle<i32> for ClosedWriteHandle {
        fn push(&self, _value: i32) {}
        fn try_push(&self, value: i32) -> Result<(), TryProduceError<i32>> {
            Err(TryProduceError::Closed(value))
        }
    }

    #[test]
    fn try_push_full_round_trips_value_through_producer() {
        use alloc::sync::Arc;
        let producer = crate::typed_api::Producer::new(Arc::new(FullWriteHandle));
        let result = producer.try_produce(42_i32);
        assert!(
            matches!(result, Err(TryProduceError::Full(42))),
            "expected Full(42), got {result:?}",
        );
    }

    #[test]
    fn try_push_closed_round_trips_value_through_producer() {
        use alloc::sync::Arc;
        let producer = crate::typed_api::Producer::new(Arc::new(ClosedWriteHandle));
        let result = producer.try_produce(42_i32);
        assert!(
            matches!(result, Err(TryProduceError::Closed(42))),
            "expected Closed(42), got {result:?}",
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn try_produce_error_displays_failure_mode_without_value() {
        use alloc::string::ToString;
        assert_eq!(
            TryProduceError::Full(42_i32).to_string(),
            "buffer is at capacity and configured not to overwrite",
        );
        assert_eq!(
            TryProduceError::Closed(42_i32).to_string(),
            "buffer or record has been torn down",
        );
    }
}
