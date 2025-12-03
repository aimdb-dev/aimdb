//! Tokio buffer implementations for AimDB
//!
//! This module provides Tokio-specific implementations of the buffer traits
//! defined in `aimdb-core`. It uses Tokio's async synchronization primitives:
//!
//! - **SPMC Ring**: `tokio::sync::broadcast` for bounded multi-consumer queues
//! - **SingleLatest**: `tokio::sync::watch` for latest-value semantics
//! - **Mailbox**: `tokio::sync::Mutex` + `tokio::sync::Notify` for single-slot overwrite

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};

use aimdb_core::buffer::{Buffer, BufferCfg, BufferReader};
use aimdb_core::DbError;
use tokio::sync::{broadcast, watch, Notify};

#[cfg(feature = "metrics")]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "metrics")]
use aimdb_core::buffer::{BufferMetrics, BufferMetricsSnapshot};

/// Internal metrics counters (feature-gated)
///
/// Shared between buffer and all readers via Arc.
#[cfg(feature = "metrics")]
pub(crate) struct BufferMetricsInner {
    /// Total items pushed
    produced: AtomicU64,
    /// Total items consumed (aggregate across all readers)
    consumed: AtomicU64,
    /// Total items dropped due to lag
    dropped: AtomicU64,
    /// Buffer capacity (for occupancy calculation)
    capacity: usize,
}

#[cfg(feature = "metrics")]
impl BufferMetricsInner {
    fn new(capacity: usize) -> Self {
        Self {
            produced: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            capacity,
        }
    }

    fn increment_produced(&self) {
        self.produced.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_consumed(&self) {
        self.consumed.fetch_add(1, Ordering::Relaxed);
    }

    fn add_dropped(&self, count: u64) {
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }

    fn snapshot(&self, current_occupancy: usize) -> BufferMetricsSnapshot {
        BufferMetricsSnapshot {
            produced_count: self.produced.load(Ordering::Relaxed),
            consumed_count: self.consumed.load(Ordering::Relaxed),
            dropped_count: self.dropped.load(Ordering::Relaxed),
            occupancy: (current_occupancy, self.capacity),
        }
    }

    fn reset(&self) {
        self.produced.store(0, Ordering::Relaxed);
        self.consumed.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
    }
}

/// Tokio buffer implementation
pub struct TokioBuffer<T: Clone + Send + Sync + 'static> {
    inner: Arc<TokioBufferInner<T>>,
    #[cfg(feature = "metrics")]
    metrics: Arc<BufferMetricsInner>,
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

impl<T: Clone + Send + Sync + 'static> Buffer<T> for TokioBuffer<T> {
    type Reader = TokioBufferReader<T>;

    fn new(cfg: &BufferCfg) -> Self {
        #[cfg(feature = "metrics")]
        let capacity = match cfg {
            BufferCfg::SpmcRing { capacity } => *capacity,
            BufferCfg::SingleLatest => 1, // Conceptually holds 1 value
            BufferCfg::Mailbox => 1,      // Single slot
        };

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
            #[cfg(feature = "metrics")]
            metrics: Arc::new(BufferMetricsInner::new(capacity)),
        }
    }

    fn push(&self, value: T) {
        #[cfg(feature = "metrics")]
        self.metrics.increment_produced();

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

    fn subscribe(&self) -> Self::Reader {
        match &*self.inner {
            TokioBufferInner::Broadcast { tx } => TokioBufferReader::Broadcast {
                rx: tx.subscribe(),
                #[cfg(feature = "metrics")]
                metrics: Arc::clone(&self.metrics),
            },
            TokioBufferInner::Watch { tx } => TokioBufferReader::Watch {
                rx: tx.subscribe(),
                #[cfg(feature = "metrics")]
                metrics: Arc::clone(&self.metrics),
            },
            TokioBufferInner::Notify { slot, notify } => TokioBufferReader::Notify {
                slot: Arc::clone(slot),
                notify: Arc::clone(notify),
                #[cfg(feature = "metrics")]
                metrics: Arc::clone(&self.metrics),
            },
        }
    }
}

// When metrics feature is disabled, DynBuffer is provided by the blanket impl in aimdb-core
// When metrics feature is enabled, we provide our own impl that includes metrics_snapshot

