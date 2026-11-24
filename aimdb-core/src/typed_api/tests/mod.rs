//! Tests for `typed_api`, split per concern. Shared fixtures live here;
//! child modules see them (and every private item of `typed_api`) via
//! `use super::*`.

mod build;
mod link_inbound;
mod link_outbound;
mod registrar;

use super::*;
use crate::{connector::SerializeError, DbResult};
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;

// The original inline module got these through `super::*` from the old
// `typed_api.rs` imports; after the split they live in the submodules.
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

#[cfg(not(feature = "std"))]
use alloc::vec;

#[allow(dead_code)]
#[derive(Clone, Debug)]
struct TestRecord {
    value: i32,
}

#[test]
fn test_link_address_simple() {
    use crate::connector::LinkAddress;
    let addr = LinkAddress::parse("mqtt://sensors").unwrap();
    assert_eq!(addr.scheme(), "mqtt");
    assert_eq!(addr.resource_id(), "sensors");
}

#[test]
fn test_link_address_multi_level() {
    use crate::connector::LinkAddress;
    let addr = LinkAddress::parse("mqtt://sensors/temperature").unwrap();
    assert_eq!(addr.resource_id(), "sensors/temperature");
}

#[test]
fn test_link_address_deep() {
    use crate::connector::LinkAddress;
    let addr = LinkAddress::parse("mqtt://factory/floor1/sensors/temp").unwrap();
    assert_eq!(addr.resource_id(), "factory/floor1/sensors/temp");
}

// ====================================================================
// Test infrastructure for InboundLinkBuilder deserializer tests
// ====================================================================

/// Minimal mock runtime for context tests — the builder only needs the
/// dyn-safe `RuntimeOps` surface, supplied by the shared test stub.
use crate::executor::test_support::NoopRuntimeOps as MockRuntime;

/// Minimal mock buffer so `has_buffer()` returns true
struct MockBuffer;

impl crate::buffer::DynBuffer<crate::Stamped<TestRecord>> for MockBuffer {
    fn push(&self, _value: crate::Stamped<TestRecord>) {}
    fn subscribe_boxed(
        &self,
    ) -> Box<dyn crate::buffer::BufferReader<crate::Stamped<TestRecord>> + Send> {
        unimplemented!("not needed for deserializer tests")
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// Mock connector builder that reports a given scheme
struct MockConnectorBuilder {
    scheme: String,
}

impl crate::connector::ConnectorBuilder for MockConnectorBuilder {
    fn build<'a>(
        &'a self,
        _db: &'a crate::AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>>
                + Send
                + 'a,
        >,
    > {
        unimplemented!("not needed for deserializer tests")
    }
    fn scheme(&self) -> &str {
        &self.scheme
    }
}

/// Helper: build a RecordRegistrar wired to a TypedRecord with a buffer and a
/// mock connector builder for the given scheme.
fn make_registrar<'a>(
    rec: &'a mut crate::typed_record::TypedRecord<TestRecord>,
    builders: &'a [Box<dyn crate::connector::ConnectorBuilder>],
    extensions: &'a crate::extensions::Extensions,
) -> RecordRegistrar<'a, TestRecord> {
    RecordRegistrar {
        rec,
        connector_builders: builders,
        builder_id: TEST_BUILDER,
        record_key: "test::Record".to_string(),
        key_link_address: None,
        extensions,
        last_stage: None,
        _schema: core::marker::PhantomData,
    }
}

/// Drains the configuration errors a registrar/setter recorded on `rec`.
fn drain_errors(
    rec: &mut crate::typed_record::TypedRecord<TestRecord>,
) -> Vec<crate::error::ConfigError> {
    use crate::typed_record::AnyRecord;
    rec.drain_config_errors()
}

fn ser(_ctx: crate::RuntimeContext, r: &TestRecord) -> Result<Vec<u8>, SerializeError> {
    Ok(r.value.to_le_bytes().to_vec())
}

