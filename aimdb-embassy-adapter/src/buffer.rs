//! Embassy buffer implementations for AimDB
//!
//! This module provides Embassy-specific implementations of the buffer traits
//! defined in `aimdb-core`. It uses Embassy's no_std async synchronization primitives:
//!
//! - **SPMC Ring**: `embassy_sync::pubsub::PubSubChannel` for bounded multi-consumer queues
//! - **SingleLatest**: `embassy_sync::watch::Watch` for latest-value semantics
//! - **Mailbox**: `embassy_sync::channel::Channel` (capacity=1) for single-slot mailbox
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────┐
//! │ EmbassyBuffer<T>    │  ←  BufferBackend trait
//! └──────────┬──────────┘
//!            │
//!     ┌──────┴──────────────────────┐
//!     │ EmbassyBufferInner<T>       │
//!     ├─────────────────────────────┤
//!     │ • PubSub     (SPMC Ring)    │
//!     │ • Watch      (SingleLatest) │
//!     │ • Channel    (Mailbox)      │
//!     └─────────────────────────────┘
//! ```
//!
//! # Limitations
//!
//! Embassy's sync primitives require compile-time constants for sizing:
//! - `PubSubChannel<M, T, CAP, SUBS, PUBS>` - fixed capacity and subscriber/publisher counts
//! - `Watch<M, T, N>` - fixed number of receivers
//!
//! The buffer configuration's runtime capacity parameter must match the const generic `CAP`.
//! A panic will occur if there's a mismatch. This is a design constraint of Embassy's no_std
//! implementation.
//!
//! # Arc-Based Readers
//!
//! To enable universal subscription support (BufferSubscribable trait), readers hold
//! Arc references to the buffer internals. This requires the `alloc` feature (enabled by default).

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use aimdb_core::buffer::{Buffer, BufferCfg, BufferReader};
use aimdb_core::DbError;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::pubsub::{PubSubChannel, WaitResult};
use embassy_sync::watch::Watch;

#[cfg(feature = "metrics")]
use aimdb_core::buffer::{BufferCounters, BufferMetrics, BufferMetricsSnapshot};

/// Embassy buffer implementation that wraps the appropriate Embassy primitive
/// based on the buffer configuration.
///
/// Uses Arc internally to enable owned readers that can outlive borrows,
/// making it compatible with the BufferSubscribable trait.
///
/// # Type Parameters
/// - `T`: The value type (must be `Clone` for Embassy primitives)
/// - `CAP`: Capacity for SPMC ring buffer (const generic)
/// - `SUBS`: Maximum subscribers for SPMC ring (const generic)
/// - `PUBS`: Maximum publishers for SPMC ring (const generic)
/// - `WATCH_N`: Maximum receivers for Watch channel (const generic)
///
/// # Example
/// ```no_run
/// use aimdb_embassy_adapter::EmbassyBuffer;
/// use aimdb_core::buffer::{Buffer, BufferReader};
///
/// // Create an SPMC ring buffer with capacity 32, 4 subscribers, 2 publishers
/// type MyBuffer = EmbassyBuffer<u32, 32, 4, 2, 4>;
///
/// # async fn example() {
/// let buffer: MyBuffer = MyBuffer::new_spmc();
/// let mut reader = buffer.subscribe();
/// buffer.push(42);
/// let value = reader.recv().await.unwrap();
/// # }
/// ```
pub struct EmbassyBuffer<
    T: Clone,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
    const WATCH_N: usize,
> {
    inner: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
    #[cfg(feature = "metrics")]
    metrics: Arc<BufferCounters>,
}

/// Inner buffer variants using Embassy primitives
enum EmbassyBufferInner<
    T: Clone,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
    const WATCH_N: usize,
