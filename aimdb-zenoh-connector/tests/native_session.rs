//! The `zenoh` crate backend against a real Zenoh router, in-process: the
//! connector is a client of the router, and a plain `zenoh` session is the
//! remote side.

#![cfg(feature = "std")]

use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::builder::AimDbRunner;
use aimdb_core::{AimDb, AimDbBuilder, KeyId};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::ZenohConnector;
use zenoh::Session;

const WAIT: Duration = Duration::from_secs(20);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A session config that only talks to `endpoint`: no scouting, so tests
/// running side by side cannot find each other's routers.
fn config(mode: &str, endpoint: &str, listen: bool) -> zenoh::Config {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", &format!(r#""{mode}""#))
        .unwrap();
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .unwrap();
    let field = if listen {
        "listen/endpoints"
    } else {
        "connect/endpoints"
    };
    config
        .insert_json5(field, &format!(r#"["{endpoint}"]"#))
        .unwrap();
    config
}

async fn router(endpoint: &str) -> Session {
    zenoh::open(config("router", endpoint, true)).await.unwrap()
}

async fn client(endpoint: &str) -> Session {
    zenoh::open(config("client", endpoint, false))
        .await
        .unwrap()
}

/// The connector under test, as a client of `endpoint` without scouting.
fn connector(endpoint: &str) -> ZenohConnector {
    ZenohConnector::new(endpoint).with_zenoh_config(config("client", endpoint, false))
}

async fn build(
    endpoint: &str,
    configure: impl FnOnce(&mut AimDbBuilder) + Send,
) -> (AimDb, AimDbRunner) {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(connector(endpoint));
    configure(&mut builder);
    builder.build().await.expect("build")
}

/// Polls `done` until it holds or `WAIT` passes.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn le(
    _ctx: aimdb_core::RuntimeContext,
    v: &u32,
) -> Result<Vec<u8>, aimdb_core::connector::SerializeError> {
    Ok(v.to_le_bytes().to_vec())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbound_values_reach_a_zenoh_subscriber() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = router(&endpoint).await;
    let remote = client(&endpoint).await;
    let fixed = remote.declare_subscriber("aimdb/test/out").await.unwrap();
    let written = remote.declare_subscriber("aimdb/test/dyn/*").await.unwrap();

    let (db, runner) = build(&endpoint, |b| {
        b.configure::<u32>("out", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
                .link_to("zenoh://aimdb/test/out")
                .with_serializer(le)
                .finish();
        });
        b.configure::<u32>("dyn", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
                .link_to("zenoh://aimdb/test/dyn/default")
                .with_topic_fn(32, |v: &u32, out| {
                    use core::fmt::Write;
                    write!(out, "aimdb/test/dyn/{v}")?;
                    Ok(true)
                })
                .with_serializer(le)
                .finish();
        });
    })
    .await;
    tokio::spawn(runner.run());

    let deadline = tokio::time::Instant::now() + WAIT;
    let mut value = 0u32;
    let sample = loop {
        db.produce("out", value).unwrap();
        value += 1;
        if let Ok(Ok(sample)) =
            tokio::time::timeout(Duration::from_millis(200), fixed.recv_async()).await
        {
            break sample;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no fixed-key sample"
        );
    };
    assert_eq!(sample.key_expr().as_str(), "aimdb/test/out");
    assert_eq!(sample.payload().to_bytes().len(), 4);

    db.produce("dyn", 7u32).unwrap();
    let sample = tokio::time::timeout(WAIT, written.recv_async())
        .await
        .expect("written-key sample")
        .unwrap();
    assert_eq!(sample.key_expr().as_str(), "aimdb/test/dyn/7");
    assert_eq!(&*sample.payload().to_bytes(), &7u32.to_le_bytes());
}

/// What one inbound link's deserializer saw: capture value and key.
#[derive(Default)]
struct Probe {
    seen: Mutex<Vec<(String, Option<KeyId>)>>,
    calls: AtomicUsize,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbound_patterns_capture_and_key_each_publisher() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = router(&endpoint).await;
    let remote = client(&endpoint).await;
    let probe = Arc::new(Probe::default());

