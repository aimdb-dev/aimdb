//! MPSC outbox pattern for external system integration
//!
//! Provides type-safe message queues from multiple internal producers
//! to single external connector workers (MQTT, Kafka, etc.).
//!
//! # Key Difference from Buffers
//! - Buffers: SPMC for internal consumers  
//! - Outboxes: MPSC for external protocols
//!
//! See `examples/producer-consumer-demo` for usage.

use core::any::Any;
use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;
#[cfg(not(feature = "std"))]
use alloc::sync::Arc;

#[cfg(feature = "std")]
use std::sync::Arc;

use crate::{DbError, RuntimeAdapter};

// ============================================================================
// Runtime Outbox Support Trait (Internal - Adapter Use Only)
// ============================================================================

/// Optional trait for runtime adapters that support MPSC outbox channels
///
/// **Internal API**: Implemented by runtime adapters, not for application use.
///
/// Generic method preserves type information for channel creation while
/// returning type-erased sender/receiver for heterogeneous storage.
pub trait OutboxRuntimeSupport: RuntimeAdapter {
    /// Creates an MPSC channel for outbox use
    ///
    /// Returns (type-erased sender, type-erased receiver). The sender implements
    /// `AnySender` for registry storage; receiver is passed to `SinkWorker`.
    fn create_outbox_channel<T: Send + 'static>(
        &self,
        capacity: usize,
    ) -> (Box<dyn AnySender>, Box<dyn Any + Send>);
}

// ============================================================================
// Type-Erased Sender Trait (Internal - Adapter Use Only)
// ============================================================================

/// Type-erased sender trait for MPSC outboxes
///
/// **Internal API**: Implemented by runtime adapter sender wrappers,
/// not for application use.
///
/// Enables storing different `Sender<T>` types in heterogeneous collections.
/// Provides type erasure via `as_any()` and async sending methods.
pub trait AnySender: Send + Sync {
    /// Downcast to `&dyn Any` for type checking
    fn as_any(&self) -> &dyn Any;

    /// Send a type-erased value asynchronously
    ///
    /// Value is downcast to concrete type by implementation.
    /// Returns `Ok(())` on success or `Err(())` if channel closed.
    fn send_any(&self, value: Box<dyn Any + Send>) -> SendFuture;

    /// Try to send a type-erased value without blocking
    ///
    /// # Arguments
    ///
    /// * `value` - Type-erased value to send
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Message sent successfully
    /// * `Err(value)` - Channel full or closed, value returned
    fn try_send_any(&self, value: Box<dyn Any + Send>) -> Result<(), Box<dyn Any + Send>>;

    /// Returns the channel capacity
    ///
    /// This provides information about the maximum number of messages
    /// the channel can buffer. Useful for error reporting and metrics.
    ///
    /// # Returns
    ///
    /// The channel capacity, or 0 if unknown/unbounded
    fn capacity(&self) -> usize;

    /// Checks if the channel is closed
    ///
    /// This allows distinguishing between a full channel (temporary backpressure)
    /// and a closed channel (permanent failure) when `try_send_any` fails.
    ///
    /// # Returns
    ///
    /// `true` if the channel is closed, `false` otherwise
    fn is_closed(&self) -> bool;
}

/// Future type for async send operations
///
/// **Internal type**: Used by AnySender trait implementations. Not re-exported from crate root.
pub type SendFuture = Pin<Box<dyn Future<Output = Result<(), ()>> + Send>>;

// ============================================================================
// SinkWorker Trait (Internal - Adapter Use Only)
// ============================================================================

/// Worker that consumes messages from an MPSC outbox
///
/// **Internal API**: This trait is implemented by connector workers and
/// should not be directly referenced by most application code. It is not
/// **Internal API**: Implemented by connector workers, not typically used
/// directly by application code. See adapter examples for implementations.
///
/// Implementers spawn a task that drains the receiver and communicates
/// with an external system (MQTT broker, Kafka cluster, DDS domain, etc.).
///
/// The receiver is provided as `Box<dyn Any>` and must be downcast to the
/// concrete runtime type (tokio::mpsc::Receiver, embassy::channel::Receiver).
pub trait SinkWorker<T: Send + 'static>: Send + 'static {
    /// Spawn the worker task
    ///
    /// Should downcast receiver, establish connection, spawn drain task,
    /// and return handle for monitoring.
    fn spawn(self, rt: Arc<dyn RuntimeAdapter>, rx: Box<dyn Any + Send>) -> WorkerHandle;
}

// ============================================================================
// WorkerHandle
// ============================================================================

