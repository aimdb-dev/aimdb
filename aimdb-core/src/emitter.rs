//! Cross-record communication via Emitter pattern
//!
//! The Emitter provides a way for records to emit data to other record types,
//! enabling reactive data flow pipelines across the database.

use core::any::{Any, TypeId};
use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};

use crate::DbResult;

// Forward declare AimDbInner (will be defined in builder.rs)
pub use crate::builder::AimDbInner;

/// Type alias for boxed future returning unit
#[allow(dead_code)]
type BoxFutureUnit = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Emitter for cross-record communication
///
/// Allows records to emit data to other record types, creating reactive data flow pipelines.
/// Clone-able (Arc-based) for passing to async tasks.
///
/// # Example
///
/// ```rust,ignore
/// async fn process_sensor(emitter: Emitter, data: SensorData) {
///     if data.value > 100.0 {
///         emitter.emit(Alert::new("High value detected")).await?;
///     }
/// }
/// ```
#[derive(Clone)]
pub struct Emitter {
    /// Runtime adapter (type-erased for storage)
    #[cfg(feature = "std")]
    pub(crate) runtime: Arc<dyn core::any::Any + Send + Sync>,

    #[cfg(not(feature = "std"))]
    pub(crate) runtime: Arc<dyn core::any::Any + Send + Sync>,

    /// Database internal state (record registry)
    pub(crate) inner: Arc<AimDbInner>,
}

impl Emitter {
    /// Creates a new emitter from a concrete runtime
    pub fn new<R>(runtime: Arc<R>, inner: Arc<AimDbInner>) -> Self
    where
        R: 'static + Send + Sync,
    {
        Self {
            runtime: runtime as Arc<dyn core::any::Any + Send + Sync>,
            inner,
        }
    }

