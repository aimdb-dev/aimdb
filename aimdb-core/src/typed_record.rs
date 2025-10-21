//! Type-safe record storage using TypeId
//!
//! This module provides a type-safe alternative to string-based record
//! identification, using Rust's `TypeId` for compile-time type safety.

use core::any::Any;
use core::fmt::Debug;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc, vec::Vec};

use crate::buffer::DynBuffer;
use crate::metrics::CallStats;
use crate::tracked_fn::TrackedAsyncFn;

/// Type-erased trait for records
///
/// Allows storage of heterogeneous record types in a single collection
/// while maintaining type safety through downcast operations.
pub trait AnyRecord: Send + Sync {
    /// Validates that the record has correct producer/consumer setup
    ///
    /// Rules: Must have exactly one producer and at least one consumer.
    fn validate(&self) -> Result<(), &'static str>;

    /// Returns self as Any for downcasting
    fn as_any(&self) -> &dyn Any;

    /// Returns self as mutable Any for downcasting
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

// Helper extension trait for type-safe downcasting
pub trait AnyRecordExt {
    /// Attempts to downcast to a typed record reference
    ///
    /// # Type Parameters
    /// * `T` - The expected record type
    ///
    /// # Returns
    /// `Some(&TypedRecord<T>)` if types match, `None` otherwise
    fn as_typed<T: Send + 'static + Debug + Clone>(&self) -> Option<&TypedRecord<T>>;

    /// Attempts to downcast to a mutable typed record reference
    ///
    /// # Type Parameters
    /// * `T` - The expected record type
    ///
    /// # Returns
    /// `Some(&mut TypedRecord<T>)` if types match, `None` otherwise
    fn as_typed_mut<T: Send + 'static + Debug + Clone>(&mut self) -> Option<&mut TypedRecord<T>>;
}

impl AnyRecordExt for Box<dyn AnyRecord> {
    fn as_typed<T: Send + 'static + Debug + Clone>(&self) -> Option<&TypedRecord<T>> {
        self.as_any().downcast_ref::<TypedRecord<T>>()
    }

    fn as_typed_mut<T: Send + 'static + Debug + Clone>(&mut self) -> Option<&mut TypedRecord<T>> {
        self.as_any_mut().downcast_mut::<TypedRecord<T>>()
    }
}

/// Typed record storage with producer/consumer functions
///
/// Stores type-safe producer and consumer functions for a specific record type,
/// with optional buffering for async dispatch patterns.
pub struct TypedRecord<T: Send + 'static + Debug + Clone> {
    /// Optional producer function
    producer: Option<TrackedAsyncFn<T>>,

    /// List of consumer functions
    consumers: Vec<TrackedAsyncFn<T>>,

    /// Optional buffer for async dispatch
    /// When present, produce() enqueues to buffer instead of direct call
    buffer: Option<Box<dyn DynBuffer<T>>>,

    /// List of connector links for external system integration
    /// Each link represents a protocol connector (MQTT, Kafka, HTTP, etc.)
    connectors: Vec<crate::connector::ConnectorLink>,
}

