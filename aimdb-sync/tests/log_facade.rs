//! Design 050: this crate's facade events reach an installed `log` destination.
//!
//! Not a duplicate of `aimdb-core`'s coverage — the macros expand here, so the
//! arm is selected by *this* crate's `log` feature. Breaking the mirror two of
//! three ways already fails loudly (a feature that stops forwarding names a
//! configured-out re-export; deleting it makes `--features log` unknown to
//! Cargo). This covers the third: present, forwarding, but not arriving.
#![cfg(all(feature = "std", feature = "log"))]

use std::sync::Arc;

use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
// Through the re-export, not a dependency of this crate — the same path the
// macro arm uses.
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

    // The cheapest facade call site here: two `log_warn!`s in `Drop`.
    drop(builder.attach().expect("attach"));

    let targets = CAPTURE.targets.lock().unwrap().clone();
    assert!(
        targets.iter().any(|t| t.starts_with("aimdb_sync")),
        "no aimdb-sync event reached the log destination — is the `log` feature \
         still mirrored in aimdb-sync/Cargo.toml? got: {targets:?}"
    );
}
