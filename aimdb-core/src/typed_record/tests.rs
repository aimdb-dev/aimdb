use super::*;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::any::Any;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll};

use super::record::RecordCodec;
use crate::buffer::{BufferReader, DynBuffer};
use crate::codec::JsonCodec;
use crate::{Stamp, Stamped, WriteMetadata};

/// A sample whose stamp equals its value.
fn sample(v: u32) -> Stamped<u32> {
    Stamped::new(v, WriteMetadata::new(Stamp::from_nanos(u64::from(v))))
}

struct OneReader(Option<Stamped<u32>>);

impl BufferReader<Stamped<u32>> for OneReader {
    fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Stamped<u32>, crate::DbError>> {
        Poll::Ready(self.try_recv())
    }

    fn try_recv(&mut self) -> Result<Stamped<u32>, crate::DbError> {
        self.0.take().ok_or(crate::DbError::BufferEmpty)
    }
}

struct PeekBuffer(u32);

impl DynBuffer<Stamped<u32>> for PeekBuffer {
    fn push(&self, _value: Stamped<u32>) {}

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<Stamped<u32>> + Send> {
        Box::new(OneReader(Some(sample(self.0))))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn peek(&self) -> Option<Stamped<u32>> {
        Some(sample(self.0))
    }
}

struct TrackingCodec {
    tree_calls: Arc<AtomicUsize>,
    byte_calls: Arc<AtomicUsize>,
}

impl JsonCodec<u32> for TrackingCodec {
    fn encode(&self, value: &u32) -> Option<serde_json::Value> {
        self.tree_calls.fetch_add(1, Ordering::Relaxed);
        Some(serde_json::json!(value))
    }

    fn decode(&self, value: &serde_json::Value) -> Option<u32> {
        value.as_u64()?.try_into().ok()
    }

    fn encode_to_vec(&self, value: &u32) -> Option<Vec<u8>> {
        self.byte_calls.fetch_add(1, Ordering::Relaxed);
        serde_json::to_vec(value).ok()
    }
}

/// A buffer where every `peek` sees a newer write: value and stamp both
/// equal the number of peeks so far.
struct AdvancingBuffer(core::sync::atomic::AtomicU32);

impl DynBuffer<Stamped<u32>> for AdvancingBuffer {
    fn push(&self, _value: Stamped<u32>) {}

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<Stamped<u32>> + Send> {
        Box::new(OneReader(None))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn peek(&self) -> Option<Stamped<u32>> {
        Some(sample(self.0.fetch_add(1, Ordering::SeqCst) + 1))
    }
}

/// Design 058 §4.4: payload and stamp come from one buffer read. A path
/// that peeked twice would pair a value with a newer write's stamp.
#[test]
fn json_reads_take_payload_and_stamp_from_one_read() {
    let mut record = TypedRecord::<u32>::new();
    record.with_remote_access();
    record.buffer = Some(Arc::new(AdvancingBuffer(
        core::sync::atomic::AtomicU32::new(0),
    )));

    let tree = record.latest_json().unwrap();
    assert_eq!(tree.stamp(), Stamp::from_nanos(1));
    assert_eq!(tree.value, serde_json::json!(1));

    let bytes = record.latest_json_bytes().unwrap();
    assert_eq!(bytes.stamp(), Stamp::from_nanos(2));
    assert_eq!(bytes.value, b"2");

    let latest = record.latest().unwrap();
    assert_eq!((*latest.get(), latest.stamp()), (3, Stamp::from_nanos(3)));
}

#[test]
fn typed_latest_and_subscription_select_the_codec_byte_path() {
    let tree_calls = Arc::new(AtomicUsize::new(0));
    let byte_calls = Arc::new(AtomicUsize::new(0));
    let codec: RecordCodec<u32> = Arc::new(TrackingCodec {
        tree_calls: tree_calls.clone(),
        byte_calls: byte_calls.clone(),
    });

    let mut record = TypedRecord::<u32>::new();
    record.remote_codec = Some(codec);
    record.buffer = Some(Arc::new(PeekBuffer(17)));

    // Payloads are the codec's output for `T` alone; each carries the
    // stamp of the sample it was encoded from.
    let latest = record.latest_json_bytes().unwrap();
    assert_eq!(
        (latest.value.as_slice(), latest.stamp()),
        (&b"17"[..], Stamp::from_nanos(17))
    );

    let mut reader = record.subscribe_json().unwrap();
    let received = reader.try_recv_json_bytes().unwrap();
    assert_eq!(
        (received.value.as_slice(), received.stamp()),
        (&b"17"[..], Stamp::from_nanos(17))
    );

    assert_eq!(byte_calls.load(Ordering::Relaxed), 2);
    assert_eq!(tree_calls.load(Ordering::Relaxed), 0);

    let tree = record.latest_json().unwrap();
    assert_eq!(
        (tree.stamp(), tree.value),
        (Stamp::from_nanos(17), serde_json::json!(17))
    );
    assert_eq!(tree_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn remote_write_rejection_is_typed() {
    use crate::typed_record::events::{capture, RemoteWriteEvent};
    let mut record = TypedRecord::<u32>::new();
    record.with_remote_access();
    record.set_producer(|_, _| async {});
    let seen = Arc::new(core::sync::atomic::AtomicBool::new(false));
    let observer = {
        let seen = seen.clone();
        Arc::new(move |event: &RemoteWriteEvent<'_>| {
            let RemoteWriteEvent::Rejected {
                operation,
                record_type,
            } = event;
            assert_eq!(*operation, "set_from_json");
            assert_eq!(*record_type, core::any::type_name::<u32>());
            seen.store(true, core::sync::atomic::Ordering::Relaxed);
        })
    };
    assert!(capture::with(observer, || record.set_from_json(serde_json::json!(7))).is_err());
    assert!(seen.load(core::sync::atomic::Ordering::Relaxed));
}