/// Handle for monitoring and controlling sink workers
///
/// Provides visibility into worker status and optional control operations
/// like restart or graceful shutdown.
///
/// # Example
///
/// ```rust,ignore
/// let handle = db.init_outbox::<MqttMsg, _>(config, worker)?;
///
/// // Check if worker is running
/// if !handle.is_running() {
///     eprintln!("MQTT worker has stopped!");
/// }
///
/// // Get worker ID for logging
/// println!("Worker ID: {}", handle.task_id());
/// ```
pub struct WorkerHandle {
    /// Unique identifier for this worker task
    task_id: usize,

    /// Atomic flag indicating if worker is still running
    ///
    /// Workers should set this to `false` when they exit
    /// (either normally or due to error).
    is_running: Arc<portable_atomic::AtomicBool>,

    /// Optional restart function
    ///
    /// If provided, allows programmatic restart of failed workers.
    /// This is useful for implementing auto-restart behavior.
    restart_fn: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Debug for WorkerHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("task_id", &self.task_id)
            .field("is_running", &self.is_running())
            .field(
                "restart_fn",
                &if self.restart_fn.is_some() {
                    "Some(<fn>)"
                } else {
                    "None"
                },
            )
            .finish()
    }
}

impl WorkerHandle {
    /// Creates a new worker handle
    ///
    /// # Arguments
    ///
    /// * `task_id` - Unique identifier for the worker task
    /// * `is_running` - Atomic flag for monitoring worker status
    pub fn new(task_id: usize, is_running: Arc<portable_atomic::AtomicBool>) -> Self {
        Self {
            task_id,
            is_running,
            restart_fn: None,
        }
    }

    /// Check if worker is still running
    ///
    /// # Returns
    ///
    /// `true` if the worker task is active, `false` if it has stopped
    pub fn is_running(&self) -> bool {
        self.is_running.load(portable_atomic::Ordering::Relaxed)
    }

    /// Get the worker task ID
    ///
    /// # Returns
    ///
    /// Unique identifier for this worker
    pub fn task_id(&self) -> usize {
        self.task_id
    }

    /// Set restart function for auto-restart capability
    ///
    /// # Arguments
    ///
    /// * `f` - Function to call when restart is triggered
    pub fn with_restart_fn<F>(mut self, f: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.restart_fn = Some(Box::new(f));
        self
    }

    /// Attempt to restart the worker
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Restart initiated successfully
    /// * `Err(DbError::MissingConfiguration)` - No restart function configured
    pub fn restart(&self) -> Result<(), DbError> {
        if let Some(ref restart_fn) = self.restart_fn {
            restart_fn();
            Ok(())
        } else {
            Err(DbError::MissingConfiguration {
                #[cfg(feature = "std")]
                parameter: "restart_fn".to_string(),
                #[cfg(not(feature = "std"))]
                _parameter: (),
            })
        }
    }
}

// ============================================================================
// Configuration Types
// ============================================================================

/// Configuration for an MPSC outbox
///
/// Defines capacity, overflow behavior, and worker management options.
///
/// # Example
///
/// ```rust
/// use aimdb_core::outbox::{OutboxConfig, OverflowBehavior};
///
/// let config = OutboxConfig {
///     capacity: 1024,
///     overflow: OverflowBehavior::Block,
///     auto_restart: true,
///     max_concurrent_enqueue: 0, // unlimited
/// };
/// ```
#[derive(Clone, Debug)]
pub struct OutboxConfig {
    /// Channel capacity (buffer size)
    ///
    /// Determines how many messages can be buffered before backpressure
    /// or overflow behavior is triggered.
    ///
    /// **Recommendations:**
    /// - Embedded: 128-512 (memory constrained)
    /// - Edge: 512-2048 (moderate buffering)
    /// - Cloud: 2048-8192 (high throughput)
    pub capacity: usize,

    /// Behavior when channel is full
    ///
    /// Determines what happens when attempting to enqueue to a full buffer.
    pub overflow: OverflowBehavior,

    /// Enable automatic worker restart on panic
    ///
    /// If `true`, the system will attempt to restart workers that
    /// terminate unexpectedly. Useful for production deployments.
    pub auto_restart: bool,

    /// Maximum concurrent enqueue operations (0 = unlimited)
    ///
    /// Limits the number of concurrent `enqueue()` calls to prevent
    /// resource exhaustion. Set to 0 for unlimited concurrency.
    pub max_concurrent_enqueue: usize,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            capacity: 1024,
            overflow: OverflowBehavior::Block,
            auto_restart: false,
            max_concurrent_enqueue: 0,
        }
    }
}

