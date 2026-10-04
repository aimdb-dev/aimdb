//! `OutboundRoutes` on real Tokio buffers: round-robin, topics, serializers,
//! lag, outage semantics per buffer type, cancellation and shutdown.

use core::fmt::Write as _;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::{ConnectorBuilder, SerializeError, TopicProvider};
use aimdb_core::{AimDb, AimDbBuilder, DbResult, OutboundPayload, OutboundRoutes, RecordRegistrar};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

#[derive(Clone, Debug)]
struct V(u32);

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

fn le(_ctx: aimdb_core::RuntimeContext, v: &V) -> Result<Vec<u8>, SerializeError> {
    Ok(v.0.to_le_bytes().to_vec())
}

type Configure = Box<dyn FnOnce(&mut RecordRegistrar<'_, V>) + Send>;

const KEYS: [&str; 4] = ["r0", "r1", "r2", "r3"];

/// One record per entry, keyed `r0`, `r1`, …, in route order.
async fn db(records: Vec<Configure>) -> AimDb {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(TestConnector);
    for (i, configure) in records.into_iter().enumerate() {
        builder.configure::<V>(KEYS[i], move |reg| configure(reg));
    }
    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());
    db
}

/// A record with `cfg` linked to `test://r{i}` with the owned serializer.
fn plain(i: usize, cfg: BufferCfg) -> Configure {
    Box::new(move |reg| {
        reg.buffer(cfg)
            .link_to(&format!("test://r{i}"))
            .with_serializer(le)
            .finish();
    })
}

fn spmc(i: usize, capacity: usize) -> Configure {
    plain(i, BufferCfg::SpmcRing { capacity })
}

fn produce(db: &AimDb, i: usize, v: u32) {
    db.produce::<V>(KEYS[i], V(v)).unwrap();
}

/// Route, topic and payload of the next message.
async fn pull(o: &mut OutboundRoutes) -> Option<(usize, String, Vec<u8>)> {
    let m = o.next().await?;
    Some((m.route.id, m.topic.to_string(), m.payload.into_vec()))
}

/// The next message, or `None` if none comes within 100 ms.
async fn try_pull(o: &mut OutboundRoutes) -> Option<(usize, String, Vec<u8>)> {
    tokio::time::timeout(Duration::from_millis(100), pull(o))
        .await
        .ok()
        .flatten()
}

fn value(payload: &[u8]) -> u32 {
    u32::from_le_bytes(payload.try_into().unwrap())
}

/// Values pulled until none comes within 100 ms.
async fn drain(o: &mut OutboundRoutes) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    while let Some((id, _, payload)) = try_pull(o).await {
        out.push((id, value(&payload)));
    }
    out
}

#[tokio::test]
async fn no_outbound_links_is_done_on_the_first_poll() {
    let db = db(vec![Box::new(|reg| {
        reg.buffer(BufferCfg::SingleLatest);
    })])
    .await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    assert!(o.routes().is_empty());
    let mut cx = Context::from_waker(Waker::noop());
    assert_eq!(o.poll_stage(&mut cx), Poll::Ready(None));
}

#[tokio::test]
async fn routes_are_served_round_robin() {
    let db = db((0..3).map(|i| spmc(i, 16)).collect()).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    for v in 0..3 {
        for i in 0..3 {
            produce(&db, i, v);
        }
    }
    let order: Vec<usize> = drain(&mut o).await.into_iter().map(|(id, _)| id).collect();
    assert_eq!(order, [0, 1, 2, 0, 1, 2, 0, 1, 2]);
}

#[tokio::test]
async fn a_hot_route_does_not_starve_a_quiet_one() {
    let db = db(vec![spmc(0, 256), spmc(1, 16)]).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    for v in 0..200 {
        produce(&db, 0, v);
    }
    produce(&db, 1, 0);
    let first_two = [pull(&mut o).await.unwrap().0, pull(&mut o).await.unwrap().0];
    assert!(first_two.contains(&1), "{first_two:?}");
}

#[tokio::test]
async fn written_and_default_topics_and_overflow() {
    let db = db(vec![
        Box::new(|reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
                .link_to("test://default")
                .with_topic_fn(8, |v, out| {
                    if v.0 == 0 {
                        return Ok(false);
                    }
                    write!(out, "t/{}", v.0)?;
                    Ok(true)
                })
                .with_serializer(le)
                .finish();
        }),
        Box::new(|reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
                .link_to("test://ignored")
                .with_topic_fn(4, |v, out| {
                    let _ = write!(out, "t/{}", v.0);
                    Ok(true)
                })
                .with_serializer(le)
                .finish();
        }),
    ])
    .await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    assert_eq!(o.routes()[0].topic_capacity, 8);

    // "t/1234567" is 9 bytes.
    for v in [5, 0, 1_234_567, 6] {
        produce(&db, 0, v);
    }
    let mut got = Vec::new();
    while let Some((_, topic, payload)) = try_pull(&mut o).await {
        got.push((topic, value(&payload)));
    }
    assert_eq!(
        got,
        [
            ("t/5".to_string(), 5),
            ("default".to_string(), 0),
            ("t/6".to_string(), 6)
        ]
    );
    assert_eq!(o.stats(0).unwrap().topic_overflow, 1);

    // A writer that ignores the overflow still has its value skipped.
    produce(&db, 1, 123);
    produce(&db, 1, 7);
    assert_eq!(drain(&mut o).await, [(1, 7)]);
    assert_eq!(o.stats(1).unwrap().topic_overflow, 1);
}

