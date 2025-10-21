//! Tokio-specific MPSC channel implementation for outboxes
//!
//! This module provides the Tokio runtime implementation of outbox channels
//! using `tokio::sync::mpsc` for multi-producer single-consumer communication.
//!
//! # Architecture
//!
//! - Uses `tokio::sync::mpsc::channel()` for bounded channels
//! - Provides `TokioSender<T>` wrapper that implements `AnySender`
//! - Type-safe downcasting for sender retrieval
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_tokio_adapter::outbox::TokioSender;
//! use aimdb_core::outbox::AnySender;
//!
//! // Create channel
//! let (tx, rx) = tokio::sync::mpsc::channel(1024);
//! let sender = TokioSender::new(tx);
//!
//! // Store as type-erased
//! let any_sender: Box<dyn AnySender> = Box::new(sender);
//!
//! // Retrieve typed sender
//! let stored = any_sender.as_any()
//!     .downcast_ref::<TokioSender<MsgType>>()
//!     .unwrap();
//! ```

use aimdb_core::outbox::{AnySender, SendFuture};
use core::any::Any;
use tokio::sync::mpsc;

/// Type alias for Tokio MPSC sender
///
/// This is the concrete sender type used by the Tokio adapter for outbox channels.
pub type OutboxSender<T> = mpsc::Sender<T>;

/// Type alias for Tokio MPSC receiver
///
/// This is the concrete receiver type used by the Tokio adapter for outbox channels.
pub type OutboxReceiver<T> = mpsc::Receiver<T>;

/// Tokio-specific sender wrapper that implements AnySender
///
/// This wrapper stores a `tokio::sync::mpsc::Sender<T>` and implements
/// the `AnySender` trait for type-erased storage in the outbox registry.
///
/// # Type Parameters
///
/// * `T` - The message payload type (must be `Send + 'static`)
///
/// # Example
///
/// ```rust,ignore
/// let (tx, rx) = tokio::sync::mpsc::channel::<MyMsg>(1024);
/// let sender = TokioSender::new(tx);
///
/// // Can be stored in outbox registry
/// let any_sender: Box<dyn AnySender> = Box::new(sender);
/// ```
pub struct TokioSender<T: Send + 'static> {
    /// The underlying Tokio MPSC sender
    inner: mpsc::Sender<T>,
}