fn de(_ctx: crate::RuntimeContext, _bytes: &[u8]) -> Result<TestRecord, crate::IngestError> {
    Ok(TestRecord { value: 0 })
}

impl From<i32> for TestRecord {
    fn from(value: i32) -> Self {
        TestRecord { value }
    }
}

/// A custom input, generic over every record type it can produce.
struct Counter;

impl<T, S: crate::SchemaState> Input<T, S> for Counter
where
    T: From<i32> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
        reg.source(|_ctx, producer| async move {
            for i in 0..3 {
                producer.produce(T::from(i));
            }
        });
    }
}

// ====================================================================
// Links through a connector handle: `OutboundLink::on` / `InboundLink::on`
// ====================================================================

/// The builder id `make_registrar` uses; real builders start at 1.
const TEST_BUILDER: u32 = 0;

/// Connector builder whose `build()` contributes nothing — for tests
/// that must get through the connector phase.
struct NoopConnectorBuilder;

impl crate::connector::ConnectorBuilder for NoopConnectorBuilder {
    fn build<'a>(
        &'a self,
        _db: &'a crate::AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn scheme(&self) -> &str {
        "mqtt"
    }
}

// ====================================================================
// Fused ingest roundtrip tests
// ====================================================================

use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

/// Buffer that records the last pushed value and a push count —
/// atomics only, so the test runs under no_std + alloc too.
struct RecordingBuffer {
    last: Arc<AtomicI32>,
    count: Arc<AtomicUsize>,
}

impl crate::buffer::DynBuffer<crate::Stamped<TestRecord>> for RecordingBuffer {
    fn push(&self, sample: crate::Stamped<TestRecord>) {
        self.last.store(sample.value.value, Ordering::SeqCst);
        self.count.fetch_add(1, Ordering::SeqCst);
    }
    fn subscribe_boxed(
        &self,
    ) -> Box<dyn crate::buffer::BufferReader<crate::Stamped<TestRecord>> + Send> {
        unimplemented!("not needed for ingest tests")
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ====================================================================
// InboundDispatch: patterns, keys and connector-build errors
// ====================================================================

use crate::topic_pattern::test_support::Plus;

/// A record `rec.in` whose buffer stores the last value and a count.
async fn inbound_db(
    links: impl FnOnce(&mut RecordRegistrar<'_, TestRecord>) + Send + 'static,
) -> (crate::AimDb, Arc<AtomicI32>, Arc<AtomicUsize>) {
    let key_rec_in_11 = crate::Name::<TestRecord>::new("rec.in");
    let last = Arc::new(AtomicI32::new(-1));
    let count = Arc::new(AtomicUsize::new(0));
    let (buf_last, buf_count) = (last.clone(), count.clone());
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder);
    builder.configure(key_rec_in_11, move |reg| {
        reg.buffer_raw(Box::new(RecordingBuffer {
            last: buf_last,
            count: buf_count,
        }));
        links(reg);
    });
    let (db, _runner) = builder.build().await.expect("build must succeed");
    (db, last, count)
}

/// The key index, or -1 without a key.
fn key_deser(
    _ctx: &crate::RuntimeContext,
    m: &crate::TopicMatch<'_>,
    _bytes: &[u8],
) -> Result<TestRecord, crate::IngestError> {
    Ok(TestRecord {
        value: m.key().map_or(-1, |k| k.index() as i32),
    })
}

fn config_errors<T>(result: crate::DbResult<T>) -> Vec<crate::ConfigError> {
    match result {
        Err(crate::DbError::InvalidConfiguration { errors }) => errors,
        Err(e) => panic!("unexpected error {e:?}"),
        Ok(_) => panic!("expected configuration errors"),
    }
}

fn le(_ctx: crate::RuntimeContext, r: &TestRecord) -> Result<Vec<u8>, SerializeError> {
    Ok(r.value.to_le_bytes().to_vec())
}