    let p = probe.clone();
    let (_db, runner) = build(&endpoint, move |b| {
        b.configure::<u32>("cells", move |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
                .link_from("zenoh://aimdb/{cell}/state")
                .key("cell", 8)
                .with_match_deserializer(move |_ctx, m, _bytes| {
                    p.calls.fetch_add(1, Ordering::SeqCst);
                    let cell = m.get("cell").unwrap_or_default().to_string();
                    p.seen.lock().unwrap().push((cell, m.key()));
                    Ok(0)
                })
                .finish();
        });
    })
    .await;
    tokio::spawn(runner.run());

    // Repeat until the connector's subscriber is in place.
    let seen = |cell: &str| probe.seen.lock().unwrap().iter().any(|(c, _)| c == cell);
    let deadline = tokio::time::Instant::now() + WAIT;
    while !seen("cell4") {
        remote.put("aimdb/cell4/state", "x").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(tokio::time::Instant::now() < deadline, "no cell4 sample");
    }
    remote.put("aimdb/*/state", "wildcard").await.unwrap();
    remote.put("aimdb/cell5/state", "x").await.unwrap();
    eventually("cell5", || seen("cell5")).await;

    let seen = probe.seen.lock().unwrap();
    let key = |cell: &str| seen.iter().find(|(c, _)| c == cell).and_then(|(_, k)| *k);
    assert!(key("cell4").is_some() && key("cell5").is_some());
    assert_ne!(key("cell4"), key("cell5"), "one KeyId per publisher");
    assert!(
        seen.iter().all(|(c, _)| c != "*"),
        "a wildcard key is not a publisher"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_route_ingests_a_sample_once() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = router(&endpoint).await;
    let remote = client(&endpoint).await;
    // One counter per link: covered (`aimdb/cell4/x` under `aimdb/*/x`) and
    // partly overlapping (`a/*/c` and `a/b/*`) filters.
    let counters: Vec<Arc<AtomicUsize>> = (0..4).map(|_| Arc::default()).collect();
    let c = counters.clone();
    let (_db, runner) = build(&endpoint, move |b| {
        b.configure::<u32>("covered", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
            for (url, counter) in [
                ("zenoh://aimdb/cell4/x", &c[0]),
                ("zenoh://aimdb/*/x", &c[1]),
            ] {
                let counter = counter.clone();
                reg.link_from(url)
                    .with_deserializer(move |_ctx, _bytes: &[u8]| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(0)
                    })
                    .finish();
            }
        });
        b.configure::<u32>("overlap", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
            for (url, counter) in [("zenoh://a/*/c", &c[2]), ("zenoh://a/b/*", &c[3])] {
                let counter = counter.clone();
                reg.link_from(url)
                    .with_deserializer(move |_ctx, _bytes: &[u8]| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(0)
                    })
                    .finish();
            }
        });
    })
    .await;
    tokio::spawn(runner.run());

    // Wait for the subscribers with a probe key only `aimdb/*/x` matches.
    let deadline = tokio::time::Instant::now() + WAIT;
    while counters[1].load(Ordering::SeqCst) == 0 {
        remote.put("aimdb/probe/x", "x").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(tokio::time::Instant::now() < deadline, "no probe sample");
    }
    let before: Vec<usize> = counters.iter().map(|c| c.load(Ordering::SeqCst)).collect();
    remote.put("aimdb/cell4/x", "x").await.unwrap();
    remote.put("a/b/c", "x").await.unwrap();
    eventually("both samples", || {
        counters[0].load(Ordering::SeqCst) > before[0]
            && counters[2].load(Ordering::SeqCst) > before[2]
    })
    .await;
    // Let any duplicate arrive before counting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let delta: Vec<usize> = counters
        .iter()
        .zip(&before)
        .map(|(c, b)| c.load(Ordering::SeqCst) - b)
        .collect();
    assert_eq!(delta, [1, 1, 1, 1], "once per route");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_outbound_wildcard_key_fails_the_build() {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(connector("tcp/127.0.0.1:1"));
    builder.configure::<u32>("out", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_to("zenoh://aimdb/*/out")
            .with_serializer(le)
            .finish();
    });
    let error = builder.build().await.err().expect("refused");
    assert!(error.to_string().contains("wildcard"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_connector_waits_for_a_router_that_starts_later() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let probe = Arc::new(Probe::default());
    let p = probe.clone();
    let (_db, runner) = build(&endpoint, move |b| {
        b.configure::<u32>("late", move |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from("zenoh://aimdb/late")
                .with_deserializer(move |_ctx, _bytes: &[u8]| {
                    p.calls.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                })
                .finish();
        });
    })
    .await;
    tokio::spawn(runner.run());

    tokio::time::sleep(Duration::from_secs(1)).await;
    let _router = router(&endpoint).await;
    let remote = client(&endpoint).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while probe.calls.load(Ordering::SeqCst) == 0 {
        remote.put("aimdb/late", "x").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "no sample after the router started"
        );
    }
}

