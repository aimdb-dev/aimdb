//! A database the child attaches *after* forking is its own and must work.
//!
//! This is why the guard is a generation counter and not a poison flag: a
//! supervisor that forks per job and then does its own work would otherwise
//! find the API dead for no reason.
//!
//! # Why this has a binary to itself
//!
//! Its child allocates and spawns a thread — proving `attach` works cannot
//! avoid that — so it is the most exposed of the fork tests to a lock inherited
//! from a thread that did not survive. It deadlocked on the first CI run and
//! hung until the six-hour job ceiling killed it.
//!
//! What makes it safe here is not the separate binary as such: measurement
//! showed harness parallelism has no bearing on the hang rate (see the table in
//! `fork_child`). It is that this test attaches **nothing before forking**, so
//! the parent has no runtime thread that could be mid-allocation when the fork
//! happens — every other fork test must attach first in order to have something
//! to inherit. Its own binary is what keeps it that way: one `attach` anywhere
//! else in the process would undo it. The watchdog in `fork_child` bounds what
//! is left.
#![cfg(all(unix, feature = "std"))]
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

mod fork_child;
use fork_child::in_forked_child;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reading {
    value: u32,
}

#[test]
fn a_child_can_attach_its_own_database_after_forking() {
    let code = in_forked_child(|| {
        // Built inside the child on purpose: nothing is attached in the parent,
        // so no runtime thread exists to be missing from this address space.
        let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
        builder.configure::<Reading>("sensor.reading", |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
                .tap(|_ctx, _consumer| async move {});
        });
        let handle = match builder.attach() {
            Ok(h) => h,
            Err(_) => return false,
        };
        let producer = match handle.producer::<Reading>("sensor.reading") {
            Ok(p) => p,
            Err(_) => return false,
        };
        let published = producer.set(Reading { value: 7 }).is_ok();
        let detached = handle.detach().is_ok();
        published && detached
    });
    assert_eq!(
        code, 0,
        "a post-fork attach belongs to the child and must work"
    );
}
