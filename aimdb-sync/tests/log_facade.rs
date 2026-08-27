//! Design 050: this crate's facade events reach an installed `log` destination.
//!
//! Not a duplicate of `aimdb-core`'s coverage. The `log_*` macros are
//! `#[macro_export]`ed, so their `#[cfg(feature = "log")]` arm is resolved
//! against *this* crate's feature set — enabling `aimdb-core/log` alone leaves
//! these ten call sites silently unrouted. `aimdb-sync/Cargo.toml`'s
//! `log = ["aimdb-core/log"]` is what prevents that.
//!
//! Two of the three ways to break that mirror fail loudly on their own: a
//! feature that stops forwarding to `aimdb-core/log` makes the arm name a
//! re-export that is configured out (a compile error), and deleting the feature
//! makes `--features log` unknown to Cargo. This test covers the third — a
//! feature that is present and forwards, but whose events do not in fact
//! arrive — and is the reason `make test` names the combination at all.
#![cfg(all(feature = "std", feature = "log"))]

use std::sync::Arc;

use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
// Through the re-export, not a `log` dependency of this crate — the same path
// the macro arm uses, and the reason a facade user needs only the feature.
use aimdb_core::__private::log;
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reading {
    value: u32,
}

struct Capture {
    targets: std::sync::Mutex<Vec<String>>,
}

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        self.targets
            .lock()
            .unwrap()
            .push(record.target().to_string());
    }
    fn flush(&self) {}
}

static CAPTURE: Capture = Capture {
    targets: std::sync::Mutex::new(Vec::new()),
};

#[test]
fn aimdb_sync_events_reach_the_log_destination() {
    log::set_logger(&CAPTURE).expect("no other logger may be installed in this binary");
    log::set_max_level(log::LevelFilter::Trace);

    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    builder.configure::<Reading>("sensor.reading", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .tap(|_ctx, _consumer| async move {});
    });

    // Dropping without `detach()` is the cheapest facade call site in this
    // crate — two `log_warn!`s in `AimDbHandle::drop`, at `aimdb_sync::handle`.
    drop(builder.attach().expect("attach"));

    let targets = CAPTURE.targets.lock().unwrap().clone();
    assert!(
        targets.iter().any(|t| t.starts_with("aimdb_sync")),
        "no aimdb-sync event reached the log destination — is the `log` feature \
         still mirrored in aimdb-sync/Cargo.toml? got: {targets:?}"
    );
}