#[tokio::test]
async fn scratch_owned_fallback_and_serializer_failures() {
    let db = db(vec![Box::new(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .link_to("test://r0")
            .with_serializer(|_ctx, v: &V| {
                if v.0 == 4 {
                    return Err(SerializeError::InvalidData);
                }
                Ok(v.0.to_le_bytes().to_vec())
            })
            .with_serializer_into(4, |_ctx, v: &V, out| match v.0 {
                2 | 4 => Err(SerializeError::BufferTooSmall),
                3 => Ok(99),
                _ => {
                    out[..4].copy_from_slice(&v.0.to_le_bytes());
                    Ok(4)
                }
            })
            .finish();
    })])
    .await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    assert_eq!(o.routes()[0].payload_capacity, 4);
    for v in 1..=5 {
        produce(&db, 0, v);
    }

    let m = o.next().await.unwrap();
    assert_eq!(m.payload, OutboundPayload::Borrowed(&1u32.to_le_bytes()));
    let m = o.next().await.unwrap();
    assert_eq!(
        m.payload,
        OutboundPayload::Owned(2u32.to_le_bytes().to_vec())
    );
    // 3: invalid length, 4: fallback fails; both skipped.
    let m = o.next().await.unwrap();
    assert_eq!(m.payload, OutboundPayload::Borrowed(&5u32.to_le_bytes()));
    let stats = o.stats(0).unwrap();
    assert_eq!((stats.sent, stats.serialize_failed), (3, 2));
}

#[tokio::test]
async fn lag_is_reported_then_recovered() {
    let db = db(vec![spmc(0, 4)]).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    for v in 0..10 {
        produce(&db, 0, v);
    }
    assert_eq!(drain(&mut o).await, [(0, 6), (0, 7), (0, 8), (0, 9)]);
    assert_eq!(o.stats(0).unwrap().lagged, 6);
}

#[tokio::test]
async fn outage_semantics_per_buffer_type() {
    let db = db(vec![
        plain(0, BufferCfg::SingleLatest),
        plain(1, BufferCfg::Mailbox),
        spmc(2, 16),
    ])
    .await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    for v in 0..5 {
        for i in 0..3 {
            produce(&db, i, v);
        }
    }
    let got = drain(&mut o).await;
    let of = |route| -> Vec<u32> {
        got.iter()
            .filter(|(id, _)| *id == route)
            .map(|(_, v)| *v)
            .collect()
    };
    assert_eq!(of(0), [4], "single-latest");
    assert_eq!(of(1), [4], "mailbox");
    assert_eq!(of(2), [0, 1, 2, 3, 4], "spmc ring");
}

#[tokio::test]
async fn a_dropped_pending_next_loses_nothing() {
    let db = db(vec![spmc(0, 16)]).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    assert!(try_pull(&mut o).await.is_none(), "dropped while pending");
    produce(&db, 0, 1);
    assert_eq!(drain(&mut o).await, [(0, 1)]);
}

#[tokio::test]
async fn a_staged_value_that_was_not_taken_is_returned_again() {
    let db = db(vec![spmc(0, 16), spmc(1, 16)]).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    produce(&db, 1, 7);
    produce(&db, 1, 8);
    let id = poll_fn(|cx| o.poll_stage(cx)).await;
    assert_eq!(id, Some(1));
    assert_eq!(
        poll_fn(|cx| o.poll_stage(cx)).await,
        Some(1),
        "not replaced"
    );
    assert_eq!(drain(&mut o).await, [(1, 7), (1, 8)]);
}

#[tokio::test]
async fn values_survive_a_select_that_loses_every_third_poll() {
    let db = db(vec![spmc(0, 512)]).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    let producer = {
        let db = db.clone();
        tokio::spawn(async move {
            for v in 0..500 {
                produce(&db, 0, v);
                tokio::task::yield_now().await;
            }
        })
    };
    let mut got = Vec::new();
    let mut i = 0u32;
    while got.len() < 500 {
        i += 1;
        // Unbiased: on every third iteration the stage arm may be polled,
        // return `Pending`, and be dropped when the yield arm wins.
        tokio::select! {
            _ = tokio::task::yield_now(), if i.is_multiple_of(3) => {}
            id = poll_fn(|cx| o.poll_stage(cx)) => {
                assert_eq!(id, Some(0));
                let m = o.take_staged().unwrap();
                got.push(value(m.payload.as_slice()));
            }
        }
    }
    producer.await.unwrap();
    assert_eq!(got, (0..500).collect::<Vec<_>>());
}

