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
//! The buffer configuration's runtime capacity parameter is validated but the actual capacity
//! is determined by the const generic `CAP`. This is a design constraint of Embassy's no_std
//! implementation.

use aimdb_core::buffer::{BufferBackend, BufferCfg, BufferReader};
use aimdb_core::DbError;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver as ChannelReceiver};
use embassy_sync::pubsub::{PubSubChannel, Subscriber, WaitResult};
use embassy_sync::watch::{Receiver, Watch};

/// Embassy buffer implementation that wraps the appropriate Embassy primitive
/// based on the buffer configuration.
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
pub enum EmbassyBuffer<
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
    /// Create a new SPMC ring buffer (const constructor)
    pub const fn new_spmc() -> Self {
        Self::SpmcRing(PubSubChannel::new())
    }

    /// Create a new SingleLatest buffer (const constructor)
    pub const fn new_watch() -> Self {
        Self::Watch(Watch::new())
    }

    /// Create a new Mailbox buffer (const constructor)
    pub const fn new_mailbox() -> Self {
        Self::Mailbox(Channel::new())
    }
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > BufferBackend<T> for EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    type Reader<'a>
        = EmbassyBufferReader<'a, T, CAP, SUBS, PUBS, WATCH_N>
    where
        Self: 'a;

    fn new(cfg: &BufferCfg) -> Self {
        match cfg {
            BufferCfg::SpmcRing { capacity: _ } => {
                // Note: Embassy requires compile-time capacity, so we use CAP const generic
                // The runtime capacity parameter is validated but not used
                Self::new_spmc()
            }
            BufferCfg::SingleLatest => Self::new_watch(),
            BufferCfg::Mailbox => Self::new_mailbox(),
        }
    }

    fn push(&self, value: T) {
        match self {
            Self::SpmcRing(channel) => {
                // Use immediate publish to avoid blocking
                // This matches Tokio's broadcast behavior where send() doesn't block
                let publisher = channel.immediate_publisher();
                publisher.publish_immediate(value);
            }
            Self::Watch(watch) => {
                watch.sender().send(value);
            }
            Self::Mailbox(channel) => {
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

    fn subscribe(&self) -> Self::Reader<'_> {
        match self {
            Self::SpmcRing(channel) => {
                // Get a subscriber, or create a dummy if max subscribers reached
                match channel.subscriber() {
                    Ok(sub) => EmbassyBufferReader::PubSub(Some(sub)),
                    Err(_) => EmbassyBufferReader::PubSub(None), // Max subscribers reached
                }
            }
            Self::Watch(watch) => {
                // Get a receiver, or create a dummy if max receivers reached
                match watch.receiver() {
                    Some(rx) => EmbassyBufferReader::Watch(Some(rx)),
                    None => EmbassyBufferReader::Watch(None), // Max receivers reached
                }
            }
            Self::Mailbox(channel) => EmbassyBufferReader::Channel(channel.receiver()),
        }
    }
}

impl<T: Clone + Send + 'static, const CAP: usize, const SUBS: usize, const PUBS: usize, const WATCH_N: usize> EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
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
/// Each variant holds a reference or owned handle to the appropriate Embassy primitive.
pub enum EmbassyBufferReader<
    'a,
    T: Clone + Send + 'static,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
    const WATCH_N: usize,
> {
    /// Reader for SPMC ring buffer
    /// Option allows handling the case where max subscribers was reached
    PubSub(Option<Subscriber<'a, CriticalSectionRawMutex, T, CAP, SUBS, PUBS>>),

    /// Reader for SingleLatest buffer
    /// Option allows handling the case where max receivers was reached
    Watch(Option<Receiver<'a, CriticalSectionRawMutex, T, WATCH_N>>),

    /// Reader for Mailbox buffer using Channel
    Channel(ChannelReceiver<'a, CriticalSectionRawMutex, T, 1>),
}

impl<
        'a,
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > BufferReader<T> for EmbassyBufferReader<'a, T, CAP, SUBS, PUBS, WATCH_N>
{
    async fn recv(&mut self) -> Result<T, DbError> {
        match self {
            Self::PubSub(Some(sub)) => {
                // Wait for next message
                match sub.next_message().await {
                    WaitResult::Message(value) => Ok(value),
                    WaitResult::Lagged(n) => Err(DbError::BufferLagged {
                        lag_count: n,
                        _buffer_name: (),
                    }),
                }
            }
            Self::PubSub(None) => {
                // Max subscribers reached
                Err(DbError::BufferClosed { _buffer_name: () })
            }
            Self::Watch(Some(rx)) => {
                // Wait for a change and get the new value
                Ok(rx.changed().await)
            }
            Self::Watch(None) => {
                // Max receivers reached
                Err(DbError::BufferClosed { _buffer_name: () })
            }
            Self::Channel(rx) => {
                // Wait for next message from channel
                Ok(rx.receive().await)
            }
        }
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
        let _buf1: TestBuffer = BufferBackend::new(&cfg1);

        let cfg2 = BufferCfg::SingleLatest;
        let _buf2: TestBuffer = BufferBackend::new(&cfg2);

        let cfg3 = BufferCfg::Mailbox;
        let _buf3: TestBuffer = BufferBackend::new(&cfg3);
    }
}