> {
    /// SPMC Ring buffer using PubSubChannel
    /// Supports multiple publishers and subscribers with bounded capacity
    SpmcRing(PubSubChannel<CriticalSectionRawMutex, T, CAP, SUBS, PUBS>),

    /// SingleLatest buffer using Watch
    /// Latest value semantics with multiple receivers
    Watch(Watch<CriticalSectionRawMutex, T, WATCH_N>),

    /// Mailbox buffer using Channel with capacity 1
    /// Single-slot overwrite behavior (try_send drops oldest)
    Mailbox(Channel<CriticalSectionRawMutex, T, 1>),
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    /// Create a new SPMC ring buffer
    pub fn new_spmc() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::SpmcRing(PubSubChannel::new())),
            #[cfg(feature = "metrics")]
            metrics: Arc::new(BufferCounters::new(CAP)),
        }
    }

    /// Create a new SingleLatest buffer
    pub fn new_watch() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::Watch(Watch::new())),
            #[cfg(feature = "metrics")]
            metrics: Arc::new(BufferCounters::new(1)),
        }
    }

    /// Create a new Mailbox buffer
    pub fn new_mailbox() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::Mailbox(Channel::new())),
            #[cfg(feature = "metrics")]
            metrics: Arc::new(BufferCounters::new(1)),
        }
    }
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > Buffer<T> for EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    type Reader = EmbassyBufferReader<T, CAP, SUBS, PUBS, WATCH_N>;

    fn new(cfg: &BufferCfg) -> Self {
        match cfg {
            BufferCfg::SpmcRing { capacity } => {
                // Note: Embassy requires compile-time capacity, so we use CAP const generic
                // Validate that the runtime capacity matches the const generic
                if *capacity != CAP {
                    panic!(
                        "BufferCfg::SpmcRing capacity ({}) does not match const generic CAP ({})",
                        capacity, CAP
                    );
                }
                Self::new_spmc()
            }
            BufferCfg::SingleLatest => Self::new_watch(),
            BufferCfg::Mailbox => Self::new_mailbox(),
        }
    }

    fn push(&self, value: T) {
        #[cfg(feature = "metrics")]
        self.metrics.increment_produced();

        match &*self.inner {
            EmbassyBufferInner::SpmcRing(channel) => {
                // Use immediate publish to avoid blocking
                // This matches Tokio's broadcast behavior where send() doesn't block
                let publisher = channel.immediate_publisher();
                publisher.publish_immediate(value);
            }
            EmbassyBufferInner::Watch(watch) => {
                watch.sender().send(value);
            }
            EmbassyBufferInner::Mailbox(channel) => {
                // Try to send, if full, clear and try again (overwrite semantic)
                let sender = channel.sender();
                if sender.try_send(value.clone()).is_err() {
                    // Channel is full (has 1 old message), try to receive it to make space
                    let _ = channel.try_receive();
                    // Now try to send again
                    let _ = sender.try_send(value);
                }
            }
        }
    }

    fn subscribe(&self) -> Self::Reader {
        // Clone the Arc for the reader
        EmbassyBufferReader {
            buffer: Arc::clone(&self.inner),
            watch_receiver: None, // Will be initialized on first recv() for Watch buffers
            spmc_subscriber: None, // Will be initialized on first recv() for SpmcRing buffers
            #[cfg(feature = "metrics")]
            metrics: Arc::clone(&self.metrics),
        }
    }
}

/// Explicit DynBuffer implementation for EmbassyBuffer
///
/// Required since there is no blanket impl - each adapter provides its own.
impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > aimdb_core::buffer::DynBuffer<T> for EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }

    fn subscribe_boxed(&self) -> alloc::boxed::Box<dyn aimdb_core::buffer::BufferReader<T> + Send> {
        alloc::boxed::Box::new(self.subscribe())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn peek(&self) -> Option<T> {
        match &*self.inner {
            // Watch stores the latest value natively; try_get() clones it
            // without consuming a receiver slot or advancing any cursor.
            EmbassyBufferInner::Watch(watch) => watch.try_get(),
            // Channel<_, T, 1>::try_peek() clones the pending slot without
            // removing it (the slot is drained once a consumer receives).
            // Mirrors the Tokio Mailbox arm.
            EmbassyBufferInner::Mailbox(channel) => channel.try_peek().ok(),
            // PubSub has no canonical latest — see design 031 §SPMC Ring.
            EmbassyBufferInner::SpmcRing(_) => None,
        }
    }

    #[cfg(feature = "metrics")]
    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        Some(<Self as BufferMetrics>::metrics(self))
    }

    #[cfg(feature = "metrics")]
    fn reset_metrics(&self) {
        <Self as BufferMetrics>::reset_metrics(self);
    }
}

