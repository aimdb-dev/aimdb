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
use alloc::sync::Arc;

use aimdb_core::buffer::{Buffer, BufferCfg, BufferReader};
use aimdb_core::DbError;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::pubsub::{PubSubChannel, WaitResult};
use embassy_sync::watch::Watch;

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
/// use aimdb_core::buffer::{BufferBackend, BufferCfg};
///
/// // Create an SPMC ring buffer with capacity 32, 4 subscribers, 2 publishers
/// type MyBuffer = EmbassyBuffer<u32, 32, 4, 2, 4>;
/// static BUFFER: MyBuffer = MyBuffer::new_spmc();
///
/// # async fn example() {
/// let mut reader = BUFFER.subscribe();
/// BUFFER.push(42);
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
        }
    }

    /// Create a new SingleLatest buffer
    pub fn new_watch() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::Watch(Watch::new())),
        }
    }

    /// Create a new Mailbox buffer
    pub fn new_mailbox() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::Mailbox(Channel::new())),
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
                EmbassyBufferInner::SpmcRing(channel) => match channel.subscriber() {
                    Ok(mut sub) => match sub.next_message().await {
                        WaitResult::Message(value) => Ok(value),
                        WaitResult::Lagged(n) => Err(DbError::BufferLagged {
                            lag_count: n,
                            _buffer_name: (),
                        }),
                    },
                    Err(_) => Err(DbError::BufferClosed { _buffer_name: () }),
                },
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
                            return Err(DbError::BufferClosed { _buffer_name: () });
                        }
                    }

                    // Use the persistent receiver to detect changes
                    if let Some(ref mut rx) = self.watch_receiver {
                        Ok(rx.changed().await)
                    } else {
                        Err(DbError::BufferClosed { _buffer_name: () })
                    }
                }
                EmbassyBufferInner::Mailbox(channel) => {
                    let rx = channel.receiver();
                    Ok(rx.receive().await)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