    /// Gets a reference to the runtime (downcasted)
    ///
    /// Returns `Some(&R)` if the runtime type matches, `None` otherwise.
    pub fn runtime<R: 'static>(&self) -> Option<&R> {
        self.runtime.downcast_ref::<R>()
    }

    /// Emits a value to another record type
    ///
    /// Invokes the producer and all consumers registered for the target record type.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// async fn process(emitter: Emitter, data: SensorData) {
    ///     if data.temp > 100.0 {
    ///         emitter.emit(Alert::new("Temperature too high")).await?;
    ///     }
    /// }
    /// ```
    pub async fn emit<U>(&self, value: U) -> DbResult<()>
    where
        U: Send + 'static + Debug + Clone,
    {
        use crate::typed_record::AnyRecordExt;
        use crate::DbError;

        // Look up the record by TypeId
        let rec = self.inner.records.get(&TypeId::of::<U>()).ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: format!(
                        "No record registered for type: {:?}",
                        core::any::type_name::<U>()
                    ),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RuntimeError { _message: () }
            }
        })?;

        // Downcast to typed record
        let rec_typed = rec.as_typed::<U>().ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: "Type mismatch in record registry".into(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RuntimeError { _message: () }
            }
        })?;

        // Produce the value (calls producer and consumers)
        rec_typed.produce(self.clone(), value).await;

        Ok(())
    }

    /// Enqueues a message to an outbox for external system delivery
    ///
    /// Sends a message to an MPSC outbox channel that feeds an external system worker
    /// (MQTT, Kafka, DDS, etc.). Blocks if the channel is full (backpressure).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(Clone, Debug)]
    /// struct MqttMsg {
    ///     topic: String,
    ///     payload: Vec<u8>,
    /// }
    ///
    /// async fn process(emitter: Emitter, data: SensorData) {
    ///     emitter.enqueue(MqttMsg {
    ///         topic: "sensors/temperature".to_string(),
    ///         payload: data.to_bytes(),
    ///     }).await?;
    /// }
    /// ```
    pub async fn enqueue<T>(&self, value: T) -> DbResult<()>
    where
        T: Send + 'static,
    {
        use crate::DbError;
        use core::any::TypeId;

        let type_id = TypeId::of::<T>();

        // Retrieve and clone sender from registry
        let outboxes = self.inner.outboxes.lock();

        #[cfg(feature = "std")]
        let map = outboxes.as_ref().expect("Failed to lock outboxes");
        #[cfg(not(feature = "std"))]
        let map = &*outboxes;

        let sender = map.get(&type_id).ok_or(DbError::OutboxNotFound {
            #[cfg(feature = "std")]
            type_name: core::any::type_name::<T>().to_string(),
            #[cfg(not(feature = "std"))]
            _type_name: (),
        })?;

        // Send via type-erased trait method
        let boxed_value = Box::new(value) as Box<dyn Any + Send>;
        sender
            .send_any(boxed_value)
            .await
            .map_err(|_| DbError::OutboxClosed {
                #[cfg(feature = "std")]
                type_name: core::any::type_name::<T>().to_string(),
                #[cfg(not(feature = "std"))]
                _type_name: (),
            })
    }

    /// Tries to enqueue a message without blocking
    ///
    /// Attempts to send without blocking. Returns error immediately if channel is full.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// match emitter.try_enqueue(msg) {
    ///     Ok(()) => println!("Sent"),
    ///     Err(DbError::OutboxFull { .. }) => {
    ///         eprintln!("Outbox full, dropping message");
    ///     }
    ///     Err(e) => return Err(e),
    /// }
    /// ```
    pub fn try_enqueue<T>(&self, value: T) -> DbResult<()>
    where
        T: Send + 'static,
    {
        use crate::DbError;
        use core::any::TypeId;

        let type_id = TypeId::of::<T>();

        // Retrieve and clone sender from registry
        let outboxes = self.inner.outboxes.lock();

        #[cfg(feature = "std")]
        let map = outboxes.as_ref().expect("Failed to lock outboxes");
        #[cfg(not(feature = "std"))]
        let map = &*outboxes;

        let sender = map.get(&type_id).ok_or(DbError::OutboxNotFound {
            #[cfg(feature = "std")]
            type_name: core::any::type_name::<T>().to_string(),
            #[cfg(not(feature = "std"))]
            _type_name: (),
        })?;

        // Try to send via type-erased trait method
        let boxed_value = Box::new(value) as Box<dyn Any + Send>;
        sender.try_send_any(boxed_value).map_err(|_returned_value| {
            // Check if channel is closed to distinguish error types
            if sender.is_closed() {
                DbError::OutboxClosed {
                    #[cfg(feature = "std")]
                    type_name: core::any::type_name::<T>().to_string(),
                    #[cfg(not(feature = "std"))]
                    _type_name: (),
                }
            } else {
                // Channel is full - now we can report the actual capacity
                DbError::OutboxFull {
                    capacity: sender.capacity(),
                    #[cfg(feature = "std")]
                    type_name: core::any::type_name::<T>().to_string(),
                    #[cfg(not(feature = "std"))]
                    _type_name: (),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::any::Any;

    extern crate alloc;
    use alloc::collections::BTreeMap;

    #[allow(dead_code)]
    #[derive(Debug, Clone, PartialEq)]
    struct TestData {
        value: i32,
    }

    #[derive(Debug, Clone, PartialEq)]
    struct TestMessage {
        id: u32,
        content: &'static str,
    }

    // Mock AnySender for testing
    #[derive(Clone)]
    struct MockSender<T> {
        #[cfg(feature = "std")]
        sent_values: Arc<std::sync::Mutex<Vec<T>>>,
        #[cfg(not(feature = "std"))]
        sent_values: Arc<spin::Mutex<alloc::vec::Vec<T>>>,
        should_fail: bool,
        should_block: bool,
    }

    impl<T: Clone + Send + 'static> MockSender<T> {
        fn new(should_fail: bool, should_block: bool) -> Self {
            Self {
                #[cfg(feature = "std")]
                sent_values: Arc::new(std::sync::Mutex::new(Vec::new())),
                #[cfg(not(feature = "std"))]
                sent_values: Arc::new(spin::Mutex::new(alloc::vec::Vec::new())),
                should_fail,
                should_block,
            }
        }

        #[allow(dead_code)]
        #[cfg(feature = "std")]
        fn get_sent_values(&self) -> Vec<T> {
            self.sent_values.lock().unwrap().clone()
        }

        #[allow(dead_code)]
        #[cfg(not(feature = "std"))]
        fn get_sent_values(&self) -> alloc::vec::Vec<T> {
            self.sent_values.lock().clone()
        }
    }

    impl<T: Clone + Send + 'static> crate::outbox::AnySender for MockSender<T> {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn send_any(&self, value: Box<dyn Any + Send>) -> crate::outbox::SendFuture {
            let value = *value.downcast::<T>().expect("Type mismatch in mock sender");
            let sent_values = self.sent_values.clone();
            let should_fail = self.should_fail;

            Box::pin(async move {
                if should_fail {
                    Err(())
                } else {
                    #[cfg(feature = "std")]
                    {
                        sent_values.lock().unwrap().push(value);
                    }
                    #[cfg(not(feature = "std"))]
                    {
                        sent_values.lock().push(value);
                    }
                    Ok(())
                }
            })
        }

        fn try_send_any(&self, value: Box<dyn Any + Send>) -> Result<(), Box<dyn Any + Send>> {
            if self.should_fail {
                Err(value)
            } else if self.should_block {
                // Simulate "would block" scenario
                Err(value)
            } else {
                let value = *value.downcast::<T>().expect("Type mismatch in mock sender");
                #[cfg(feature = "std")]
                {
                    self.sent_values.lock().unwrap().push(value);
                }
                #[cfg(not(feature = "std"))]
                {
                    self.sent_values.lock().push(value);
                }
                Ok(())
            }
        }

        fn capacity(&self) -> usize {
            // Mock capacity for testing
            100
        }

        fn is_closed(&self) -> bool {
            // If should_fail is true, simulate closed channel
            self.should_fail
        }
    }

    fn create_test_emitter() -> Emitter {
        let records = BTreeMap::new();

        let inner = Arc::new(AimDbInner {
            records,
            #[cfg(feature = "std")]
            outboxes: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            #[cfg(not(feature = "std"))]
            outboxes: Arc::new(spin::Mutex::new(BTreeMap::new())),
        });

        let runtime = Arc::new(());
        Emitter::new(runtime, inner)
    }

    #[test]
    fn test_emitter_creation() {
        let _emitter = create_test_emitter();
    }

    #[test]
    fn test_emitter_clone() {
        let emitter = create_test_emitter();
        let _emitter2 = emitter.clone();
    }

    // Tests for try_enqueue method
    #[test]
    fn test_try_enqueue_outbox_not_found() {
        let emitter = create_test_emitter();
        let msg = TestMessage {
            id: 1,
            content: "test",
        };

        let result = emitter.try_enqueue(msg);
        assert!(result.is_err());

        match result {
            Err(crate::DbError::OutboxNotFound { .. }) => {
                // Expected error
            }
            _ => panic!("Expected OutboxNotFound error"),
        }
    }

    #[test]
    fn test_try_enqueue_success() {
        let emitter = create_test_emitter();

        // Register a mock sender
        let mock_sender = MockSender::<TestMessage>::new(false, false);
        let type_id = TypeId::of::<TestMessage>();

        #[cfg(feature = "std")]
        {
            let mut outboxes = emitter.inner.outboxes.lock().unwrap();
            outboxes.insert(type_id, Box::new(mock_sender.clone()));
        }
        #[cfg(not(feature = "std"))]
        {
            let mut outboxes = emitter.inner.outboxes.lock();
            outboxes.insert(type_id, Box::new(mock_sender.clone()));
        }

        let msg = TestMessage {
            id: 42,
            content: "hello",
        };

        let result = emitter.try_enqueue(msg.clone());
        assert!(result.is_ok());

        // Verify the message was sent
        let sent = mock_sender.get_sent_values();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], msg);
    }

    #[test]
    fn test_try_enqueue_channel_full() {
        let emitter = create_test_emitter();

        // Register a mock sender that simulates full channel
        let mock_sender = MockSender::<TestMessage>::new(false, true);
        let type_id = TypeId::of::<TestMessage>();

        #[cfg(feature = "std")]
        {
            let mut outboxes = emitter.inner.outboxes.lock().unwrap();
            outboxes.insert(type_id, Box::new(mock_sender));
        }
        #[cfg(not(feature = "std"))]
        {
            let mut outboxes = emitter.inner.outboxes.lock();
            outboxes.insert(type_id, Box::new(mock_sender));
        }

        let msg = TestMessage {
            id: 99,
            content: "full",
        };

        let result = emitter.try_enqueue(msg);
        assert!(result.is_err());

        match result {
            Err(crate::DbError::OutboxFull { capacity, .. }) => {
                // Capacity should now be reported from the mock sender
                assert_eq!(capacity, 100);
            }
            _ => panic!("Expected OutboxFull error"),
        }
    }

    #[test]
    fn test_try_enqueue_channel_closed() {
        let emitter = create_test_emitter();

        // Register a mock sender that simulates closed channel (should_fail=true)
        let mock_sender = MockSender::<TestMessage>::new(true, false);
        let type_id = TypeId::of::<TestMessage>();

        #[cfg(feature = "std")]
        {
            let mut outboxes = emitter.inner.outboxes.lock().unwrap();
            outboxes.insert(type_id, Box::new(mock_sender));
        }
        #[cfg(not(feature = "std"))]
        {
            let mut outboxes = emitter.inner.outboxes.lock();
            outboxes.insert(type_id, Box::new(mock_sender));
        }

        let msg = TestMessage {
            id: 88,
            content: "closed",
        };

        let result = emitter.try_enqueue(msg);
        assert!(result.is_err());

        match result {
            Err(crate::DbError::OutboxClosed { .. }) => {
                // Expected - should detect closed channel via is_closed()
            }
            _ => panic!("Expected OutboxClosed error"),
        }
    }

    // Tests for enqueue method (async)
    #[cfg(feature = "std")]
    #[tokio::test]
    async fn test_enqueue_outbox_not_found() {
        let emitter = create_test_emitter();
        let msg = TestMessage {
            id: 1,
            content: "test",
        };

        let result = emitter.enqueue(msg).await;
        assert!(result.is_err());

        match result {
            Err(crate::DbError::OutboxNotFound { .. }) => {
                // Expected error
            }
            _ => panic!("Expected OutboxNotFound error"),
        }
    }

    #[cfg(feature = "std")]
    #[tokio::test]
    async fn test_enqueue_success() {
        let emitter = create_test_emitter();

        // Register a mock sender
        let mock_sender = MockSender::<TestMessage>::new(false, false);
        let type_id = TypeId::of::<TestMessage>();

        {
            let mut outboxes = emitter.inner.outboxes.lock().unwrap();
            outboxes.insert(type_id, Box::new(mock_sender.clone()));
        }

        let msg = TestMessage {
            id: 123,
            content: "async message",
        };

        let result = emitter.enqueue(msg.clone()).await;
        assert!(result.is_ok());

        // Verify the message was sent
        let sent = mock_sender.get_sent_values();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], msg);
    }

    #[cfg(feature = "std")]
    #[tokio::test]
    async fn test_enqueue_channel_closed() {
        let emitter = create_test_emitter();

        // Register a mock sender that always fails (simulating closed channel)
        let mock_sender = MockSender::<TestMessage>::new(true, false);
        let type_id = TypeId::of::<TestMessage>();

        {
            let mut outboxes = emitter.inner.outboxes.lock().unwrap();
            outboxes.insert(type_id, Box::new(mock_sender));
        }

        let msg = TestMessage {
            id: 456,
            content: "closed",
        };

        let result = emitter.enqueue(msg).await;
        assert!(result.is_err());

        match result {
            Err(crate::DbError::OutboxClosed { .. }) => {
                // Expected error
            }
            _ => panic!("Expected OutboxClosed error"),
        }
    }

    #[cfg(feature = "std")]
    #[tokio::test]
    async fn test_enqueue_multiple_messages() {
        let emitter = create_test_emitter();

        // Register a mock sender
        let mock_sender = MockSender::<TestMessage>::new(false, false);
        let type_id = TypeId::of::<TestMessage>();

        {
            let mut outboxes = emitter.inner.outboxes.lock().unwrap();
            outboxes.insert(type_id, Box::new(mock_sender.clone()));
        }

        // Send multiple messages
        for i in 0..5 {
            let msg = TestMessage {
                id: i,
                content: "multi",
            };
            let result = emitter.enqueue(msg).await;
            assert!(result.is_ok());
        }

        // Verify all messages were sent
        let sent = mock_sender.get_sent_values();
        assert_eq!(sent.len(), 5);
        for (i, msg) in sent.iter().enumerate() {
            assert_eq!(msg.id, i as u32);
        }
    }

    #[test]
    fn test_type_erasure_safety() {
        let emitter = create_test_emitter();

        // Register sender for TestMessage
        let mock_sender = MockSender::<TestMessage>::new(false, false);
        let type_id = TypeId::of::<TestMessage>();

        #[cfg(feature = "std")]
        {
            let mut outboxes = emitter.inner.outboxes.lock().unwrap();
            outboxes.insert(type_id, Box::new(mock_sender));
        }
        #[cfg(not(feature = "std"))]
        {
            let mut outboxes = emitter.inner.outboxes.lock();
            outboxes.insert(type_id, Box::new(mock_sender));
        }

        // Try to send the correct type
        let msg = TestMessage {
            id: 1,
            content: "test",
        };
        let result = emitter.try_enqueue(msg);
        assert!(result.is_ok());

        // Trying to send a different type to same outbox would fail
        // at runtime due to type_id mismatch (caught by OutboxNotFound)
        let wrong_msg = TestData { value: 42 };
        let result = emitter.try_enqueue(wrong_msg);
        assert!(result.is_err());
        match result {
            Err(crate::DbError::OutboxNotFound { .. }) => {
                // Expected - different TypeId
            }
            _ => panic!("Expected OutboxNotFound for wrong type"),
        }
    }
}