/// Behavior when outbox channel is full
///
/// Defines the strategy for handling buffer overflow during enqueue operations.
///
/// # Variants
///
/// * `Block` - Apply backpressure by waiting for space
/// * `Error` - Fail fast and return an error
/// * `DropOldest` - Ring buffer behavior (overwrite old messages)
/// * `DropNewest` - Reject new messages (keep existing)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowBehavior {
    /// Block until space is available (applies backpressure)
    ///
    /// The `enqueue()` call will wait asynchronously until there is
    /// space in the buffer. This applies backpressure to producers.
    ///
    /// **Use when:** Message delivery is critical and producers can tolerate delays.
    Block,

    /// Return error immediately (fail fast)
    ///
    /// The `enqueue()` call returns `Err(DbError::ChannelFull)` immediately.
    /// The caller must decide whether to retry, drop, or handle the error.
    ///
    /// **Use when:** Fast response is critical and messages can be dropped.
    Error,

    /// Drop oldest message (ring buffer behavior)
    ///
    /// Automatically removes the oldest buffered message to make space
    /// for the new one. Provides predictable latency but may lose data.
    ///
    /// **Use when:** Recent data is more valuable than historical data.
    DropOldest,

    /// Drop newest message (reject incoming)
    ///
    /// Rejects the new message, keeping existing buffered messages.
    /// Returns `Ok(())` but message is not enqueued.
    ///
    /// **Use when:** Existing buffered data has higher priority.
    DropNewest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outbox_config_default() {
        let config = OutboxConfig::default();
        assert_eq!(config.capacity, 1024);
        assert_eq!(config.overflow, OverflowBehavior::Block);
        assert!(!config.auto_restart);
        assert_eq!(config.max_concurrent_enqueue, 0);
    }

    #[test]
    fn test_overflow_behavior_variants() {
        // Ensure all variants are distinct
        assert_ne!(OverflowBehavior::Block, OverflowBehavior::Error);
        assert_ne!(OverflowBehavior::Block, OverflowBehavior::DropOldest);
        assert_ne!(OverflowBehavior::Block, OverflowBehavior::DropNewest);
    }

    #[test]
    fn test_worker_handle_creation() {
        let is_running = Arc::new(portable_atomic::AtomicBool::new(true));
        let handle = WorkerHandle::new(42, is_running);

        assert_eq!(handle.task_id(), 42);
        assert!(handle.is_running());
    }

    #[test]
    fn test_worker_handle_restart_without_fn() {
        let is_running = Arc::new(portable_atomic::AtomicBool::new(true));
        let handle = WorkerHandle::new(1, is_running);

        // Should fail when no restart function is configured
        assert!(handle.restart().is_err());
    }

    #[test]
    fn test_worker_handle_with_restart_fn() {
        let is_running = Arc::new(portable_atomic::AtomicBool::new(true));
        let restart_called = Arc::new(portable_atomic::AtomicBool::new(false));
        let restart_called_clone = restart_called.clone();

        let handle = WorkerHandle::new(1, is_running).with_restart_fn(move || {
            restart_called_clone.store(true, portable_atomic::Ordering::Relaxed);
        });

        // Should succeed and call the function
        assert!(handle.restart().is_ok());
        assert!(restart_called.load(portable_atomic::Ordering::Relaxed));
    }

    #[test]
    fn test_worker_handle_status_tracking() {
        let is_running = Arc::new(portable_atomic::AtomicBool::new(true));
        let handle = WorkerHandle::new(100, is_running.clone());

        // Initially running
        assert!(handle.is_running());

        // Simulate worker stopping
        is_running.store(false, portable_atomic::Ordering::Relaxed);
        assert!(!handle.is_running());

        // Simulate worker restarting
        is_running.store(true, portable_atomic::Ordering::Relaxed);
        assert!(handle.is_running());
    }

    #[test]
    fn test_outbox_config_validation() {
        // Valid config
        let config = OutboxConfig {
            capacity: 100,
            overflow: OverflowBehavior::Block,
            auto_restart: true,
            max_concurrent_enqueue: 10,
        };
        assert_eq!(config.capacity, 100);
        assert!(config.auto_restart);

        // Zero capacity (edge case - should be handled by implementation)
        let zero_config = OutboxConfig {
            capacity: 0,
            overflow: OverflowBehavior::DropOldest,
            auto_restart: false,
            max_concurrent_enqueue: 0,
        };
        assert_eq!(zero_config.capacity, 0);
        assert_eq!(zero_config.max_concurrent_enqueue, 0); // Unlimited
    }

    #[test]
    fn test_overflow_behavior_semantics() {
        // Test that variants exist and can be created
        let _block = OverflowBehavior::Block;
        let _drop_oldest = OverflowBehavior::DropOldest;
        let _drop_newest = OverflowBehavior::DropNewest;
        let _error = OverflowBehavior::Error;

        // Test pattern matching
        match OverflowBehavior::Block {
            OverflowBehavior::Block => {
                // Correct
            }
            OverflowBehavior::DropOldest
            | OverflowBehavior::DropNewest
            | OverflowBehavior::Error => {
                panic!("Wrong variant");
            }
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_worker_handle_debug_format() {
        let is_running = Arc::new(portable_atomic::AtomicBool::new(true));
        let handle = WorkerHandle::new(42, is_running);

        let debug_str = format!("{:?}", handle);
        assert!(debug_str.contains("WorkerHandle"));
        assert!(debug_str.contains("task_id"));
        assert!(debug_str.contains("42"));
    }

    #[test]
    fn test_worker_handle_with_restart_multiple_calls() {
        let is_running = Arc::new(portable_atomic::AtomicBool::new(true));
        let call_count = Arc::new(portable_atomic::AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let handle = WorkerHandle::new(1, is_running).with_restart_fn(move || {
            call_count_clone.fetch_add(1, portable_atomic::Ordering::Relaxed);
        });

        // Call restart multiple times
        for _ in 0..5 {
            assert!(handle.restart().is_ok());
        }

        assert_eq!(call_count.load(portable_atomic::Ordering::Relaxed), 5);
    }

    // Mock AnySender for testing type erasure
    #[cfg(feature = "std")]
    mod any_sender_tests {
        use super::*;

        struct MockAnySender<T> {
            values: std::sync::Arc<std::sync::Mutex<Vec<T>>>,
        }

        impl<T: 'static + Send> MockAnySender<T> {
            fn new() -> Self {
                Self {
                    values: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                }
            }

            fn get_values(&self) -> Vec<T>
            where
                T: Clone,
            {
                self.values.lock().unwrap().clone()
            }
        }

        impl<T: Clone + Send + 'static> AnySender for MockAnySender<T> {
            fn as_any(&self) -> &dyn core::any::Any {
                self
            }

            fn send_any(&self, value: Box<dyn core::any::Any + Send>) -> SendFuture {
                let value = *value.downcast::<T>().expect("Type mismatch");
                let values = self.values.clone();

                Box::pin(async move {
                    values.lock().unwrap().push(value);
                    Ok(())
                })
            }

            fn try_send_any(
                &self,
                value: Box<dyn core::any::Any + Send>,
            ) -> Result<(), Box<dyn core::any::Any + Send>> {
                let value = *value.downcast::<T>().expect("Type mismatch");
                self.values.lock().unwrap().push(value);
                Ok(())
            }

            fn capacity(&self) -> usize {
                // Mock capacity for testing
                1000
            }

            fn is_closed(&self) -> bool {
                // Mock: never closed
                false
            }
        }

        #[derive(Debug, Clone, PartialEq)]
        struct TestPayload {
            id: u32,
            data: String,
        }

        #[test]
        fn test_any_sender_as_any_downcast() {
            let sender = MockAnySender::<TestPayload>::new();
            let sender_ref: &dyn AnySender = &sender;

            // Test downcast via as_any
            let downcasted = sender_ref
                .as_any()
                .downcast_ref::<MockAnySender<TestPayload>>();
            assert!(downcasted.is_some());
        }

        #[tokio::test]
        async fn test_any_sender_send_any() {
            let sender = MockAnySender::<TestPayload>::new();
            let sender_ref: &dyn AnySender = &sender;

            let payload = TestPayload {
                id: 42,
                data: "test".to_string(),
            };

            let boxed = Box::new(payload.clone()) as Box<dyn core::any::Any + Send>;
            let result = sender_ref.send_any(boxed).await;

            assert!(result.is_ok());
            let values = sender.get_values();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0], payload);
        }

        #[test]
        fn test_any_sender_try_send_any() {
            let sender = MockAnySender::<TestPayload>::new();
            let sender_ref: &dyn AnySender = &sender;

            let payload = TestPayload {
                id: 99,
                data: "sync".to_string(),
            };

            let boxed = Box::new(payload.clone()) as Box<dyn core::any::Any + Send>;
            let result = sender_ref.try_send_any(boxed);

            assert!(result.is_ok());
            let values = sender.get_values();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0], payload);
        }

        #[test]
        fn test_type_erased_storage() {
            // Simulate storing multiple senders in a type-erased collection
            let sender1 = MockAnySender::<i32>::new();
            let sender2 = MockAnySender::<String>::new();

            let storage: Vec<Box<dyn AnySender>> = vec![Box::new(sender1), Box::new(sender2)];

            assert_eq!(storage.len(), 2);

            // Each sender can still be used via the trait
            let boxed_int = Box::new(42i32) as Box<dyn core::any::Any + Send>;
            let result = storage[0].try_send_any(boxed_int);
            assert!(result.is_ok());

            let boxed_string = Box::new("hello".to_string()) as Box<dyn core::any::Any + Send>;
            let result = storage[1].try_send_any(boxed_string);
            assert!(result.is_ok());
        }
    }

    // Note: Additional integration tests for AnySender with real runtime adapters
    // are in the tokio-adapter and embassy-adapter test suites
}