impl<T: Send + 'static> TokioSender<T> {
    /// Creates a new TokioSender wrapper
    ///
    /// # Arguments
    ///
    /// * `sender` - The Tokio MPSC sender to wrap
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let (tx, _rx) = tokio::sync::mpsc::channel::<String>(100);
    /// let sender = TokioSender::new(tx);
    /// ```
    pub fn new(sender: mpsc::Sender<T>) -> Self {
        Self { inner: sender }
    }

    /// Returns a reference to the inner Tokio sender
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let sender = TokioSender::new(tx);
    /// let inner_tx = sender.inner();
    /// inner_tx.send(msg).await?;
    /// ```
    pub fn inner(&self) -> &mpsc::Sender<T> {
        &self.inner
    }

    /// Consumes the wrapper and returns the inner sender
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let sender = TokioSender::new(tx);
    /// let tx = sender.into_inner();
    /// ```
    pub fn into_inner(self) -> mpsc::Sender<T> {
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
    /// # Returns
    ///
    /// * `Ok(())` - Message sent successfully
    /// * `Err(value)` - Channel closed, message returned
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// sender.send(msg).await?;
    /// ```
    pub async fn send(&self, value: T) -> Result<(), mpsc::error::SendError<T>> {
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
    /// * `Err(TrySendError)` - Channel full or closed
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// match sender.try_send(msg) {
    ///     Ok(()) => println!("Sent"),
    ///     Err(e) => eprintln!("Failed: {}", e),
    /// }
    /// ```
    pub fn try_send(&self, value: T) -> Result<(), mpsc::error::TrySendError<T>> {
        self.inner.try_send(value)
    }

    /// Clones the sender
    ///
    /// Tokio MPSC senders are cheaply cloneable (Arc internally).
    pub fn clone_sender(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Send + 'static> AnySender for TokioSender<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn send_any(&self, value: Box<dyn Any + Send>) -> SendFuture {
        // Downcast the value to the concrete type
        let value = *value
            .downcast::<T>()
            .expect("Type mismatch in send_any - this is a bug");

        // Clone the sender for the async block
        let sender = self.inner.clone();

        Box::pin(async move {
            sender.send(value).await.map_err(|_| ()) // Convert SendError to ()
        })
    }

    fn try_send_any(&self, value: Box<dyn Any + Send>) -> Result<(), Box<dyn Any + Send>> {
        // Downcast the value to the concrete type
        let value = *value
            .downcast::<T>()
            .expect("Type mismatch in try_send_any - this is a bug");

        self.inner.try_send(value).map_err(|e| match e {
            mpsc::error::TrySendError::Full(v) => Box::new(v) as Box<dyn Any + Send>,
            mpsc::error::TrySendError::Closed(v) => Box::new(v) as Box<dyn Any + Send>,
        })
    }

    fn capacity(&self) -> usize {
        // Tokio channels expose max_capacity() which returns the capacity
        self.inner.max_capacity()
    }

    fn is_closed(&self) -> bool {
        // Tokio channels expose is_closed() method
        self.inner.is_closed()
    }
}

// Implement Clone for TokioSender since mpsc::Sender is Clone
impl<T: Send + 'static> Clone for TokioSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Creates a bounded Tokio MPSC channel for outbox use
///
/// This is a convenience function that creates a channel and wraps
/// the sender in `TokioSender`.
///
/// # Arguments
///
/// * `capacity` - The channel buffer size
///
/// # Returns
///
/// A tuple of (wrapped sender, receiver)
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_tokio_adapter::outbox::create_outbox_channel;
///
/// let (tx, rx) = create_outbox_channel::<MyMsg>(1024);
/// ```
pub fn create_outbox_channel<T: Send + 'static>(
    capacity: usize,
) -> (TokioSender<T>, OutboxReceiver<T>) {
    let (tx, rx) = mpsc::channel(capacity);
    (TokioSender::new(tx), rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::outbox::AnySender;

    #[tokio::test]
    async fn test_tokio_sender_creation() {
        let (tx, _rx) = mpsc::channel::<i32>(10);
        let sender = TokioSender::new(tx);

        // Should be able to send
        assert!(sender.send(42).await.is_ok());
    }

    #[tokio::test]
    async fn test_tokio_sender_type_erasure() {
        let (tx, _rx) = mpsc::channel::<String>(10);
        let sender = TokioSender::new(tx);

        // Store as AnySender
        let any_sender: &dyn AnySender = &sender;

        // Should be able to downcast back to TokioSender<String>
        assert!(any_sender
            .as_any()
            .downcast_ref::<TokioSender<String>>()
            .is_some());

        // Should fail to downcast to wrong type
        assert!(any_sender
            .as_any()
            .downcast_ref::<TokioSender<i32>>()
            .is_none());
    }

    #[tokio::test]
    async fn test_tokio_sender_send() {
        let (tx, mut rx) = mpsc::channel::<String>(10);
        let sender = TokioSender::new(tx);

        // Send message
        sender.send("test".to_string()).await.unwrap();

        // Receive message
        assert_eq!(rx.recv().await.unwrap(), "test");
    }

    #[tokio::test]
    async fn test_tokio_sender_try_send() {
        let (tx, mut rx) = mpsc::channel::<i32>(2);
        let sender = TokioSender::new(tx);

        // Should succeed
        assert!(sender.try_send(1).is_ok());
        assert!(sender.try_send(2).is_ok());

        // Should fail (buffer full)
        assert!(sender.try_send(3).is_err());

        // Drain buffer
        assert_eq!(rx.recv().await.unwrap(), 1);

        // Should succeed again
        assert!(sender.try_send(3).is_ok());
    }

    #[tokio::test]
    async fn test_tokio_sender_clone() {
        let (tx, mut rx) = mpsc::channel::<i32>(10);
        let sender = TokioSender::new(tx);

        // Clone sender
        let sender2 = sender.clone();

        // Both can send
        sender.send(1).await.unwrap();
        sender2.send(2).await.unwrap();

        // Both messages received
        assert_eq!(rx.recv().await.unwrap(), 1);
        assert_eq!(rx.recv().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_create_outbox_channel() {
        let (sender, mut rx) = create_outbox_channel::<String>(100);

        sender.send("hello".to_string()).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), "hello");
    }

    #[test]
    fn test_tokio_sender_into_inner() {
        let (tx, _rx) = mpsc::channel::<i32>(10);
        let sender = TokioSender::new(tx.clone());

        let inner = sender.into_inner();

        // Should have same channel
        assert_eq!(inner.capacity(), tx.capacity());
    }
}
