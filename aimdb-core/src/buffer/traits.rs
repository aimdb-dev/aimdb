//! Runtime-agnostic buffer traits
//!
//! Defines the trait interface that buffer implementations must satisfy.
//! Actual implementations are provided by adapter crates (tokio, embassy).

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

/// Dyn-compatible sender interface for buffers
///
/// This trait provides a type-erased interface for sending values to buffers,
/// allowing buffers to be stored as trait objects (`Box<dyn BufferSender<T>>`).
///
/// Unlike `BufferBackend`, this trait does not include reader operations or
/// associated types with lifetimes, making it object-safe.
///
/// # Object Safety
///
/// This trait is dyn-compatible because:
/// - All methods take `&self` (not `Self`)
/// - No associated types or generics in methods
/// - Uses boxed futures for async operations
///
/// # Usage
///
/// Used in `TypedRecord` to store heterogeneous buffer types without
/// requiring concrete types or feature flags.
///
/// # Example
///
/// ```rust,ignore
/// let buffer: Box<dyn BufferSender<SensorData>> = Box::new(tokio_buffer);
/// buffer.send(SensorData { temp: 23.5 }).await?;
/// ```
pub trait BufferSender<T: Clone + Send>: Send + Sync {
    /// Sends a value into the buffer (async, non-blocking)
    ///
    /// Returns an error if the buffer is closed or encounters an error.
    ///
    /// # Arguments
    /// * `value` - The value to send
    ///
    /// # Returns
    /// `Ok(())` if value was enqueued, `Err(DbError)` otherwise
    fn send(&self, value: T) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + '_>>;

    /// Returns self as Any for downcasting to concrete buffer types
    ///
    /// This enables adapter-specific operations (like subscription) on buffers
    /// stored as trait objects.
    ///
    /// # Returns
    /// `&dyn Any` for use with `downcast_ref()`
    fn as_any(&self) -> &dyn core::any::Any;
}

/// Backend-agnostic buffer trait
///
/// This trait defines the interface for buffer implementations across different
/// async runtimes (Tokio, Embassy, etc.). It provides non-blocking push and
/// subscription operations.
///
/// # Design Philosophy
///
/// - **Runtime Agnostic**: Works with any async runtime
/// - **Type Safe**: Generic over item type `T`
/// - **Clone Safe**: Buffers can be shared across threads
/// - **Non-Blocking**: Push operations never block
///
/// # Implementation Requirements
///
/// Implementations must:
/// 1. Support concurrent access from multiple threads
/// 2. Provide independent readers via `subscribe()`
/// 3. Handle overflow according to `BufferCfg` semantics
/// 4. Be `Send + Sync` for multi-threaded use
///
/// # Examples
///
/// Implementing for a custom runtime:
///
/// ```rust,ignore
/// use aimdb_core::buffer::{BufferBackend, BufferReader, BufferCfg};
/// use aimdb_core::DbError;
///
/// struct MyBuffer<T> {
///     inner: MyRuntimeChannel<T>,
/// }
///
/// impl<T: Clone + Send> BufferBackend<T> for MyBuffer<T> {
///     type Reader = MyReader<T>;
///     
///     fn new(cfg: &BufferCfg) -> Self {
///         // Create appropriate channel based on config
///         todo!()
///     }
///     
///     fn push(&self, value: T) {
///         // Non-blocking enqueue
///         self.inner.send(value);
///     }
///     
///     fn subscribe(&self) -> Self::Reader {
///         // Create independent reader
///         MyReader {
///             rx: self.inner.subscribe(),
///         }
///     }
/// }
/// ```
pub trait BufferBackend<T: Clone + Send>: Send + Sync + 'static {
    /// Reader type for consuming values
    ///
    /// Each call to `subscribe()` returns an independent owned reader that can
    /// consume values at its own pace.
    ///
    /// The reader is owned (not borrowed) and can outlive the reference used
    /// to create it. This enables universal subscription support across all
    /// runtimes (Tokio, Embassy, etc.) by using Arc or static references internally.
    type Reader: BufferReader<T> + 'static;

    /// Creates a new buffer with the given configuration
    ///
    /// # Arguments
    /// * `cfg` - Buffer configuration specifying capacity and behavior
    ///
    /// # Returns
    /// A new buffer instance
    ///
    /// # Panics
    /// May panic if configuration is invalid (call `cfg.validate()` first)
    fn new(cfg: &BufferCfg) -> Self
    where
        Self: Sized;

    /// Push a value into the buffer (non-blocking)
    ///
    /// This method never blocks, regardless of buffer state:
    /// - **SPMC Ring**: Overwrites oldest value if full
    /// - **SingleLatest**: Overwrites previous value
    /// - **Mailbox**: Overwrites pending value if not consumed
    ///
    /// # Arguments
    /// * `value` - The value to enqueue
    ///
    /// # Concurrency
    /// This method is thread-safe and can be called concurrently from multiple
    /// threads, though only one producer should call it per buffer (by convention).
    ///
    /// # Example
    /// ```rust,ignore
    /// buffer.push(SensorData { temp: 23.5 });
    /// // Returns immediately, consumers will receive asynchronously
    /// ```
    fn push(&self, value: T);

    /// Create a new independent reader for a consumer
    ///
    /// Each reader maintains its own position in the buffer and can consume
    /// values at its own pace. For SPMC ring buffers, readers track their
    /// position independently and may lag if processing is slow.
    ///
    /// The returned reader is owned and can outlive the buffer reference.
    /// Implementations use Arc or static references internally to achieve this.
    ///
    /// # Returns
    /// A new owned reader instance
    ///
    /// # Example
    /// ```rust,ignore
    /// let reader1 = buffer.subscribe(); // Consumer 1
    /// let reader2 = buffer.subscribe(); // Consumer 2 (independent)
    /// ```
    fn subscribe(&self) -> Self::Reader;
}

