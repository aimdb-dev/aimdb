//! `ZenohGrammar` through core's router (`InboundDispatch`), with Zenoh's
//! delivery emulated: a sample reaches every declared subscriber whose key
//! expression intersects its key, and each delivery is one `dispatch` call
//! (053 §4.3, inbound).

#![cfg(feature = "std")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDb, AimDbBuilder, ConnectorBuilder, DbResult, InboundDispatch, KeyId};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::ZenohGrammar;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use zenoh_keyexpr::keyexpr;

#[derive(Clone, Debug)]
struct Sample;

/// Registers the `zenoh` scheme so `build()` accepts the links.
struct NoopZenoh;

impl ConnectorBuilder for NoopZenoh {
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
        "zenoh"
    }
}

/// What one link's deserializer saw: its key capture's value and key.
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    seen: Mutex<Vec<(String, Option<KeyId>)>>,
}

/// One record with an inbound link per pattern, keyed by `(capture, capacity)`.
async fn record(links: &[(&'static str, Option<(&'static str, u16)>)]) -> (AimDb, Vec<Arc<Probe>>) {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(NoopZenoh);
    let probes: Vec<Arc<Probe>> = links.iter().map(|_| Arc::default()).collect();
    let config: Vec<_> = links.iter().copied().zip(probes.clone()).collect();
    builder.configure::<Sample>("rec", move |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        for ((pattern, key), probe) in config {
            let mut link = reg.link_from(&format!("zenoh://{pattern}"));
            if let Some((capture, capacity)) = key {
                link = link.key(capture, capacity);
            }
            link.with_match_deserializer(move |_ctx, m, _bytes| {
                probe.calls.fetch_add(1, Ordering::SeqCst);
                let value = key.and_then(|(c, _)| m.get(c)).unwrap_or_default();
                probe.seen.lock().unwrap().push((value.into(), m.key()));
                Ok(Sample)
            })
            .finish();
        }
    });
    let (db, _runner) = builder.build().await.expect("build");
    (db, probes)
}

/// Delivers `key` once per intersecting subscriber, as Zenoh does; returns
/// the number of deliveries.
fn publish(inbound: &InboundDispatch, key: &str) -> usize {
    let key_expr = keyexpr::new(key).expect("valid key expression");
    let mut deliveries = 0;
    for subscription in inbound.subscriptions() {
        if keyexpr::new(&*subscription).unwrap().intersects(key_expr) {
            inbound.dispatch(key, b"");
            deliveries += 1;
        }
    }
    deliveries
}

fn calls(probes: &[Arc<Probe>]) -> Vec<usize> {
    probes
        .iter()
        .map(|p| p.calls.load(Ordering::SeqCst))
        .collect()
}

fn subscriptions(inbound: &InboundDispatch) -> Vec<String> {
    inbound
        .subscriptions()
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[tokio::test]
async fn covered_filters_deliver_once_per_route() {
    let (db, probes) = record(&[
        ("aimdb/*/state", None),
        ("aimdb/cell4/state", None),
        ("aimdb/{cell}/state", None),
    ])
    .await;
    let inbound = InboundDispatch::new(&db, "zenoh", &ZenohGrammar).unwrap();
    assert_eq!(subscriptions(&inbound), ["aimdb/*/state"]);
    assert_eq!(publish(&inbound, "aimdb/cell4/state"), 1);
    assert_eq!(calls(&probes), [1, 1, 1]);
}

/// `**` and `*/**` match the same keys; `**` covers `*/**`, so only it is
/// declared.
#[tokio::test]
async fn an_equivalent_filter_is_not_declared_twice() {
    let (db, probes) = record(&[("{all..}", None), ("{path..}/{leaf}", None)]).await;
    let inbound = InboundDispatch::new(&db, "zenoh", &ZenohGrammar).unwrap();
    assert_eq!(subscriptions(&inbound), ["**"]);
    assert_eq!(publish(&inbound, "plant/cell4/temp"), 1);
    assert_eq!(calls(&probes), [1, 1]);
}

/// `x$*` covers `x$*y`; were both declared, each route would ingest a
/// sample matching both twice.
#[tokio::test]
async fn sub_chunk_filters_deliver_once_per_route() {
    let (db, probes) = record(&[("dev/x$*", None), ("dev/x$*y", None)]).await;
    let inbound = InboundDispatch::new(&db, "zenoh", &ZenohGrammar).unwrap();
    assert_eq!(subscriptions(&inbound), ["dev/x$*"]);
    assert_eq!(publish(&inbound, "dev/xzy"), 1);
    assert_eq!(calls(&probes), [1, 1]);
}

/// 055 §5.7's known difference: Zenoh delivers a sample matching two partly
/// overlapping filters to both subscribers, so each route sees it twice.
#[tokio::test]
async fn partly_overlapping_filters_deliver_per_subscriber() {
    let (db, probes) = record(&[("a/*/c", None), ("a/b/*", None)]).await;
    let inbound = InboundDispatch::new(&db, "zenoh", &ZenohGrammar).unwrap();
    assert_eq!(subscriptions(&inbound), ["a/*/c", "a/b/*"]);
    assert_eq!(publish(&inbound, "a/b/c"), 2);
    assert_eq!(calls(&probes), [2, 2]);
}

/// One `KeyId` per publisher, and a `put` on a wildcard key expression,
/// which Zenoh delivers with the wildcard as its key, takes none.
#[tokio::test]
async fn keyed_pattern_assigns_one_key_per_publisher() {
    let (db, probes) = record(&[("aimdb/{cell}/state", Some(("cell", 2)))]).await;
    let inbound = InboundDispatch::new(&db, "zenoh", &ZenohGrammar).unwrap();

    assert_eq!(publish(&inbound, "aimdb/*/state"), 1);
    assert_eq!(publish(&inbound, "aimdb/c$*/state"), 1);
    assert_eq!(calls(&probes), [0], "wildcard keys reach no route");

    for key in [
        "aimdb/cell4/state",
        "aimdb/cell5/state",
        "aimdb/cell4/state",
    ] {
        publish(&inbound, key);
    }
    let seen = probes[0].seen.lock().unwrap();
    let values: Vec<&str> = seen.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(values, ["cell4", "cell5", "cell4"]);
    assert!(seen.iter().all(|(_, k)| k.is_some()));
    assert_ne!(seen[0].1, seen[1].1);
    assert_eq!(seen[0].1, seen[2].1);
    assert_eq!(
        db.inbound_key_name("rec", seen[1].1.unwrap()).as_deref(),
        Some("cell5")
    );
}
