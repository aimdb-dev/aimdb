//! Embassy-specific MPSC channel implementation for outboxes
//!
//! This module provides the Embassy runtime implementation of outbox channels
//! using `embassy_sync::channel` for multi-producer single-consumer communication.
//!
//! # Architecture
//!
//! - Uses `embassy_sync::channel::Channel` with `'static` lifetime
//! - Allocates channels using `Box::leak` for dynamic allocation
//! - Provides `EmbassySender<T>` wrapper that implements `AnySender`
//! - Type-safe downcasting for sender retrieval
//!
//! # Static Lifetime Handling
//!
//! Embassy channels require `'static` lifetime, which means they must either:
//! 1. Be defined as `static` with const capacity (compile-time allocation)
//! 2. Be allocated with `Box::leak` (heap allocation with 'static lifetime)
//!
//! This implementation uses `Box::leak` strategy for flexibility, requiring
//! the `alloc` feature in no_std environments.
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_embassy_adapter::outbox::EmbassySender;
//! use aimdb_core::outbox::AnySender;
//!
//! // Create channel (requires alloc feature in no_std)
//! let (tx, rx) = create_outbox_channel::<MyMsg>(1024);
//!
//! // Store as type-erased
//! let any_sender: Box<dyn AnySender> = Box::new(tx);
//!
//! // Retrieve typed sender
//! let stored = any_sender.as_any()
//!     .downcast_ref::<EmbassySender<MyMsg>>()
//!     .unwrap();
//! ```

use aimdb_core::outbox::AnySender;
use core::any::Any;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

extern crate alloc;
use alloc::boxed::Box;

/// Type alias for Embassy MPSC sender with dynamic capacity
///
/// Note: Embassy channels require const N, so we use a default capacity of 64.
/// Use `create_outbox_channel_with_capacity` for custom capacities.
pub type OutboxSender<T, const N: usize = 64> =
    embassy_sync::channel::Sender<'static, CriticalSectionRawMutex, T, N>;

/// Type alias for Embassy MPSC receiver with dynamic capacity
///
/// Note: Embassy channels require const N, so we use a default capacity of 64.
/// Use `create_outbox_channel_with_capacity` for custom capacities.
pub type OutboxReceiver<T, const N: usize = 64> =
    embassy_sync::channel::Receiver<'static, CriticalSectionRawMutex, T, N>;

/// Embassy-specific sender wrapper that implements AnySender
///
/// This wrapper stores an `embassy_sync::channel::Sender<'static, T, N>` and
/// implements the `AnySender` trait for type-erased storage in the outbox registry.
///
/// # Type Parameters
///
/// * `T` - The message payload type (must be `Send + 'static`)
/// * `N` - Channel capacity (const generic, default 64)
///
/// # Static Lifetime
///
/// The sender contains a reference to a channel with `'static` lifetime,
/// which is achieved through `Box::leak` during channel creation.
///
/// # Example
///
/// ```rust,ignore
/// let (tx, rx) = create_outbox_channel::<MyMsg>(1024);
/// let sender = EmbassySender::new(tx);
///
/// // Can be stored in outbox registry
/// let any_sender: Box<dyn AnySender> = Box::new(sender);
/// ```
pub struct EmbassySender<T: Send + 'static, const N: usize = 64> {
    /// The underlying Embassy MPSC sender
    inner: embassy_sync::channel::Sender<'static, CriticalSectionRawMutex, T, N>,
}