/// Reader trait for consuming values from a buffer
///
/// This trait defines the async interface for reading values from a buffer.
/// Each reader is independent and can progress at its own rate.
///
/// # Design Philosophy
///
/// - **Async**: All read operations are async (non-blocking)
/// - **Independent**: Each reader has its own state
/// - **Lag Tolerant**: Readers can detect when they fall behind (SPMC)
/// - **Graceful Shutdown**: Closed channel errors enable clean exit
///
/// # Examples
///
/// Typical consumer loop:
///
/// ```rust,ignore
/// use aimdb_core::buffer::BufferReader;
/// use aimdb_core::DbError;
///
/// async fn consumer_loop<R: BufferReader<Data>>(mut reader: R) {
///     loop {
///         match reader.recv().await {
///             Ok(item) => {
///                 // Process the item
///                 process(item).await;
///             }
///             Err(DbError::BufferLagged { lag_count, .. }) => {
///                 // Log lag and continue
///                 tracing::warn!("Lagged by {} messages", lag_count);
///                 continue;
///             }
///             Err(DbError::BufferClosed { .. }) => {
///                 // Graceful exit
///                 break;
///             }
///             Err(e) => {
///                 // Other errors
///                 tracing::error!("Buffer error: {}", e);
///                 break;
///             }
///         }
///     }
/// }
/// ```
pub trait BufferReader<T: Clone + Send>: Send {
    /// Receive the next value (async)
    ///
    /// This method waits asynchronously for the next value to become available.
    /// It will return immediately if a value is already buffered.
    ///
    /// # Returns
    /// - `Ok(value)` - Successfully received a value
    /// - `Err(DbError::BufferLagged { lag_count, .. })` - Missed messages (SPMC ring only)
    /// - `Err(DbError::BufferClosed { .. })` - Buffer is closed (shutdown)
    ///
    /// # Behavior by Buffer Type
    ///
    /// ## SPMC Ring
    /// - Returns next available value from the ring
    /// - Returns `Lagged(n)` if consumer fell behind
    /// - Consumer resumes from current position after lag
    ///
    /// ## SingleLatest
    /// - Waits for a value to be set
    /// - Returns most recent value when changed
    /// - Never returns `Lagged` (intermediate values are skipped)
    ///
    /// ## Mailbox
    /// - Waits for a value in the slot
    /// - Takes the value (clears slot)
    /// - Never returns `Lagged`
    ///
    /// # Cancellation Safety
    /// Implementations should be cancellation-safe. If the future is dropped,
    /// no value should be lost (for SingleLatest/Mailbox) or the reader should
    /// be in a valid state for the next call (SPMC ring).
    ///
    /// # Example
    /// ```rust,ignore
    /// match reader.recv().await {
    ///     Ok(data) => println!("Received: {:?}", data),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>>;
}