impl<T: Send + 'static + Debug + Clone> TypedRecord<T> {
    /// Creates a new empty typed record
    ///
    /// # Returns
    /// A `TypedRecord<T>` with no producer or consumers
    pub fn new() -> Self {
        Self {
            producer: None,
            consumers: Vec::new(),
            buffer: None,
            connectors: Vec::new(),
        }
    }

    /// Sets the producer function for this record
    ///
    /// # Arguments
    /// * `f` - An async function taking `(Emitter, T)` and returning `()`
    ///
    /// # Panics
    /// Panics if a producer is already set (each record can have only one producer)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// record.set_producer(|emitter, data| async move {
    ///     println!("Processing: {:?}", data);
    ///     // Can emit to other record types
    ///     emitter.emit(OtherType(data.value)).await?;
    /// });
    /// ```
    pub fn set_producer<F, Fut>(&mut self, f: F)
    where
        F: Fn(crate::emitter::Emitter, T) -> Fut + Send + Sync + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        if self.producer.is_some() {
            panic!("This record type already has a producer");
        }
        self.producer = Some(TrackedAsyncFn::new(f));
    }

    /// Adds a consumer function for this record
    ///
    /// Multiple consumers can be registered for the same record type.
    /// They will all be called when data is produced.
    ///
    /// # Arguments
    /// * `f` - An async function taking `(Emitter, T)` and returning `()`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// record.add_consumer(|emitter, data| async move {
    ///     println!("Consumer 1: {:?}", data);
    /// });
    /// record.add_consumer(|emitter, data| async move {
    ///     println!("Consumer 2: {:?}", data);
    /// });
    /// ```
    pub fn add_consumer<F, Fut>(&mut self, f: F)
    where
        F: Fn(crate::emitter::Emitter, T) -> Fut + Send + Sync + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        self.consumers.push(TrackedAsyncFn::new(f));
    }

    /// Sets the buffer for this record
    ///
    /// When a buffer is set, `produce()` will enqueue values instead of
    /// calling producer/consumers directly. A separate dispatcher task
    /// should drain the buffer and invoke the functions.
    ///
    /// # Arguments
    /// * `buffer` - A buffer backend implementation
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// // Configure buffer (adapter-specific implementation)
    /// let buffer = runtime.create_buffer(BufferCfg::SpmcRing { capacity: 1024 });
    /// record.set_buffer(buffer);
    /// ```
    pub fn set_buffer(&mut self, buffer: Box<dyn DynBuffer<T>>) {
        self.buffer = Some(buffer);
    }

    /// Returns whether a buffer is configured
    ///
    /// # Returns
    /// `true` if buffer is set, `false` otherwise
    pub fn has_buffer(&self) -> bool {
        self.buffer.is_some()
    }

    /// Returns a reference to the buffer if present
    ///
    /// # Returns
    /// `Some(&dyn DynBuffer<T>)` if buffer is set, `None` otherwise
    pub fn buffer(&self) -> Option<&dyn DynBuffer<T>> {
        self.buffer.as_deref()
    }

    /// Adds a connector link for external system integration
    ///
    /// Connectors bridge AimDB records to external protocols (MQTT, Kafka, HTTP, etc.).
    /// Multiple connectors can be registered for the same record type.
    ///
    /// # Arguments
    /// * `link` - The connector configuration
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_core::connector::{ConnectorUrl, ConnectorLink};
    ///
    /// let url = ConnectorUrl::parse("mqtt://broker.local:1883")?;
    /// let link = ConnectorLink::new(url);
    /// record.add_connector(link);
    /// ```
    pub fn add_connector(&mut self, link: crate::connector::ConnectorLink) {
        self.connectors.push(link);
    }

    /// Returns a reference to the registered connectors
    ///
    /// # Returns
    /// A slice of connector links
    pub fn connectors(&self) -> &[crate::connector::ConnectorLink] {
        &self.connectors
    }

    /// Returns the number of registered connectors
    ///
    /// # Returns
    /// The count of connectors
    pub fn connector_count(&self) -> usize {
        self.connectors.len()
    }

    /// Produces a value by calling producer and all consumers
    ///
    /// This is the core of the data flow mechanism:
    /// 1. If buffer is configured: enqueues value to buffer (async dispatch)
    /// 2. Otherwise: calls producer function (if set) and all consumers directly
    ///
    /// # Arguments
    /// * `emitter` - The emitter context for cross-record communication
    /// * `val` - The value to produce
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// record.produce(emitter.clone(), SensorData { temp: 23.5 }).await;
    /// ```
    pub async fn produce(&self, emitter: crate::emitter::Emitter, val: T) {
        // If buffer is configured, enqueue instead of direct call
        if let Some(buf) = &self.buffer {
            // DynBuffer::push is synchronous and non-blocking
            buf.push(val.clone());
            return;
        }

        // No buffer: direct synchronous call path
        // Call producer if present
        if let Some(p) = &self.producer {
            p.call(emitter.clone(), val.clone()).await;
        }

        // Call all consumers
        for c in &self.consumers {
            c.call(emitter.clone(), val.clone()).await;
        }
    }

    /// Returns statistics for the producer function
    ///
    /// # Returns
    /// `Some(Arc<CallStats<T>>)` if producer is set, `None` otherwise
    pub fn producer_stats(&self) -> Option<Arc<CallStats<T>>> {
        self.producer.as_ref().map(|p| p.stats())
    }

    /// Returns statistics for all consumer functions
    ///
    /// # Returns
    /// A vector of `Arc<CallStats<T>>`, one for each consumer
    pub fn consumer_stats(&self) -> Vec<Arc<CallStats<T>>> {
        self.consumers.iter().map(|c| c.stats()).collect()
    }
}