impl<T: Send + 'static, const N: usize> EmbassySender<T, N> {
    /// Creates a new EmbassySender wrapper
    ///
    /// # Arguments
    ///
    /// * `sender` - The Embassy MPSC sender to wrap
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let (tx, _rx) = create_outbox_channel::<String>(100);
    /// let sender = EmbassySender::new(tx);
    /// ```
    pub fn new(
        sender: embassy_sync::channel::Sender<'static, CriticalSectionRawMutex, T, N>,
    ) -> Self {
        Self { inner: sender }
    }

    /// Returns a reference to the inner Embassy sender
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let sender = EmbassySender::new(tx);
    /// let inner_tx = sender.inner();
    /// inner_tx.send(msg).await;
    /// ```
    pub fn inner(&self) -> &embassy_sync::channel::Sender<'static, CriticalSectionRawMutex, T, N> {
        &self.inner
    }

    /// Consumes the wrapper and returns the inner sender
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let sender = EmbassySender::new(tx);
    /// let tx = sender.into_inner();
    /// ```
    pub fn into_inner(
        self,
    ) -> embassy_sync::channel::Sender<'static, CriticalSectionRawMutex, T, N> {
        self.inner
    }

    /// Sends a message to the channel
    ///
    /// This is a convenience method that delegates to the inner sender.
    ///
    /// # Arguments
    ///
    /// * `value` - The message to send
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// sender.send(msg).await;
    /// ```
    pub async fn send(&self, value: T) {
        self.inner.send(value).await
    }

    /// Attempts to send a message without blocking
    ///
    /// # Arguments
    ///
    /// * `value` - The message to send
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Message sent successfully
    /// * `Err(value)` - Channel full, message returned
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// match sender.try_send(msg) {
    ///     Ok(()) => println!("Sent"),
    ///     Err(msg) => eprintln!("Full, message returned"),
    /// }
    /// ```
    pub fn try_send(&self, value: T) -> Result<(), T> {
        use embassy_sync::channel::TrySendError;
        self.inner.try_send(value).map_err(|e| match e {
            TrySendError::Full(v) => v,
        })
    }

    /// Clones the sender
    ///
    /// Embassy MPSC senders are cheaply cloneable.
    pub fn clone_sender(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Send + 'static, const N: usize> AnySender for EmbassySender<T, N> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn send_any(&self, value: Box<dyn Any + Send>) -> aimdb_core::SendFuture {
        // Downcast the value to the concrete type
        let value = *value
            .downcast::<T>()
            .expect("Type mismatch in send_any - this is a bug");

        // Clone the sender for the async block
        let sender = self.inner.clone();

        Box::pin(async move {
            sender.send(value).await;
            Ok(()) // Embassy send never fails
        })
    }

    fn try_send_any(&self, value: Box<dyn Any + Send>) -> Result<(), Box<dyn Any + Send>> {
        // Downcast the value to the concrete type
        let value = *value
            .downcast::<T>()
            .expect("Type mismatch in try_send_any - this is a bug");

        self.inner.try_send(value).map_err(|v| {
            // Embassy returns the value on error
            Box::new(v) as Box<dyn Any + Send>
        })
    }
}

// Implement Clone for EmbassySender since embassy Sender is Clone
impl<T: Send + 'static, const N: usize> Clone for EmbassySender<T, N> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Creates a bounded Embassy MPSC channel for outbox use
///
/// This function allocates a channel on the heap and leaks it to obtain
/// a `'static` lifetime, which is required by Embassy's channel API.
///
/// # Memory Management
///
/// The channel is allocated with `Box::leak`, so it will never be deallocated.
/// This is acceptable for outboxes which are typically created during
/// application initialization and used for the entire application lifetime.
///
/// # Arguments
///
/// * `capacity` - The channel buffer size (currently fixed at 64 due to const generic)
///
/// # Returns
///
/// A tuple of (wrapped sender, receiver)
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_embassy_adapter::outbox::create_outbox_channel;
///
/// let (tx, rx) = create_outbox_channel::<MyMsg>(1024);
/// ```
///
/// # Note
///
/// This function requires the `alloc` feature in no_std environments
/// as it uses `Box::leak` for heap allocation. The capacity parameter is
/// currently ignored due to Embassy's const generic requirement.
pub fn create_outbox_channel<T: Send + 'static>(
    _capacity: usize,
) -> (EmbassySender<T, 64>, OutboxReceiver<T, 64>) {
    // Create channel on heap with default capacity of 64
    // We use Box::leak to convert Box<Channel> into &'static Channel
    // This is safe because outboxes are long-lived (application lifetime)
    let channel: &'static mut Channel<CriticalSectionRawMutex, T, 64> =
        Box::leak(Box::new(Channel::<CriticalSectionRawMutex, T, 64>::new()));

    // Embassy channels don't have split(), we get sender/receiver directly
    let sender = channel.sender();
    let receiver = channel.receiver();
    
    (EmbassySender::new(sender), receiver)
}