/// Trait for buffers that support subscription (creating new readers)
///
/// This trait extends `BufferSender` with the ability to create new independent
/// readers (subscribers) from the buffer. This is used by adapter-specific
/// subscription mechanisms to provide stream-based APIs.
///
/// # Object Safety
///
/// This trait is dyn-compatible because:
/// - All methods take `&self` (not `Self`)
/// - The reader type is boxed and type-erased
/// - Uses boxed trait objects for readers
///
/// # Usage
///
/// Adapters implement this trait alongside `BufferSender` to enable
/// subscription capabilities. The core crate remains agnostic to specific
/// reader implementations.
///
/// # Example
///
/// ```rust,ignore
/// // In adapter crate (e.g., aimdb-tokio-adapter)
/// impl<T: Clone + Send + Sync + 'static> BufferSubscribable<T> for TokioBuffer<T> {
///     fn subscribe_reader(&self) -> Box<dyn BufferReader<T> + Send> {
///         Box::new(self.subscribe())
///     }
/// }
///
/// // In application code
/// let buffer: Box<dyn BufferSubscribable<T>> = Box::new(tokio_buffer);
/// let reader = buffer.subscribe_reader();
/// ```
pub trait BufferSubscribable<T: Clone + Send>: BufferSender<T> {
    /// Creates a new independent reader for consuming values
    ///
    /// Each reader maintains its own position in the buffer and can consume
    /// values at its own pace. For SPMC ring buffers, readers track their
    /// position independently and may lag if processing is slow.
    ///
    /// # Returns
    /// A boxed `BufferReader<T>` instance
    ///
    /// # Example
    /// ```rust,ignore
    /// let reader = buffer.subscribe_reader();
    /// while let Ok(value) = reader.recv().await {
    ///     process(value).await;
    /// }
    /// ```
    fn subscribe_reader(&self) -> Box<dyn BufferReader<T> + Send>;

    /// Attempts to downcast to a concrete buffer type for stream creation
    ///
    /// This method provides a way to access the underlying concrete buffer
    /// type for creating adapter-specific streams. Returns None if the buffer
    /// is not of the expected concrete type.
    ///
    /// # Returns
    /// `&dyn Any` for downcasting to concrete types
    fn as_any_buffer(&self) -> &dyn core::any::Any;
}

/// Helper trait for type-erased buffer operations
///
/// This trait allows storing buffers of different types in a single collection
/// while maintaining type safety through downcasting.
///
/// # Usage
/// Primarily used internally by `TypedRecord` to store buffers without knowing
/// the concrete runtime type at compile time.
#[allow(dead_code)] // Will be used in TASK-BUF-004 (TypedRecord integration)
pub trait AnyBuffer: Send + Sync {
    /// Returns the buffer configuration
    fn config(&self) -> &BufferCfg;

    /// Validates the buffer state
    fn validate(&self) -> Result<(), &'static str> {
        self.config().validate()
    }
}

/// Blanket implementation of BufferSender for all BufferBackend types
///
/// This allows any buffer backend to be used as a trait object through
/// the `BufferSender` interface.
impl<T, B> BufferSender<T> for B
where
    T: Clone + Send + 'static,
    B: BufferBackend<T> + 'static,
{
    fn send(&self, value: T) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + '_>> {
        // Convert synchronous push to async operation
        Box::pin(async move {
            self.push(value);
            Ok(())
        })
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

    impl<T: Clone + Send + Sync + 'static> BufferBackend<T> for MockBuffer<T> {
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
}