impl<T: Send + 'static + Debug + Clone> Default for TypedRecord<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Send + 'static + Debug + Clone> AnyRecord for TypedRecord<T> {
    fn validate(&self) -> Result<(), &'static str> {
        if self.producer.is_none() {
            return Err("must have exactly one producer");
        }
        if self.consumers.is_empty() {
            return Err("must have ≥1 consumer");
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct TestData {
        value: i32,
    }

    #[test]
    fn test_typed_record_new() {
        let record = TypedRecord::<TestData>::new();
        assert!(record.producer.is_none());
        assert!(record.consumers.is_empty());
    }

    #[test]
    fn test_typed_record_validation() {
        let mut record = TypedRecord::<TestData>::new();

        // No producer, no consumers - invalid
        assert!(record.validate().is_err());

        // Add producer - still invalid (no consumers)
        record.set_producer(|_em, _data| async {});
        assert!(record.validate().is_err());

        // Add consumer - now valid
        record.add_consumer(|_em, _data| async {});
        assert!(record.validate().is_ok());
    }

    #[test]
    #[should_panic(expected = "already has a producer")]
    fn test_typed_record_duplicate_producer() {
        let mut record = TypedRecord::<TestData>::new();
        record.set_producer(|_em, _data| async {});
        record.set_producer(|_em, _data| async {}); // Should panic
    }

    #[test]
    fn test_any_record_downcast() {
        use super::AnyRecordExt;

        let mut record: Box<dyn AnyRecord> = Box::new(TypedRecord::<TestData>::new());

        // Should successfully downcast to correct type
        assert!(record.as_typed::<TestData>().is_some());
        assert!(record.as_typed_mut::<TestData>().is_some());

        // Should fail to downcast to wrong type
        assert!(record.as_typed::<i32>().is_none());
    }

    #[test]
    fn test_buffer_setter_and_getter() {
        use crate::buffer::{Buffer, BufferCfg, BufferReader, DynBuffer};
        use crate::DbError;
        use core::future::Future;
        use core::pin::Pin;

        // Mock buffer for testing
        struct MockBuffer;

        impl Buffer<TestData> for MockBuffer {
            type Reader = MockReader;

            fn new(_cfg: &BufferCfg) -> Self {
                MockBuffer
            }

            fn push(&self, _value: TestData) {
                // No-op
            }

            fn subscribe(&self) -> Self::Reader {
                MockReader
            }
        }

        struct MockReader;

        impl BufferReader<TestData> for MockReader {
            fn recv(
                &mut self,
            ) -> Pin<Box<dyn Future<Output = Result<TestData, DbError>> + Send + '_>> {
                Box::pin(async {
                    Err(DbError::BufferClosed {
                        #[cfg(feature = "std")]
                        buffer_name: "mock".to_string(),
                        #[cfg(not(feature = "std"))]
                        _buffer_name: (),
                    })
                })
            }
        }

        // Note: DynBuffer is auto-implemented via blanket impl

        let mut record = TypedRecord::<TestData>::new();

        // Initially no buffer
        assert!(!record.has_buffer());
        assert!(record.buffer().is_none());

        // Set buffer
        let buffer: Box<dyn DynBuffer<TestData>> = Box::new(MockBuffer);
        record.set_buffer(buffer);

        // Now has buffer
        assert!(record.has_buffer());
        assert!(record.buffer().is_some());
    }
}