/// A record linked to and from one key ingests no publication of its own:
/// without remote-only locality, one value looped through the session
/// hundreds of thousands of times a second.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_linked_both_ways_does_not_ingest_its_own_puts() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = router(&endpoint).await;
    let remote = client(&endpoint).await;
    let echoes = remote.declare_subscriber("aimdb/mirror").await.unwrap();
    let ingested = Arc::new(AtomicUsize::new(0));
    let i = ingested.clone();
    let (db, runner) = build(&endpoint, move |b| {
        b.configure::<u32>("mirror", move |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
                .link_to("zenoh://aimdb/mirror")
                .with_serializer(le)
                .finish()
                .link_from("zenoh://aimdb/mirror")
                .with_deserializer(move |_ctx, bytes: &[u8]| {
                    i.fetch_add(1, Ordering::SeqCst);
                    Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| "4 bytes")?))
                })
                .finish();
        });
    })
    .await;
    tokio::spawn(runner.run());

    // Local values reach the remote side and never come back in.
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut produced = 0u32;
    loop {
        db.produce("mirror", produced).unwrap();
        produced += 1;
        if let Ok(Ok(_)) =
            tokio::time::timeout(Duration::from_millis(200), echoes.recv_async()).await
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no outbound sample");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(ingested.load(Ordering::SeqCst), 0, "own puts ingested");
    while echoes.try_recv().ok().flatten().is_some() {}

    // A remote value is ingested once, published back out once, and stops.
    remote
        .put("aimdb/mirror", 42u32.to_le_bytes().to_vec())
        .await
        .unwrap();
    eventually("remote value ingested", || {
        ingested.load(Ordering::SeqCst) == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        ingested.load(Ordering::SeqCst),
        1,
        "re-ingested its own echo"
    );
    let mut echoed = 0;
    while echoes.try_recv().ok().flatten().is_some() {
        echoed += 1;
    }
    // The remote put itself, plus the record's one echo of it.
    assert!(echoed <= 2, "{echoed} samples after one remote put");
}

/// A remote `delete()` is not a value: it never reaches the deserializer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remote_delete_is_not_ingested() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = router(&endpoint).await;
    let remote = client(&endpoint).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let (_db, runner) = build(&endpoint, move |b| {
        b.configure::<u32>("deleted", move |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from("zenoh://aimdb/deleted")
                .with_deserializer(move |_ctx, _bytes: &[u8]| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                })
                .finish();
        });
    })
    .await;
    tokio::spawn(runner.run());

    let deadline = tokio::time::Instant::now() + WAIT;
    while calls.load(Ordering::SeqCst) == 0 {
        remote.put("aimdb/deleted", "x").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "subscriber never ready"
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = calls.load(Ordering::SeqCst);
    remote.delete("aimdb/deleted").await.unwrap();
    remote.put("aimdb/deleted", "after").await.unwrap();
    eventually("the put after the delete", || {
        calls.load(Ordering::SeqCst) > before
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before + 1,
        "the delete reached the deserializer"
    );
}