/// Creates an Embassy channel with dynamic capacity using const generics
///
/// This is an advanced function that allows specifying capacity at compile time.
/// For most use cases, prefer `create_outbox_channel()`.
///
/// # Type Parameters
///
/// * `T` - Message type
/// * `N` - Channel capacity (const generic)
///
/// # Example
///
/// ```rust,ignore
/// // Create channel with capacity of 256
/// let (tx, rx) = create_outbox_channel_with_capacity::<MyMsg, 256>();
/// ```
pub fn create_outbox_channel_with_capacity<T: Send + 'static, const N: usize>(
) -> (EmbassySender<T, N>, OutboxReceiver<T, N>) {
    let channel: &'static mut Channel<CriticalSectionRawMutex, T, N> =
        Box::leak(Box::new(Channel::<CriticalSectionRawMutex, T, N>::new()));
    
    let sender = channel.sender();
    let receiver = channel.receiver();
    
    (EmbassySender::new(sender), receiver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::outbox::AnySender;

    #[test]
    fn test_embassy_sender_creation() {
        let (tx, _rx) = create_outbox_channel::<i32>(10);
        let _sender = EmbassySender::new(tx.into_inner());
    }

    #[test]
    fn test_embassy_sender_type_erasure() {
        let (tx, _rx) = create_outbox_channel::<i32>(10);
        let sender = EmbassySender::new(tx.into_inner());

        // Store as AnySender
        let any_sender: &dyn AnySender = &sender;

        // Should be able to downcast back to EmbassySender<i32, 64>
        assert!(any_sender
            .as_any()
            .downcast_ref::<EmbassySender<i32, 64>>()
            .is_some());

        // Should fail to downcast to wrong type
        assert!(any_sender
            .as_any()
            .downcast_ref::<EmbassySender<u32, 64>>()
            .is_none());
    }

    #[test]
    fn test_embassy_sender_try_send() {
        let (sender, mut rx) = create_outbox_channel::<i32>(2);

        // Should succeed (capacity is 64)
        assert!(sender.try_send(42).is_ok());
        assert!(sender.try_send(43).is_ok());

        // Drain buffer
        assert!(rx.try_receive().is_ok());
        assert!(rx.try_receive().is_ok());

        // Should succeed again
        assert!(sender.try_send(44).is_ok());
    }

    #[test]
    fn test_embassy_sender_clone() {
        let (sender, _rx) = create_outbox_channel::<i32>(10);

        // Clone sender
        let sender2 = sender.clone();

        // Both should be usable
        assert!(sender.try_send(1).is_ok());
        assert!(sender2.try_send(2).is_ok());
    }

    #[test]
    fn test_create_outbox_channel_with_capacity() {
        let (sender, mut rx) = create_outbox_channel_with_capacity::<i32, 4>();

        // Should be able to send up to capacity
        assert!(sender.try_send(1).is_ok());
        assert!(sender.try_send(2).is_ok());
        assert!(sender.try_send(3).is_ok());
        assert!(sender.try_send(4).is_ok());

        // Should be full now
        assert!(sender.try_send(5).is_err());

        // Verify messages
        assert_eq!(rx.try_receive().unwrap(), 1);
        assert_eq!(rx.try_receive().unwrap(), 2);
    }
}
