//! What a connector pulling from `OutboundRoutes` sees after an outage, on the
//! Embassy buffers: the same cases as the Tokio adapter's
//! `outage_semantics_per_buffer_type`, driven on the host with a no-op waker.
#![cfg(all(feature = "embassy-sync", feature = "embassy-time"))]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use aimdb_core::buffer::DynBuffer;
use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::executor::test_support::NoopRuntimeOps;
use aimdb_core::{AimDb, AimDbBuilder, DbResult, OutboundRoutes};
use aimdb_embassy_adapter::EmbassyBuffer;
use futures::executor::block_on;

// No-op defmt logger + host time driver, so the binary links.
aimdb_embassy_adapter::host_test_stubs!();

type Futures = Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>;

/// Lets `link_to("test://…")` register; drives nothing.
struct TestConnector;

impl ConnectorBuilder for TestConnector {
    fn build<'a>(
        &'a self,
        _db: &'a AimDb,
    ) -> Pin<Box<dyn Future<Output = DbResult<Futures>> + Send + 'a>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn scheme(&self) -> &str {
        "test"
    }
}

const KEYS: [&str; 3] = ["r0", "r1", "r2"];

/// One record per buffer, keyed `r0`, `r1`, …, each linked to `test://r{i}`.
fn db(buffers: Vec<Box<dyn DynBuffer<u32>>>) -> AimDb {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(NoopRuntimeOps))
        .with_connector(TestConnector);
    for (i, buffer) in buffers.into_iter().enumerate() {
        builder.configure::<u32>(KEYS[i], move |reg| {
            reg.buffer_raw(buffer)
                .link_to(&format!("test://r{i}"))
                .with_serializer(|_ctx, v: &u32| Ok(v.to_le_bytes().to_vec()))
                .finish();
        });
    }
    block_on(builder.build()).expect("build").0
}

/// Every message available now, as (route, value).
fn drain(o: &mut OutboundRoutes) -> Vec<(usize, u32)> {
    let mut cx = Context::from_waker(Waker::noop());
    let mut out = Vec::new();
    while let Poll::Ready(Some(m)) = o.poll_next(&mut cx) {
        out.push((
            m.route.id,
            u32::from_le_bytes(m.payload.as_slice().try_into().unwrap()),
        ));
    }
    out
}

fn values(got: &[(usize, u32)], route: usize) -> Vec<u32> {
    got.iter()
        .filter(|(id, _)| *id == route)
        .map(|(_, v)| *v)
        .collect()
}

#[test]
fn outage_semantics_per_buffer_type() {
    let db = db(vec![
        Box::new(EmbassyBuffer::<u32, 16, 4, 4, 4>::new_watch()),
        Box::new(EmbassyBuffer::<u32, 16, 4, 4, 4>::new_mailbox()),
        Box::new(EmbassyBuffer::<u32, 16, 4, 4, 4>::new_spmc()),
    ]);
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    for v in 0..5 {
        for key in KEYS {
            db.produce::<u32>(key, v).unwrap();
        }
    }
    let got = drain(&mut o);
    assert_eq!(values(&got, 0), [4], "single-latest");
    assert_eq!(values(&got, 1), [4], "mailbox");
    assert_eq!(values(&got, 2), [0, 1, 2, 3, 4], "spmc ring");
}

#[test]
fn an_spmc_ring_that_overflows_reports_lag_then_recovers() {
    let db = db(vec![Box::new(EmbassyBuffer::<u32, 4, 4, 4, 4>::new_spmc())]);
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    for v in 0..10 {
        db.produce::<u32>("r0", v).unwrap();
    }
    assert_eq!(values(&drain(&mut o), 0), [6, 7, 8, 9]);
    assert_eq!(o.stats(0).unwrap().lagged, 6);
}
