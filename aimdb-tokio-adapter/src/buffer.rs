//! Tokio buffer implementations for AimDB
//!
//! This module provides Tokio-specific implementations of the buffer traits
//! defined in `aimdb-core`. It uses Tokio's async synchronization primitives:
//!
//! - **SPMC Ring**: `tokio::sync::broadcast` for bounded multi-consumer queues
//! - **SingleLatest**: `tokio::sync::watch` for latest-value semantics
//! - **Mailbox**: `tokio::sync::Mutex` + `tokio::sync::Notify` for single-slot overwrite

use std::sync::{Arc, Mutex as StdMutex};

use aimdb_core::buffer::{BufferBackend, BufferCfg, BufferReader};
use aimdb_core::DbError;
use tokio::sync::{broadcast, watch, Notify};

/// Tokio buffer implementation
pub struct TokioBuffer<T: Clone + Send + Sync + 'static> {
    inner: Arc<TokioBufferInner<T>>,
}

/// Internal buffer variants using Tokio primitives
enum TokioBufferInner<T: Clone + Send + Sync + 'static> {
    Broadcast {
        tx: broadcast::Sender<T>,
    },
    Watch {
        tx: watch::Sender<Option<T>>,
    },
    Notify {
        slot: Arc<StdMutex<Option<T>>>,
        notify: Arc<Notify>,
    },
}

impl<T: Clone + Send + Sync + 'static> BufferBackend<T> for TokioBuffer<T> {
    type Reader<'a>
        = TokioBufferReader<T>
    where
        Self: 'a;

    fn new(cfg: &BufferCfg) -> Self {
        let inner = match &cfg {
            BufferCfg::SpmcRing { capacity } => {
                let (tx, _) = broadcast::channel(*capacity);
                TokioBufferInner::Broadcast { tx }
            }
            BufferCfg::SingleLatest => {
                let (tx, _rx) = watch::channel(None);
                TokioBufferInner::Watch { tx }
            }
            BufferCfg::Mailbox => TokioBufferInner::Notify {
                slot: Arc::new(StdMutex::new(None)),
                notify: Arc::new(Notify::new()),
            },
        };

        Self {
            inner: Arc::new(inner),
        }
    }

    fn push(&self, value: T) {
        match &*self.inner {
            TokioBufferInner::Broadcast { tx } => {
                let _ = tx.send(value);
            }
            TokioBufferInner::Watch { tx } => {
                let _ = tx.send(Some(value));
            }
            TokioBufferInner::Notify { slot, notify } => {
                *slot.lock().unwrap() = Some(value);
                notify.notify_waiters();
            }
        }
    }

    fn subscribe(&self) -> Self::Reader<'_> {
        match &*self.inner {
            TokioBufferInner::Broadcast { tx } => {
                TokioBufferReader::Broadcast { rx: tx.subscribe() }
            }
            TokioBufferInner::Watch { tx } => TokioBufferReader::Watch { rx: tx.subscribe() },
            TokioBufferInner::Notify { slot, notify } => TokioBufferReader::Notify {
                slot: Arc::clone(slot),
                notify: Arc::clone(notify),
            },
        }
    }
}

