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
