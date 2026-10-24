//! `TokioAdapter::now_nanos` under paused Tokio time.
//!
//! Under `start_paused = true`, `tokio::time::sleep` advances the virtual
//! clock instantly instead of sleeping in real time. `now_nanos` must follow
//! that same clock so time-based logic measured through `RuntimeOps` sees the
//! sleep's duration, not whatever real time elapsed while the test ran.

use aimdb_core::RuntimeOps;
use aimdb_tokio_adapter::TokioAdapter;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn now_nanos_advances_with_paused_sleep() {
    let rt = TokioAdapter::new().unwrap();

    let before = rt.now_nanos();
    tokio::time::sleep(Duration::from_secs(10)).await;
    let after = rt.now_nanos();

    let advanced = after - before;
    assert!(
        advanced >= Duration::from_secs(10).as_nanos() as u64,
        "now_nanos advanced by only {advanced}ns across a paused 10s sleep"
    );
}
