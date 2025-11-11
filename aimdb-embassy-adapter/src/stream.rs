//! Stream adapter for Embassy channels
//!
//! Provides a bridge between Embassy's channel-based communication and
//! Rust's standard `Stream` trait from `futures-core`. This enables
//! Embassy connectors to implement the bidirectional `Connector` trait
//! using the same API as Tokio-based connectors.
//!
//! # Architecture
//!
//! Embassy uses channels for inter-task communication, while the `Connector`
//! trait expects `Stream`. `ChannelStream` adapts Embassy channels to the
//! `Stream` interface with zero runtime overhead.
//!
//! # Example
//!
//! ```rust,ignore
//! use embassy_sync::channel::{Channel, Receiver};
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use aimdb_embassy_adapter::ChannelStream;
//! use futures_core::Stream;
//!
//! // Create a channel for MQTT events
//! static EVENTS: Channel<NoopRawMutex, MqttEvent, 32> = Channel::new();
//!
//! // Wrap receiver in Stream adapter
//! let receiver = EVENTS.receiver();
//! let mut stream = ChannelStream::new(receiver);
//!
//! // Use as a Stream
//! use futures::StreamExt;
//! while let Some(event) = stream.next().await {
//!     // Process event
//! }
//! ```

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Receiver;
use futures_core::Stream;

/// Adapter that implements `Stream` for Embassy channel receivers
///
/// Bridges Embassy's channel-based communication with Rust's standard
/// `Stream` trait. This allows Embassy connectors to use the same
/// `Connector::subscribe()` API as Tokio connectors.
///
/// # Type Parameters
/// * `M` - The raw mutex type (e.g., `NoopRawMutex` for single-threaded)
/// * `T` - The message type
/// * `N` - The channel capacity (const generic)
///
/// # Performance
///
/// Zero runtime overhead - directly forwards poll operations to the
/// underlying channel's receive future. No buffering or intermediate
/// allocations.
///
/// # Example
///
/// ```rust,ignore
/// impl Connector for MqttConnector {
///     fn subscribe(&self, _source: &str, _config: &ConnectorConfig)
///         -> Pin<Box<dyn Stream<Item = Result<Vec<u8>, PublishError>> + Send + '_>>
///     {
///         let receiver = self.event_receiver.clone();
///         
///         // Wrap channel in Stream adapter
///         let stream = ChannelStream::new(receiver)
///             .filter_map(|event| match event {
///                 MqttEvent::Message { payload, .. } => Some(Ok(payload)),
///                 MqttEvent::Error(e) => Some(Err(e.into())),
///                 _ => None,
///             });
///         
///         Box::pin(stream)
///     }
/// }
/// ```
pub struct ChannelStream<'a, M: RawMutex, T, const N: usize> {
    receiver: Receiver<'a, M, T, N>,
}

impl<'a, M: RawMutex, T, const N: usize> ChannelStream<'a, M, T, N> {
    /// Creates a new `ChannelStream` wrapping an Embassy channel receiver
    ///
    /// # Arguments
    /// * `receiver` - Embassy channel receiver to wrap
    ///
    /// # Returns
    /// A new `ChannelStream` that implements the `Stream` trait
    pub fn new(receiver: Receiver<'a, M, T, N>) -> Self {
        Self { receiver }
    }
}

impl<'a, M, T, const N: usize> Stream for ChannelStream<'a, M, T, N>
where
    M: RawMutex,
    T: Clone,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Create a future for receiving from the channel
        let receive_future = self.receiver.receive();

        // Pin and poll the receive future
        // Safety: We're pinning a newly created future on the stack
        let pinned_future = core::pin::pin!(receive_future);

        match pinned_future.poll(cx) {
            Poll::Ready(value) => Poll::Ready(Some(value)),
            Poll::Pending => Poll::Pending,
        }

        // Note: Embassy channels never close, so we never return None.
        // The stream continues indefinitely until the task is cancelled.
    }
}

// Implement Unpin for ChannelStream since Embassy channels are Unpin
impl<'a, M: RawMutex, T, const N: usize> Unpin for ChannelStream<'a, M, T, N> {}
