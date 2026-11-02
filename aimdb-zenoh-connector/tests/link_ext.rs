//! `Ros2LinkExt` on both forms of a link: attached (`link_to` / `link_from`)
//! and detached (`OutboundLink` / `InboundLink` via `.output()` / `.input()`)
//! record the same QoS options.

#![cfg(feature = "std")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDb, AimDbBuilder, ConnectorBuilder, DbResult, InboundLink, OutboundLink};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::{Reliability, Ros2LinkExt};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Debug)]
struct Sample;

/// Registers the `ros2` scheme so the links are accepted.
struct NoopRos2;

impl ConnectorBuilder for NoopRos2 {
    #[allow(clippy::type_complexity)]
    fn build<'a>(
        &'a self,
        _db: &'a AimDb,
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
        "ros2"
    }
}

fn sample(_ctx: aimdb_core::RuntimeContext, _bytes: &[u8]) -> Result<Sample, String> {
    Ok(Sample)
}

#[tokio::test]
async fn detached_links_take_the_same_qos() {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(NoopRos2);
    builder.configure::<Sample>("attached", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        reg.link_to("ros2://cell/out")
            .with_serializer(|_ctx, _s: &Sample| Ok(Vec::new()))
            .with_depth(1)
            .with_reliability(Reliability::BestEffort)
            .finish();
        reg.link_from("ros2://cell/in")
            .with_deserializer(sample)
            .with_depth(5)
            .finish();
    });
    builder.configure::<Sample>("detached", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .output(
                OutboundLink::new("ros2://cell/out")
                    .with_serializer(|_ctx, _s: &Sample| Ok(Vec::new()))
                    .with_depth(1)
                    .with_reliability(Reliability::BestEffort),
            )
            .input(
                InboundLink::new("ros2://cell/in")
                    .with_deserializer(sample)
                    .with_depth(5),
            );
    });
    let (db, _runner) = builder.build().await.expect("build must succeed");

    let record = |key: &str| {
        let id = db.inner().resolve_str(key).unwrap();
        db.inner().storage(id).unwrap()
    };
    let (attached, detached) = (record("attached"), record("detached"));
    let out = &detached.outbound_connectors()[0].config;
    assert_eq!(&attached.outbound_connectors()[0].config, out);
    assert_eq!(
        attached.inbound_connectors()[0].config,
        detached.inbound_connectors()[0].config
    );
    assert!(out.contains(&("ros2.depth".to_string(), "1".to_string())));
    assert!(out.contains(&("ros2.reliability".to_string(), "best_effort".to_string())));
}
