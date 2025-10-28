//! Runtime-agnostic buffer traits
//!
//! Defines `Buffer<T>` (static trait) and `DynBuffer<T>` (trait object) for
//! buffer implementations. Adapters (tokio, embassy) provide concrete types.
//!
//! See `aimdb-tokio-adapter` and `aimdb-embassy-adapter` for implementations.

use core::future::Future;
use core::pin::Pin;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

#[cfg(feature = "std")]
use std::boxed::Box;

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
}

/// Reader trait for consuming values from a buffer
///
/// All read operations are async. Each reader is independent with its own state.
///
/// # Error Handling
/// - `Ok(value)` - Successfully received a value
/// - `Err(BufferLagged)` - Missed messages (SPMC ring only, can continue)
/// - `Err(BufferClosed)` - Buffer closed (graceful shutdown)
pub trait BufferReader<T: Clone + Send>: Send {
    /// Receive the next value (async)
    ///
    /// Waits for the next available value. Returns immediately if buffered.
    ///
    /// # Behavior by Buffer Type
    /// - **SPMC Ring**: Returns next value, or `Lagged(n)` if fell behind
    /// - **SingleLatest**: Waits for value change, returns most recent
    /// - **Mailbox**: Waits for slot value, takes and clears it
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>>;
}

/// Blanket implementation of DynBuffer for all Buffer types
impl<T, B> DynBuffer<T> for B
where
    T: Clone + Send + 'static,
    B: Buffer<T>,
{
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    impl<T: Clone + Send> BufferReader<T> for MockReader<T> {
        fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>> {
            Box::pin(async {
                // Return closed for testing
                Err(DbError::BufferClosed {
                    #[cfg(feature = "std")]
                    buffer_name: "mock".to_string(),
                    #[cfg(not(feature = "std"))]
                    _buffer_name: (),
                })
            })
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
    fn test_dyn_buffer_blanket_impl() {
        // Verify DynBuffer is automatically implemented
        let buffer = MockBuffer::<i32> {
            _phantom: core::marker::PhantomData,
        };

        // Should be able to use as DynBuffer
        let _: &dyn DynBuffer<i32> = &buffer;
    }
}