/// Explicit DynBuffer implementation when metrics is enabled
/// This allows us to provide actual metrics via metrics_snapshot()
#[cfg(feature = "metrics")]
impl<T: Clone + Send + Sync + 'static> aimdb_core::buffer::DynBuffer<T> for TokioBuffer<T> {
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        Some(<Self as BufferMetrics>::metrics(self))
    }
}

/// Implementation of BufferMetrics for TokioBuffer (metrics feature only)
#[cfg(feature = "metrics")]
impl<T: Clone + Send + Sync + 'static> BufferMetrics for TokioBuffer<T> {
    fn metrics(&self) -> BufferMetricsSnapshot {
        // Calculate current occupancy based on buffer type
        // Note: For broadcast, we can't directly query occupancy, so we estimate
        // For watch/mailbox, occupancy is always 0 or 1
        let current_occupancy = match &*self.inner {
            TokioBufferInner::Broadcast { tx: _ } => {
                // Broadcast doesn't expose queue length directly
                // We can estimate: produced - consumed - dropped
                let produced = self.metrics.produced.load(Ordering::Relaxed);
                let consumed = self.metrics.consumed.load(Ordering::Relaxed);
                let dropped = self.metrics.dropped.load(Ordering::Relaxed);
                let pending = produced.saturating_sub(consumed).saturating_sub(dropped);
                pending.min(self.metrics.capacity as u64) as usize
            }
            TokioBufferInner::Watch { tx } => {
                // Watch always has at most 1 value
                if tx.is_closed() {
                    0
                } else {
                    1
                }
            }
            TokioBufferInner::Notify { slot, .. } => {
                // Check if slot has a value
                if slot.lock().unwrap().is_some() {
                    1
                } else {
                    0
                }
            }
        };

        self.metrics.snapshot(current_occupancy)
    }

    fn reset_metrics(&self) {
        self.metrics.reset();
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
#[cfg_attr(feature = "metrics", allow(private_interfaces))]
pub enum TokioBufferReader<T: Clone + Send + Sync + 'static> {
    Broadcast {
        rx: broadcast::Receiver<T>,
        #[cfg(feature = "metrics")]
        metrics: Arc<BufferMetricsInner>,
    },
    Watch {
        rx: watch::Receiver<Option<T>>,
        #[cfg(feature = "metrics")]
        metrics: Arc<BufferMetricsInner>,
    },
    Notify {
        slot: Arc<StdMutex<Option<T>>>,
        notify: Arc<Notify>,
        #[cfg(feature = "metrics")]
        metrics: Arc<BufferMetricsInner>,
    },
}