impl<T: Clone + Send + Sync + 'static> TokioBuffer<T> {
    /// Spawns a dispatcher task that drains the buffer and calls a handler function
    ///
    /// This method creates a background task that:
    /// 1. Subscribes to the buffer
    /// 2. Continuously receives values
    /// 3. Calls the provided async handler for each value
    /// 4. Handles lag and closed buffer errors
    ///
    /// # Arguments
    /// * `handler` - Async function called for each buffered value
    ///
    /// # Returns
    /// A `tokio::task::JoinHandle` that can be used to await task completion
    ///
    /// # Example
    /// ```rust,ignore
    /// let handle = buffer.spawn_dispatcher(|value| async move {
    ///     println!("Processing: {:?}", value);
    ///     // Call producer and consumers here
    /// });
    /// ```
    pub fn spawn_dispatcher<F, Fut>(&self, handler: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut reader = self.subscribe();

        tokio::spawn(async move {
            loop {
                match reader.recv().await {
                    Ok(value) => {
                        handler(value).await;
                    }
                    Err(DbError::BufferLagged { lag_count, .. }) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Buffer dispatcher lagged by {} messages", lag_count);

                        #[cfg(not(feature = "tracing"))]
                        eprintln!("Buffer dispatcher lagged by {} messages", lag_count);

                        // Continue processing after lag
                        continue;
                    }
                    Err(DbError::BufferClosed { .. }) => {
                        #[cfg(feature = "tracing")]
                        tracing::info!("Buffer closed, dispatcher exiting");

                        #[cfg(not(feature = "tracing"))]
                        eprintln!("Buffer closed, dispatcher exiting");

                        // Buffer closed, exit gracefully
                        break;
                    }
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!("Buffer dispatcher error: {:?}", e);

                        #[cfg(not(feature = "tracing"))]
                        eprintln!("Buffer dispatcher error: {:?}", e);

                        // Unexpected error, exit
                        break;
                    }
                }
            }
        })
    }
}

/// Tokio-based buffer reader
pub enum TokioBufferReader<T: Clone + Send + Sync + 'static> {
    Broadcast {
        rx: broadcast::Receiver<T>,
    },
    Watch {
        rx: watch::Receiver<Option<T>>,
    },
    Notify {
        slot: Arc<StdMutex<Option<T>>>,
        notify: Arc<Notify>,
    },
}