/// Implementation of BufferMetrics for EmbassyBuffer (metrics feature only).
///
/// Counter state is shared with readers via `Arc<BufferCounters>`. Occupancy
/// readings:
/// - `SpmcRing`: `PubSubChannel::len()` (current queued messages) over `CAP`.
/// - `Watch`: `(1, 1)` once a value has been published (tracked via the
///   `produced` counter — embassy's `Watch` doesn't expose state), else `(0, 1)`.
/// - `Mailbox`: `(1, 1)` if the slot is occupied, else `(0, 1)`.
#[cfg(feature = "metrics")]
impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > BufferMetrics for EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    fn metrics(&self) -> BufferMetricsSnapshot {
        let snap_partial = self.metrics.snapshot((0, self.metrics.capacity()));
        let current_occupancy = match &*self.inner {
            EmbassyBufferInner::SpmcRing(channel) => channel.len(),
            EmbassyBufferInner::Watch(_) => {
                if snap_partial.produced_count > 0 {
                    1
                } else {
                    0
                }
            }
            EmbassyBufferInner::Mailbox(channel) => {
                if channel.is_full() {
                    1
                } else {
                    0
                }
            }
        };
        self.metrics
            .snapshot((current_occupancy, self.metrics.capacity()))
    }

    fn reset_metrics(&self) {
        self.metrics.reset();
    }
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    /// Creates a dispatcher task closure for use with Embassy executors
    ///
    /// This method returns an async closure that can be spawned as an Embassy task.
    /// Unlike Tokio's `spawn_dispatcher` which immediately spawns the task, this
    /// method returns the task for you to spawn with your Embassy executor.
    ///
    /// # Arguments
    /// * `handler` - Async function called for each buffered value
    ///
    /// # Returns
    /// An async closure that can be passed to `embassy_executor::Spawner::spawn()`
    ///
    /// # Example
    /// ```rust,ignore
    /// // In your Embassy application:
    /// #[embassy_executor::task]
    /// async fn buffer_dispatcher(buffer: &'static EmbassyBuffer<i32, 32, 4, 1, 1>) {
    ///     let task = buffer.dispatcher_task(|value| async move {
    ///         // Process value
    ///         defmt::info!("Received: {}", value);
    ///     });
    ///     task.await;
    /// }
    /// ```
    pub async fn dispatcher_task<F, Fut>(&'static self, handler: F)
    where
        F: Fn(T) -> Fut + Send + Sync,
        Fut: core::future::Future<Output = ()> + Send,
    {
        let mut reader = self.subscribe();

        loop {
            match reader.recv().await {
                Ok(value) => {
                    handler(value).await;
                }
                Err(DbError::BufferLagged { .. }) => {
                    // Continue processing after lag
                    continue;
                }
                Err(DbError::BufferClosed { .. }) => {
                    // Buffer closed, exit gracefully
                    break;
                }
                Err(_) => {
                    // Unexpected error, exit
                    break;
                }
            }
        }
    }
}

/// Reader for Embassy buffers
///
/// Holds persistent subscription state for each buffer type.
/// For Watch buffers, stores a persistent Receiver to track which value has been seen.
/// For SpmcRing buffers, stores a persistent Subscriber for cursor continuity.
pub struct EmbassyBufferReader<
    T: Clone + Send + 'static,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
    const WATCH_N: usize,