#[tokio::test]
async fn a_skipped_value_does_not_lose_the_next_wake_up() {
    let db = db(vec![Box::new(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .link_to("test://r0")
            .with_serializer(|_ctx, v: &V| {
                if v.0 == 13 {
                    return Err(SerializeError::InvalidData);
                }
                Ok(v.0.to_le_bytes().to_vec())
            })
            .finish();
    })])
    .await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    let producer = {
        let db = db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            produce(&db, 0, 13);
            tokio::time::sleep(Duration::from_millis(20)).await;
            produce(&db, 0, 14);
        })
    };
    let got = tokio::time::timeout(Duration::from_secs(5), pull(&mut o))
        .await
        .expect("woken after the skipped value")
        .unwrap();
    assert_eq!(value(&got.2), 14);
    producer.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wakes_from_other_threads_lose_nothing() {
    const ROUTES: usize = 4;
    const PER_ROUTE: u32 = 2_500;
    let db = db((0..ROUTES).map(|i| spmc(i, 4096)).collect()).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    let producers: Vec<_> = (0..ROUTES)
        .map(|i| {
            let db = db.clone();
            tokio::spawn(async move {
                for v in 0..PER_ROUTE {
                    produce(&db, i, v);
                    if v.is_multiple_of(64) {
                        tokio::task::yield_now().await;
                    }
                }
            })
        })
        .collect();
    let mut next = [0u32; ROUTES];
    for _ in 0..ROUTES as u32 * PER_ROUTE {
        let (id, _, payload) = tokio::time::timeout(Duration::from_secs(10), pull(&mut o))
            .await
            .expect("no stall")
            .unwrap();
        assert_eq!(value(&payload), next[id], "route {id} in order");
        next[id] += 1;
    }
    for p in producers {
        p.await.unwrap();
    }
    assert!(o
        .routes()
        .iter()
        .all(|r| o.stats(r.id).unwrap().lagged == 0));
}

#[tokio::test]
async fn a_topic_provider_link_is_rejected() {
    struct Fixed;
    impl TopicProvider<V> for Fixed {
        fn topic(&self, _: &V) -> Option<String> {
            Some("x".into())
        }
    }
    let db = db(vec![Box::new(|reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_to("test://r0")
            .with_topic_provider(Fixed)
            .with_serializer(le)
            .finish();
    })])
    .await;
    let Err(aimdb_core::DbError::InvalidConfiguration { errors }) =
        OutboundRoutes::new(&db, "test")
    else {
        panic!("expected a configuration error");
    };
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].record_key, "r0");
    assert!(
        errors[0].message.contains("with_topic_writer"),
        "{}",
        errors[0].message
    );
}

#[tokio::test]
async fn route_info_matches_collect_outbound_routes() {
    let db = db(vec![
        Box::new(|reg| {
            reg.buffer(BufferCfg::SingleLatest);
        }),
        spmc(1, 16),
        Box::new(|reg| {
            reg.buffer(BufferCfg::Mailbox)
                .link_to("test://two?qos=1")
                .with_serializer(le)
                .finish();
        }),
    ])
    .await;
    let o = OutboundRoutes::new(&db, "test").unwrap();
    let old = db.collect_outbound_routes("test");
    assert_eq!(o.routes().len(), old.len());
    for (info, route) in o.routes().iter().zip(&old) {
        let config = aimdb_core::transport::ConnectorConfig::from_query(&route.config);
        assert!(info.config.record_index.is_some());
        assert_eq!(info.config.record_index, config.record_index);
        assert_eq!(&*info.default_topic, route.topic);
        assert_eq!(info.config.protocol_options, config.protocol_options);
    }
    assert_eq!(o.routes()[0].config.record_index, Some(1));
    assert_eq!(o.routes()[1].config.record_index, Some(2));
}

#[tokio::test]
async fn every_route_closes_when_the_database_is_dropped() {
    let db = db(vec![spmc(0, 16), plain(1, BufferCfg::SingleLatest)]).await;
    let mut o = OutboundRoutes::new(&db, "test").unwrap();
    produce(&db, 0, 1);
    drop(db);
    assert_eq!(
        pull(&mut o).await.map(|m| m.0),
        Some(0),
        "values before the close"
    );
    let end = tokio::time::timeout(Duration::from_secs(5), pull(&mut o))
        .await
        .expect("closes");
    assert!(end.is_none());
}
