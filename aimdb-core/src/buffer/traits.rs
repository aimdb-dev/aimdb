//! Runtime-agnostic buffer traits
//!
//! Defines the trait interface that buffer implementations must satisfy.
//! Actual implementations are provided by adapter crates (tokio, embassy).
//!
//! # Design
//!
//! This module provides two complementary traits:
//!
//! - **`Buffer<T>`**: Static trait for concrete buffer implementations with owned readers
//! - **`DynBuffer<T>`**: Dynamic trait for trait objects (object-safe, type-erased)
//!
//! The `DynBuffer` trait is automatically implemented for all `Buffer` types via a blanket impl,
//! eliminating boilerplate in adapter crates.

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
/// This trait defines the interface for buffer implementations with owned readers.
/// Use this when you know the concrete buffer type at compile time.
///
/// # Design Philosophy
///
/// - **Runtime Agnostic**: Works with any async runtime (Tokio, Embassy, etc.)
/// - **Type Safe**: Generic over item type `T` and reader type
/// - **Owned Readers**: `subscribe()` returns owned readers that can outlive borrows
/// - **Non-Blocking**: Push operations never block
///
/// # Implementation Requirements
///
/// Implementations must:
/// 1. Support concurrent access from multiple threads (`Send + Sync`)
/// 2. Provide independent owned readers via `subscribe()`
/// 3. Handle overflow according to `BufferCfg` semantics
/// 4. Be `'static` for use in spawned tasks
///
/// # Examples
///
/// Implementing for a custom runtime:
///
/// ```rust,ignore
/// use aimdb_core::buffer::{Buffer, BufferReader, BufferCfg};
/// use aimdb_core::DbError;
///
/// struct MyBuffer<T> {
///     inner: Arc<MyRuntimeChannel<T>>,
/// }
///
/// impl<T: Clone + Send> Buffer<T> for MyBuffer<T> {
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
///         // Return owned reader
///         MyReader {
///             rx: self.inner.subscribe(),
///         }
///     }
/// }
/// ```
pub trait Buffer<T: Clone + Send>: Send + Sync + 'static {
    /// Reader type for consuming values
    ///
    /// Each call to `subscribe()` returns an independent owned reader that can
    /// consume values at its own pace.
    ///
    /// The reader is owned (not borrowed) and can outlive the reference used
    /// to create it. Implementations use Arc or static references internally
    /// to achieve this (required for Embassy's const-generic channels).
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

/// Dynamic buffer trait for trait objects (object-safe)
///
/// This trait provides a type-erased interface for buffers that can be stored
/// as trait objects (`Box<dyn DynBuffer<T>>`). It is automatically implemented
/// for all types that implement `Buffer<T>`.
///
/// # Object Safety
///
/// This trait is object-safe because:
/// - All methods take `&self` (not `Self`)
/// - No associated types or generics in method signatures
/// - Returns are concrete types (Box, &dyn Any)
///
/// # Usage
///
/// Used in `TypedRecord` and other places where buffers need to be stored
/// heterogeneously without knowing the concrete runtime type at compile time.
///
/// # Example
///
/// ```rust,ignore
/// // Store different buffer types in the same collection
/// let buffer: Box<dyn DynBuffer<SensorData>> = Box::new(tokio_buffer);
/// buffer.push(SensorData { temp: 23.5 });
/// let reader = buffer.subscribe_boxed();
/// ```
pub trait DynBuffer<T: Clone + Send>: Send + Sync {
    /// Push a value into the buffer (synchronous, non-blocking)
    ///
    /// This is the same as `Buffer::push()` but callable on trait objects.
    ///
    /// # Arguments
    /// * `value` - The value to enqueue
    fn push(&self, value: T);

    /// Create a boxed reader for consuming values
    ///
    /// Returns a type-erased reader that can be stored and used without
    /// knowing the concrete reader type. Each reader maintains its own
    /// position in the buffer.
    ///
    /// # Returns
    /// `Box<dyn BufferReader<T> + Send>` - A boxed reader instance
    ///
    /// # Example
    /// ```rust,ignore
    /// let reader = buffer.subscribe_boxed();
    /// while let Ok(value) = reader.recv().await {
    ///     process(value).await;
    /// }
    /// ```
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send>;

    /// Returns self as Any for downcasting to concrete buffer types
    ///
    /// This enables adapter-specific operations on buffers stored as
    /// trait objects.
    ///
    /// # Returns
    /// `&dyn Any` for use with `downcast_ref::<ConcreteType>()`
    ///
    /// # Example
    /// ```rust,ignore
    /// // Downcast to access runtime-specific features
    /// if let Some(tokio_buf) = buffer.as_any().downcast_ref::<TokioBuffer<T>>() {
    ///     let handle = tokio_buf.spawn_dispatcher(handler);
    /// }
    /// ```
    fn as_any(&self) -> &dyn core::any::Any;
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

/// Blanket implementation of DynBuffer for all Buffer types
///
/// This automatic implementation eliminates boilerplate in adapter crates.
/// Any type implementing `Buffer<T>` automatically gets `DynBuffer<T>`.
///
/// # Example
///
/// ```rust,ignore
/// // Adapter just implements Buffer
/// impl Buffer<SensorData> for TokioBuffer<SensorData> {
///     type Reader = TokioBufferReader<SensorData>;
///     // ... implementation
/// }
///
/// // DynBuffer is automatically available
/// let dyn_buf: Box<dyn DynBuffer<SensorData>> = Box::new(tokio_buffer);
/// ```
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