> {
    buffer: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
    /// Persistent Watch receiver. The 'static lifetime is safe because the Arc keeps the Watch alive.
    watch_receiver:
        Option<embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, T, WATCH_N>>,
    /// Persistent SpmcRing subscriber (same pattern as watch_receiver).
    /// The 'static lifetime is safe because the Arc keeps the PubSubChannel alive.
    spmc_subscriber: Option<
        embassy_sync::pubsub::Subscriber<'static, CriticalSectionRawMutex, T, CAP, SUBS, PUBS>,
    >,
    /// Shared counter state (cloned from the parent buffer at subscribe time).
    #[cfg(feature = "metrics")]
    metrics: Arc<BufferCounters>,
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > BufferReader<T> for EmbassyBufferReader<T, CAP, SUBS, PUBS, WATCH_N>
{
    fn recv(
        &mut self,
    ) -> core::pin::Pin<Box<dyn core::future::Future<Output = Result<T, DbError>> + Send + '_>>
    {
        Box::pin(async move {
            match &*self.buffer {
                EmbassyBufferInner::SpmcRing(channel) => {
                    // Lazily create persistent subscriber (same pattern as watch_receiver)
                    if self.spmc_subscriber.is_none() {
                        // SAFETY: The Arc in self.buffer keeps the PubSubChannel alive for this reader's lifetime.
                        // We extend the lifetime to 'static to store the subscriber, which is safe because
                        // the subscriber is dropped with the reader.
                        let channel_static: &'static embassy_sync::pubsub::PubSubChannel<
                            CriticalSectionRawMutex,
                            T,
                            CAP,
                            SUBS,
                            PUBS,
                        > = unsafe { &*(channel as *const _) };
                        self.spmc_subscriber = Some(
                            channel_static.subscriber().map_err(|_| {
                                defmt::error!(
                                    "AimDB: SpmcRing subscriber slot exhausted (max SUBS={}). \
                                     Increase the CONSUMERS const generic on buffer_sized<CAP, CONSUMERS>. \
                                     Count one slot per .tap(), .link_to() connector, and each transform_join input.",
                                    SUBS
                                );
                                DbError::BufferClosed {
                                    buffer_name: String::from("embassy spmc ring"),
                                }
                            })?,
                        );
                    }
                    match self.spmc_subscriber.as_mut().unwrap().next_message().await {
                        WaitResult::Message(value) => {
                            #[cfg(feature = "metrics")]
                            self.metrics.increment_consumed();
                            Ok(value)
                        }
                        WaitResult::Lagged(n) => {
                            #[cfg(feature = "metrics")]
                            self.metrics.add_dropped(n);
                            Err(DbError::BufferLagged {
                                lag_count: n,
                                buffer_name: String::from("embassy spmc ring"),
                            })
                        }
                    }
                }
                EmbassyBufferInner::Watch(watch) => {
                    // Watch requires a persistent receiver to track seen values.
                    // Creating a new receiver each time causes infinite loops (always returns current value).
                    if self.watch_receiver.is_none() {
                        // SAFETY: The Arc in self.buffer keeps the Watch alive for this reader's lifetime.
                        // We extend the lifetime to 'static to store the receiver, which is safe because
                        // the receiver is just (&Watch, u64 counter) and will be dropped with the reader.
                        let watch_static: &'static embassy_sync::watch::Watch<
                            CriticalSectionRawMutex,
                            T,
                            WATCH_N,
                        > = unsafe { &*(watch as *const _) };

                        self.watch_receiver = watch_static.receiver();
                        if self.watch_receiver.is_none() {
                            return Err(DbError::BufferClosed {
                                buffer_name: String::from("embassy watch"),
                            });
                        }
                    }

                    // Use the persistent receiver to detect changes
                    if let Some(ref mut rx) = self.watch_receiver {
                        let value = rx.changed().await;
                        #[cfg(feature = "metrics")]
                        self.metrics.increment_consumed();
                        Ok(value)
                    } else {
                        Err(DbError::BufferClosed {
                            buffer_name: String::from("embassy watch"),
                        })
                    }
                }
                EmbassyBufferInner::Mailbox(channel) => {
                    let rx = channel.receiver();
                    let value = rx.receive().await;
                    #[cfg(feature = "metrics")]
                    self.metrics.increment_consumed();
                    Ok(value)
                }
            }
        })
    }

    fn try_recv(&mut self) -> Result<T, DbError> {
        match &*self.buffer {
            EmbassyBufferInner::SpmcRing(channel) => {
                // Lazily create persistent subscriber (same as recv())
                if self.spmc_subscriber.is_none() {
                    let channel_static: &'static embassy_sync::pubsub::PubSubChannel<
                        CriticalSectionRawMutex,
                        T,
                        CAP,
                        SUBS,
                        PUBS,
                    > = unsafe { &*(channel as *const _) };
                    self.spmc_subscriber = Some(
                        channel_static.subscriber().map_err(|_| {
                            defmt::error!(
                                "AimDB: SpmcRing subscriber slot exhausted (max SUBS={}). \
                                 Increase the CONSUMERS const generic on buffer_sized<CAP, CONSUMERS>. \
                                 Count one slot per .tap(), .link_to() connector, and each transform_join input.",
                                SUBS
                            );
                            DbError::BufferClosed {
                                buffer_name: String::from("embassy spmc ring"),
                            }
                        })?,
                    );
                }
                match self
                    .spmc_subscriber
                    .as_mut()
                    .unwrap()
                    .try_next_message_pure()
                {
                    Some(value) => {
                        #[cfg(feature = "metrics")]
                        self.metrics.increment_consumed();
                        Ok(value)
                    }
                    None => Err(DbError::BufferEmpty),
                }
            }
            EmbassyBufferInner::Watch(_) => {
                if let Some(ref mut rx) = self.watch_receiver {
                    match rx.try_changed() {
                        Some(value) => {
                            #[cfg(feature = "metrics")]
                            self.metrics.increment_consumed();
                            Ok(value)
                        }
                        None => Err(DbError::BufferEmpty),
                    }
                } else {
                    Err(DbError::BufferEmpty)
                }
            }
            EmbassyBufferInner::Mailbox(channel) => match channel.try_receive() {
                Ok(val) => {
                    #[cfg(feature = "metrics")]
                    self.metrics.increment_consumed();
                    Ok(val)
                }
                Err(_) => Err(DbError::BufferEmpty),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Host-test scaffolding ────────────────────────────────────────────
    // The crate links `defmt` (workspace dep) and embassy-time's
    // `defmt-timestamp-uptime`, but on the host neither a defmt logger nor a
    // time driver exists. Provide no-op stubs so the test binary links. Run via
    // the `test` Make target, or directly:
    //   cargo test -p aimdb-embassy-adapter \
    //       --no-default-features --features "alloc,embassy-sync,embassy-time"
    // (`embassy-runtime` would pull the cortex-m executor, which can't host-build.)
    #[defmt::global_logger]
    struct TestLogger;

    unsafe impl defmt::Logger for TestLogger {
        fn acquire() {}
        unsafe fn flush() {}
        unsafe fn release() {}
        unsafe fn write(_bytes: &[u8]) {}
    }

    #[defmt::panic_handler]
    fn defmt_panic() -> ! {
        core::panic!("defmt panic in host test")
    }

    // Trivial time driver so `_embassy_time_now` resolves on the host. The
    // clock is pinned at 0; `schedule_wake` wakes immediately so an
    // already-expired `Timer` (e.g. `sleep(Duration::ZERO)` in the RuntimeOps
    // contract test) completes on its next poll. Non-zero sleeps would spin
    // forever — host tests must not use them.
    struct TestTimeDriver;
    impl embassy_time_driver::Driver for TestTimeDriver {
        fn now(&self) -> u64 {
            0
        }
        fn schedule_wake(&self, _at: u64, waker: &core::task::Waker) {
            waker.wake_by_ref();
        }
    }
    embassy_time_driver::time_driver_impl!(static TEST_TIME_DRIVER: TestTimeDriver = TestTimeDriver);

    // Note: Embassy tests typically run on actual embedded targets or with embassy-executor
    // For now, these are basic compilation tests. Integration tests would need embassy-executor.

    #[test]
    fn test_buffer_creation() {
        // Test that buffers can be created
        type TestBuffer = EmbassyBuffer<u32, 10, 4, 2, 4>;
        let _spmc: TestBuffer = TestBuffer::new_spmc();
        let _watch: TestBuffer = TestBuffer::new_watch();
        let _mailbox: TestBuffer = TestBuffer::new_mailbox();
    }

    #[test]
    fn test_buffer_from_cfg() {
        type TestBuffer = EmbassyBuffer<u32, 10, 4, 2, 4>;

        let cfg1 = BufferCfg::SpmcRing { capacity: 10 };
        let _buf1: TestBuffer = Buffer::new(&cfg1);

        let cfg2 = BufferCfg::SingleLatest;
        let _buf2: TestBuffer = Buffer::new(&cfg2);

        let cfg3 = BufferCfg::Mailbox;
        let _buf3: TestBuffer = Buffer::new(&cfg3);
    }

    // ========================================================================
    // peek() Tests — non-destructive buffer-native reads (design 031)
    //
    // push()/peek() are synchronous and lock a CriticalSectionRawMutex; the
    // `critical-section` std impl in dev-dependencies provides the host-side
    // implementation, so these run without an embassy executor. Run with:
    //   cargo test -p aimdb-embassy-adapter \
    //       --no-default-features --features "alloc,embassy-sync,embassy-time"
    // ========================================================================

    use aimdb_core::buffer::DynBuffer;

    type PeekBuffer = EmbassyBuffer<i32, 8, 4, 2, 4>;

    #[test]
    fn test_peek_single_latest_empty() {
        let buffer: PeekBuffer = PeekBuffer::new_watch();
        assert_eq!(DynBuffer::peek(&buffer), None);
    }

    #[test]
    fn test_peek_single_latest_returns_latest() {
        let buffer: PeekBuffer = PeekBuffer::new_watch();
        DynBuffer::push(&buffer, 1);
        DynBuffer::push(&buffer, 2);
        DynBuffer::push(&buffer, 3);
        assert_eq!(DynBuffer::peek(&buffer), Some(3));
    }

    #[test]
    fn test_peek_single_latest_is_non_destructive() {
        let buffer: PeekBuffer = PeekBuffer::new_watch();
        DynBuffer::push(&buffer, 42);
        // Multiple peeks return the same value.
        assert_eq!(DynBuffer::peek(&buffer), Some(42));
        assert_eq!(DynBuffer::peek(&buffer), Some(42));
    }

    #[test]
    fn test_peek_mailbox_empty() {
        let buffer: PeekBuffer = PeekBuffer::new_mailbox();
        assert_eq!(DynBuffer::peek(&buffer), None);
    }

    #[test]
    fn test_peek_mailbox_returns_pending() {
        let buffer: PeekBuffer = PeekBuffer::new_mailbox();
        DynBuffer::push(&buffer, 7);
        assert_eq!(DynBuffer::peek(&buffer), Some(7));
        // Peek is non-destructive: the slot is still occupied.
        assert_eq!(DynBuffer::peek(&buffer), Some(7));
    }

    #[test]
    fn test_peek_mailbox_reflects_overwrite() {
        let buffer: PeekBuffer = PeekBuffer::new_mailbox();
        DynBuffer::push(&buffer, 1);
        DynBuffer::push(&buffer, 2);
        assert_eq!(DynBuffer::peek(&buffer), Some(2));
    }

    #[test]
    fn test_peek_spmc_ring_returns_none() {
        // PubSub has no canonical latest — see design 031 §SPMC Ring.
        let buffer: PeekBuffer = PeekBuffer::new_spmc();
        assert_eq!(DynBuffer::peek(&buffer), None);
        DynBuffer::push(&buffer, 1);
        DynBuffer::push(&buffer, 2);
        assert_eq!(DynBuffer::peek(&buffer), None);
    }
}
