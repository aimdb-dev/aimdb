//! Both envelope carriers feed core's fused ingestion and per-route counters.
#![cfg(all(
    feature = "migratable",
    feature = "linkable-postcard",
    target_has_atomic = "64"
))]

#[path = "fixtures/admission_contracts.rs"]
mod contracts;
use aimdb_core::buffer::{BufferReader, DynBuffer};
use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::envelope::{ContractRef, HeaderWriter, PayloadCodec};
use aimdb_core::executor::test_support::ManualClockRuntimeOps;
use aimdb_core::{
    AimDb, AimDbBuilder, BoxFuture, DbError, DbResult, ExactGrammar, InboundDispatch, Stamp,
    Stamped,
};
use aimdb_data_contracts::{
    admit_frame, link_codecs::Postcard, name_id, LinkContract, MigrationChain,
};
use contracts::{V1, V2};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use std::sync::{Arc, Mutex};

type Captured = Arc<Mutex<Vec<Stamped<V2>>>>;
struct Capture(Captured);
struct EmptyReader;
impl BufferReader<Stamped<V2>> for EmptyReader {
    fn poll_recv(&mut self, _: &mut Context<'_>) -> Poll<Result<Stamped<V2>, DbError>> {
        Poll::Pending
    }
    fn try_recv(&mut self) -> Result<Stamped<V2>, DbError> {
        Err(DbError::BufferClosed {
            buffer_name: "admission-test".into(),
        })
    }
}
impl DynBuffer<Stamped<V2>> for Capture {
    fn push(&self, sample: Stamped<V2>) {
        self.0.lock().unwrap().push(sample);
    }
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<Stamped<V2>> + Send> {
        Box::new(EmptyReader)
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
struct NoopConnector;
impl ConnectorBuilder for NoopConnector {
    fn build<'a>(
        &'a self,
        _: &'a AimDb,
    ) -> Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn scheme(&self) -> &str {
        "test"
    }
}
fn frame(
    name: &str,
    version: u32,
    shape: Option<u64>,
    codec: PayloadCodec,
    payload: &[u8],
) -> Vec<u8> {
    let mut header = [0; 19];
    let n = HeaderWriter::new(&mut header)
        .contract(ContractRef::new(name_id(name), version, shape))
        .codec(codec)
        .finish()
        .unwrap();
    let mut frame = header[..n].to_vec();
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn all_reasons_reach_route_metrics_and_later_values_are_locally_stamped() {
    futures::executor::block_on(async {
        let key_temperature_1 = aimdb_data_contracts::Key::<V2>::new("temperature");
        let captured: Captured = Default::default();
        let ops = Arc::new(ManualClockRuntimeOps::default());
        let link = LinkContract::<V2, _>::inline_from::<1>(PayloadCodec::Postcard, Postcard::<128>);
        let mut builder = AimDbBuilder::new()
            .runtime(ops.clone())
            .with_connector(NoopConnector);
        builder.configure(key_temperature_1, |r| {
            r.buffer_raw(Box::new(Capture(captured.clone())))
                .link_from("test://reading")
                .with_admitting_deserializer(&[1], move |_, _, bytes| admit_frame(bytes, &link))
                .finish();
        });
        let (db, _runner) = builder.build().await.unwrap();
        let inbound = InboundDispatch::new(&db, "test", &ExactGrammar).unwrap();
        for invalid in [
            b"bare".to_vec(),
            vec![0xa2],
            frame("humidity", 1, None, PayloadCodec::Postcard, &[]),
            frame("temperature", 3, None, PayloadCodec::Postcard, &[]),
            frame("temperature", 0, None, PayloadCodec::Postcard, &[]),
            frame("temperature", 1, Some(999), PayloadCodec::Postcard, &[]),
            frame("temperature", 1, V2::shape_of(1), PayloadCodec::Cdr, &[]),
            frame(
                "temperature",
                1,
                V2::shape_of(1),
                PayloadCodec::Postcard,
                &[],
            ),
        ] {
            inbound.dispatch("reading", &invalid);
        }
        assert!(captured.lock().unwrap().is_empty());
        let stats = inbound.stats(0).unwrap();
        assert_eq!(
            [
                stats.malformed,
                stats.unsupported_format,
                stats.contract_mismatch,
                stats.too_new,
                stats.too_old,
                stats.shape_mismatch,
                stats.codec_mismatch,
                stats.decode_error
            ],
            [1; 8]
        );
        assert_eq!(stats.accepted, 0);
        assert_eq!(stats.upgraded[0].count, 0);

        // A later sender's stamp (999) and unknown extension tag are skipped.
        let mut old = frame(
            "temperature",
            1,
            V2::shape_of(1),
            PayloadCodec::Postcard,
            &21.5_f32.to_le_bytes(),
        );
        old[1] |= 6;
        old.splice(15..15, [0xe7, 7, 4, 64, 2, 9, 9]);
        ops.clock.advance(core::time::Duration::from_nanos(4242));
        inbound.dispatch("reading", &old);
        let samples = captured.lock().unwrap();
        assert_eq!(
            samples[0].value,
            V2 {
                celsius: 21.5,
                quality: 1
            }
        );
        assert_eq!(samples[0].stamp(), Stamp::from_nanos(4242));
        drop(samples);
        let mut current = 22.0_f32.to_le_bytes().to_vec();
        current.push(8);
        ops.clock.advance(core::time::Duration::from_nanos(1));
        inbound.dispatch(
            "reading",
            &frame(
                "temperature",
                2,
                V2::shape_of(2),
                PayloadCodec::Postcard,
                &current,
            ),
        );
        assert_eq!(captured.lock().unwrap()[1].stamp(), Stamp::from_nanos(4243));
        let stats = inbound.stats(0).unwrap();
        assert_eq!(stats.accepted, 2);
        assert_eq!(stats.upgraded[0].count, 1);
        assert_eq!(stats.untracked_upgrades, 0);
    });
}

#[test]
fn a_bound_postcard_route_rejects_inline_garbage_without_producing() {
    futures::executor::block_on(async {
        let key_temperature_2 = aimdb_data_contracts::Key::<V2>::new("temperature");
        let captured: Captured = Default::default();
        let binding = LinkContract::<V2, _>::bound::<V1>(PayloadCodec::Postcard, Postcard::<128>);
        let mut builder = AimDbBuilder::new()
            .runtime(Arc::new(ManualClockRuntimeOps::default()))
            .with_connector(NoopConnector);
        builder.configure(key_temperature_2, |r| {
            r.buffer_raw(Box::new(Capture(captured.clone())))
                .link_from("test://reading")
                .with_admitting_deserializer(&[1], move |_, _, bytes| admit_frame(bytes, &binding))
                .finish();
        });
        let (db, _runner) = builder.build().await.unwrap();
        let inbound = InboundDispatch::new(&db, "test", &ExactGrammar).unwrap();
        let payload = 21.5_f32.to_le_bytes();
        inbound.dispatch(
            "reading",
            &frame(
                "temperature",
                1,
                V2::shape_of(1),
                PayloadCodec::Postcard,
                &payload,
            ),
        );
        assert!(captured.lock().unwrap().is_empty());
        assert_eq!(inbound.stats(0).unwrap().decode_error, 1);
        inbound.dispatch("reading", &payload);
        assert_eq!(captured.lock().unwrap().len(), 1);
        assert_eq!(inbound.stats(0).unwrap().upgraded[0].count, 1);
    });
}