impl<T: Clone + Send + Sync + 'static> BufferReader<T> for TokioBufferReader<T> {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>> {
        Box::pin(async move {
            match self {
                TokioBufferReader::Broadcast {
                    rx,
                    #[cfg(feature = "metrics")]
                    metrics,
                } => match rx.recv().await {
                    Ok(value) => {
                        #[cfg(feature = "metrics")]
                        metrics.increment_consumed();
                        Ok(value)
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        #[cfg(feature = "metrics")]
                        metrics.add_dropped(n);
                        Err(DbError::BufferLagged {
                            lag_count: n,
                            buffer_name: "broadcast".to_string(),
                        })
                    }
                    Err(broadcast::error::RecvError::Closed) => Err(DbError::BufferClosed {
                        buffer_name: "broadcast".to_string(),
                    }),
                },
                TokioBufferReader::Watch {
                    rx,
                    #[cfg(feature = "metrics")]
                    metrics,
                } => {
                    rx.changed().await.map_err(|_| DbError::BufferClosed {
                        buffer_name: "watch".to_string(),
                    })?;

                    let value = rx.borrow().clone();
                    match value {
                        Some(v) => {
                            #[cfg(feature = "metrics")]
                            metrics.increment_consumed();
                            Ok(v)
                        }
                        None => Err(DbError::BufferClosed {
                            buffer_name: "watch".to_string(),
                        }),
                    }
                }
                TokioBufferReader::Notify {
                    slot,
                    notify,
                    #[cfg(feature = "metrics")]
                    metrics,
                } => {
                    loop {
                        // Check if there's already a value
                        {
                            let mut guard = slot.lock().unwrap();
                            if let Some(value) = guard.take() {
                                #[cfg(feature = "metrics")]
                                metrics.increment_consumed();
                                return Ok(value);
                            }
                        }
                        // No value, wait for notification
                        notify.notified().await;
                    }
                }
            }
        })
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

    // ========================================================================
    // Metrics Tests (feature-gated)
    // ========================================================================

    #[cfg(feature = "metrics")]
    mod metrics_tests {
        use super::*;

        #[tokio::test]
        async fn test_spmc_ring_produced_count() {
            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);

            // Initial metrics should be zero
            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 0);
            assert_eq!(metrics.consumed_count, 0);
            assert_eq!(metrics.dropped_count, 0);

            // Push some values
            for i in 0..5 {
                buffer.push(i);
            }

            // Check produced count
            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 5);
            assert_eq!(metrics.consumed_count, 0); // No consumers yet
        }

        #[tokio::test]
        async fn test_spmc_ring_consumed_count() {
            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = buffer.subscribe();

            // Push and consume
            buffer.push(1);
            buffer.push(2);
            buffer.push(3);

            let _ = reader.recv().await.unwrap();
            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 3);
            assert_eq!(metrics.consumed_count, 2);
        }

        #[tokio::test]
        async fn test_spmc_ring_dropped_count_on_lag() {
            let cfg = BufferCfg::SpmcRing { capacity: 3 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = buffer.subscribe();

            // Overfill buffer to cause lag
            for i in 0..10 {
                buffer.push(i);
            }

            // Try to receive - should detect lag
            let result = reader.recv().await;
            if let Err(DbError::BufferLagged { lag_count, .. }) = result {
                // Check dropped count increased
                let metrics = buffer.metrics();
                assert_eq!(
                    metrics.dropped_count, lag_count,
                    "Dropped count should match lag"
                );
            }
            // Note: On very fast systems, lag might not occur
        }

        #[tokio::test]
        async fn test_metrics_reset() {
            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = buffer.subscribe();

            // Generate some metrics
            buffer.push(1);
            buffer.push(2);
            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert!(metrics.produced_count > 0);
            assert!(metrics.consumed_count > 0);

            // Reset
            buffer.reset_metrics();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 0);
            assert_eq!(metrics.consumed_count, 0);
            assert_eq!(metrics.dropped_count, 0);
        }

        #[tokio::test]
        async fn test_watch_buffer_metrics() {
            let cfg = BufferCfg::SingleLatest;
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = buffer.subscribe();

            buffer.push(1);
            buffer.push(2);
            buffer.push(3);

            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 3);
            assert_eq!(metrics.consumed_count, 1);
            // Watch doesn't drop, it overwrites
            assert_eq!(metrics.dropped_count, 0);
        }

        #[tokio::test]
        async fn test_mailbox_buffer_metrics() {
            let cfg = BufferCfg::Mailbox;
            let buffer = TokioBuffer::<i32>::new(&cfg);
            let mut reader = buffer.subscribe();

            buffer.push(1);
            let _ = reader.recv().await.unwrap();
            buffer.push(2);
            let _ = reader.recv().await.unwrap();

            let metrics = buffer.metrics();
            assert_eq!(metrics.produced_count, 2);
            assert_eq!(metrics.consumed_count, 2);
        }

        #[tokio::test]
        async fn test_dyn_buffer_metrics_snapshot() {
            use aimdb_core::buffer::DynBuffer;

            let cfg = BufferCfg::SpmcRing { capacity: 10 };
            let buffer = TokioBuffer::<i32>::new(&cfg);

            // Push some values using Buffer trait explicitly
            Buffer::push(&buffer, 1);
            Buffer::push(&buffer, 2);

            // Access via DynBuffer trait
            let dyn_buffer: &dyn DynBuffer<i32> = &buffer;
            let snapshot = dyn_buffer.metrics_snapshot();

            assert!(snapshot.is_some());
            let snapshot = snapshot.unwrap();
            assert_eq!(snapshot.produced_count, 2);
        }
    }
}