impl<T: Clone + Send + Sync + 'static> BufferReader<T> for TokioBufferReader<T> {
    async fn recv(&mut self) -> Result<T, DbError> {
        match self {
            TokioBufferReader::Broadcast { rx } => match rx.recv().await {
                Ok(value) => Ok(value),
                Err(broadcast::error::RecvError::Lagged(n)) => Err(DbError::BufferLagged {
                    lag_count: n,
                    buffer_name: "broadcast".to_string(),
                }),
                Err(broadcast::error::RecvError::Closed) => Err(DbError::BufferClosed {
                    buffer_name: "broadcast".to_string(),
                }),
            },
            TokioBufferReader::Watch { rx } => {
                rx.changed().await.map_err(|_| DbError::BufferClosed {
                    buffer_name: "watch".to_string(),
                })?;

                let value = rx.borrow().clone();
                match value {
                    Some(v) => Ok(v),
                    None => Err(DbError::BufferClosed {
                        buffer_name: "watch".to_string(),
                    }),
                }
            }
            TokioBufferReader::Notify { slot, notify } => {
                loop {
                    // Check if there's already a value
                    {
                        let mut guard = slot.lock().unwrap();
                        if let Some(value) = guard.take() {
                            return Ok(value);
                        }
                    }
                    // No value, wait for notification
                    notify.notified().await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spmc_ring_basic() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_spmc_ring_multiple_consumers() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader1 = buffer.subscribe();
        let mut reader2 = buffer.subscribe();
        buffer.push(1);
        buffer.push(2);
        assert_eq!(reader1.recv().await.unwrap(), 1);
        assert_eq!(reader2.recv().await.unwrap(), 1);
        assert_eq!(reader1.recv().await.unwrap(), 2);
        assert_eq!(reader2.recv().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_single_latest_basic() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_single_latest_skip_intermediate() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);
        assert_eq!(reader.recv().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_mailbox_basic() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_mailbox_overwrite() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(1);
        buffer.push(2);
        assert_eq!(reader.recv().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_dispatcher_spawning() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);

        // Counter to track how many values were processed
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        // Spawn dispatcher
        let _handle = buffer.spawn_dispatcher(move |value| {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(value as u32, Ordering::SeqCst);
            }
        });

        // Send some values
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);

        // Give dispatcher time to process
        sleep(Duration::from_millis(100)).await;

        // Verify all values were processed
        assert_eq!(counter.load(Ordering::SeqCst), 6); // 1 + 2 + 3
    }

    #[tokio::test]
    async fn test_dispatcher_lag_handling() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::time::{sleep, Duration};

        // Small buffer to force lagging
        let cfg = BufferCfg::SpmcRing { capacity: 2 };
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        // Slow dispatcher that will lag
        let _handle = buffer.spawn_dispatcher(move |value| {
            let counter = Arc::clone(&counter_clone);
            async move {
                sleep(Duration::from_millis(50)).await; // Slow processing
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = value; // Use value to avoid warning
            }
        });

        // Send many values quickly to cause lag
        for i in 0..10 {
            buffer.push(i);
        }

        // Wait for processing
        sleep(Duration::from_millis(600)).await;

        // Should have processed some values (exact count depends on timing)
        let count = counter.load(Ordering::SeqCst);
        assert!(
            count > 0,
            "Dispatcher should process at least some values despite lag"
        );
    }

    // ========================================================================
    // Integration Tests - Buffer Semantics
    // ========================================================================

    #[tokio::test]
    async fn test_spmc_ring_overflow_behavior() {
        // Small buffer to test overflow - need to send more than capacity
        // for a slow reader to lag
        let cfg = BufferCfg::SpmcRing { capacity: 3 };
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = buffer.subscribe();

        // Send more messages than capacity without reading
        // This will cause the slow reader to lag
        for i in 0..10 {
            buffer.push(i);
        }

        // First recv should detect lag
        let result = reader.recv().await;
        match result {
            Err(DbError::BufferLagged { lag_count, .. }) => {
                assert!(lag_count > 0, "Should detect lag: lagged by {}", lag_count);
            }
            Ok(val) => {
                // On fast systems, might get first value before lag
                // Try again - subsequent values should show lag or higher values
                assert!(val >= 0, "Should get a valid value");
            }
            _ => panic!("Unexpected error: {:?}", result),
        }

        // Should be able to continue reading newer values
        // (exact values depend on timing, but should be higher numbers)
        for _ in 0..3 {
            match reader.recv().await {
                Ok(val) => {
                    assert!((0..10).contains(&val), "Should read values in range");
                }
                Err(DbError::BufferLagged { .. }) => {
                    // Also acceptable - means we're catching up
                }
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
        }
    }

    #[tokio::test]
    async fn test_spmc_ring_multiple_independent_consumers() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);

        // Create three independent readers
        let mut reader1 = buffer.subscribe();
        let mut reader2 = buffer.subscribe();
        let mut reader3 = buffer.subscribe();

        // Send values
        for i in 0..5 {
            buffer.push(i);
        }

        // Each reader should receive all values independently
        for expected in 0..5 {
            assert_eq!(reader1.recv().await.unwrap(), expected);
            assert_eq!(reader2.recv().await.unwrap(), expected);
            assert_eq!(reader3.recv().await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn test_single_latest_skips_intermediate_values() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = buffer.subscribe();

        // Send multiple values rapidly
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);
        buffer.push(4);
        buffer.push(5);

        // Small delay to ensure values are written
        sleep(Duration::from_millis(10)).await;

        // Should only get the latest value
        let value = reader.recv().await.unwrap();
        assert_eq!(value, 5, "Should skip intermediate values and get latest");

        // Wait with no new values - should block/wait
        // (We'll test this with a timeout)
    }

    #[tokio::test]
    async fn test_single_latest_multiple_consumers_all_get_latest() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        // Create readers BEFORE sending values
        let mut reader1 = buffer.subscribe();
        let mut reader2 = buffer.subscribe();

        // Send values
        buffer.push(10);
        buffer.push(20);
        buffer.push(30);

        sleep(Duration::from_millis(10)).await;

        // Both readers should get the latest value
        assert_eq!(reader1.recv().await.unwrap(), 30);
        assert_eq!(reader2.recv().await.unwrap(), 30);
    }

    #[tokio::test]
    async fn test_mailbox_overwrite_semantics() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = buffer.subscribe();

        // Send first value
        buffer.push(1);

        // Send second value before reading - should overwrite
        buffer.push(2);

        // Send third value - should overwrite again
        buffer.push(3);

        sleep(Duration::from_millis(10)).await;

        // Should only get the last value
        let value = reader.recv().await.unwrap();
        assert_eq!(
            value, 3,
            "Mailbox should overwrite, only latest value available"
        );
    }

    #[tokio::test]
    async fn test_mailbox_single_slot_behavior() {
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let mut reader = buffer.subscribe();

        // Send and immediately read
        buffer.push(10);
        sleep(Duration::from_millis(10)).await;
        assert_eq!(reader.recv().await.unwrap(), 10);

        // Send another value
        buffer.push(20);
        sleep(Duration::from_millis(10)).await;
        assert_eq!(reader.recv().await.unwrap(), 20);

        // Verify it's truly single-slot: send multiple, only last survives
        buffer.push(30);
        buffer.push(40);
        buffer.push(50);
        sleep(Duration::from_millis(10)).await;

        let value = reader.recv().await.unwrap();
        assert_eq!(value, 50, "Only the last value should be in the slot");
    }

    #[tokio::test]
    async fn test_dispatcher_with_spmc_ring_semantics() {
        use std::sync::Mutex;
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::SpmcRing { capacity: 100 };
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        // Spawn dispatcher
        let _handle = buffer.spawn_dispatcher(move |value| {
            let received = Arc::clone(&received_clone);
            async move {
                received.lock().unwrap().push(value);
            }
        });

        // Send sequential values
        for i in 0..10 {
            buffer.push(i);
        }

        // Wait for processing
        sleep(Duration::from_millis(200)).await;

        // Verify all values received in order
        let values = received.lock().unwrap().clone();
        assert_eq!(values.len(), 10, "Should receive all values");
        for (idx, val) in values.iter().enumerate() {
            assert_eq!(*val, idx as i32, "Values should be in order");
        }
    }

    #[tokio::test]
    async fn test_dispatcher_with_single_latest_semantics() {
        use std::sync::Mutex;
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        // Spawn dispatcher
        let _handle = buffer.spawn_dispatcher(move |value| {
            let received = Arc::clone(&received_clone);
            async move {
                sleep(Duration::from_millis(20)).await; // Slow processing
                received.lock().unwrap().push(value);
            }
        });

        // Send many values rapidly
        for i in 0..20 {
            buffer.push(i);
            sleep(Duration::from_millis(5)).await;
        }

        // Wait for processing
        sleep(Duration::from_millis(500)).await;

        // Should have received fewer values than sent (intermediate skipped)
        let values = received.lock().unwrap().clone();
        assert!(
            values.len() < 20,
            "Should skip intermediate values, got {} values",
            values.len()
        );
        assert!(!values.is_empty(), "Should receive at least some values");

        // Last value should be the highest
        let last = values.last().unwrap();
        assert_eq!(*last, 19, "Last value should be the final sent value");
    }
    #[tokio::test]
    async fn test_dispatcher_with_mailbox_semantics() {
        use std::sync::Mutex;
        use tokio::time::{sleep, Duration};

        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        // Spawn dispatcher with slow processing
        let _handle = buffer.spawn_dispatcher(move |value| {
            let received = Arc::clone(&received_clone);
            async move {
                sleep(Duration::from_millis(50)).await; // Slow processing
                received.lock().unwrap().push(value);
            }
        });

        // Send values with short delays
        for i in 0..10 {
            buffer.push(i);
            sleep(Duration::from_millis(10)).await;
        }

        // Wait for all processing
        sleep(Duration::from_millis(600)).await;

        // Mailbox should have overwritten values
        let values = received.lock().unwrap().clone();
        assert!(
            values.len() < 10,
            "Mailbox should overwrite, not queue all values"
        );
        assert!(!values.is_empty(), "Should receive some values");
    }
}
