//! Build-time validation for `.with_remote_access()` records.
//!
//! Removing `latest_snapshot` means a remote-access record reads and writes
//! straight through its buffer. With no buffer there is no storage to serve:
//! `record.get`/`latest()` return not_found and `record.set` silently discards.
//! `build()` must reject this loudly instead of letting it surface as a silent
//! runtime no-op.

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Config {
    threshold: u32,
}

#[tokio::test]
async fn bufferless_remote_access_record_fails_build() {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));

    builder.configure::<Config>("test::Config", |reg| {
        // .with_remote_access() but no .buffer(...) — invalid: remote reads
        // and writes go straight to the buffer.
        reg.with_remote_access();
    });

    assert!(
        builder.build().await.is_err(),
        "build() must reject a remote-access record with no buffer"
    );
}

#[tokio::test]
async fn remote_access_record_with_buffer_builds() {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));

    builder.configure::<Config>("test::Config", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });

    assert!(
        builder.build().await.is_ok(),
        "remote-access record with a buffer must build"
    );
}

// ── Raw buffers (`buffer_raw`) ───────────────────────────────────────────────
//
// A raw buffer hides its kind. Core keeps a ring's last value only for a buffer
// declared as `SpmcRing`, and `record.get` no longer drains a cursor to find
// one, so the kind must be declared (#444, #450).

fn raw_ring() -> Box<dyn aimdb_core::buffer::DynBuffer<aimdb_core::Stamped<Config>>> {
    use aimdb_core::buffer::Buffer;
    Box::new(aimdb_tokio_adapter::TokioBuffer::<
        aimdb_core::Stamped<Config>,
    >::new(&BufferCfg::SpmcRing { capacity: 8 }))
}

#[tokio::test]
async fn undeclared_raw_buffer_with_remote_access_fails_build() {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));

    builder.configure::<Config>("test::Config", |reg| {
        reg.buffer_raw(raw_ring()).with_remote_access();
    });

    let err = match builder.build().await {
        Ok(_) => panic!("build() must reject an undeclared raw buffer on a remote-access record"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("buffer_cfg"), "the error names the fix: {err}");
}

#[tokio::test]
async fn declared_raw_ring_returns_its_last_write() {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));

    builder.configure::<Config>("test::Config", |reg| {
        reg.buffer_raw(raw_ring())
            .buffer_cfg(BufferCfg::SpmcRing { capacity: 8 })
            .with_remote_access();
    });
    let (db, _runner) = builder.build().await.expect("a declared raw ring builds");

    // Nothing written yet.
    assert_eq!(db.try_latest_as_json("test::Config"), None);

    let producer = db.producer::<Config>("test::Config").expect("producer");
    producer.produce(Config { threshold: 1 });
    producer.produce(Config { threshold: 2 });

    // What `record.get` serves: the last write, readable again and again.
    for _ in 0..2 {
        assert_eq!(
            db.try_latest_as_json("test::Config").map(|s| s.value),
            Some(serde_json::json!({ "threshold": 2 }))
        );
    }
    assert_eq!(
        db.latest::<Config>("test::Config")
            .unwrap()
            .map(|s| s.value),
        Some(Config { threshold: 2 })
    );
}

#[tokio::test]
async fn undeclared_raw_buffer_without_remote_access_still_builds() {
    // Only remote-access records need the declaration: `latest()` on an
    // undeclared raw ring returns `None`, as it did before.
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));

    builder.configure::<Config>("test::Config", |reg| {
        reg.buffer_raw(raw_ring());
    });
    let (db, _runner) = builder.build().await.expect("builds");
    db.producer::<Config>("test::Config")
        .unwrap()
        .produce(Config { threshold: 1 });
    assert_eq!(db.latest::<Config>("test::Config").unwrap(), None);
}
