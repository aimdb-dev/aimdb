//! Shared behavioral contract for [`RuntimeOps`](crate::RuntimeOps) implementations.
//!
//! Each adapter crate calls [`assert_runtime_ops_contract`] from a test running
//! under its own executor (`#[tokio::test]`, `block_on`, `#[wasm_bindgen_test]`),
//! mirroring how the join-queue behavioral tests are duplicated per adapter.

use crate::{LogLevel, RuntimeOps};

/// Asserts the behavioral contract every `RuntimeOps` implementation must hold.
///
/// Uses `Duration::ZERO` for the sleep check: the Embassy host-test time
/// driver reports `now() == 0` with a no-op `schedule_wake`, so any non-zero
/// sleep would hang forever on the host. Adapters with a real clock should
/// additionally assert that a non-zero sleep advances `now_nanos` in their
/// own tests.
pub async fn assert_runtime_ops_contract(ops: &dyn RuntimeOps) {
    assert!(!ops.name().is_empty(), "name() must be non-empty");

    let t0 = ops.now_nanos();
    let t1 = ops.now_nanos();
    assert!(t1 >= t0, "now_nanos() must be monotonic ({t1} < {t0})");

    ops.log(LogLevel::Debug, "runtime-ops contract: debug");
    ops.log(LogLevel::Info, "runtime-ops contract: info");
    ops.log(LogLevel::Warn, "runtime-ops contract: warn");
    ops.log(LogLevel::Error, "runtime-ops contract: error");

    ops.sleep(core::time::Duration::ZERO).await;

    if let Some((secs, nanos)) = ops.unix_time() {
        assert!(nanos < 1_000_000_000, "unix_time nanos out of range: {nanos}");
        // Sanity: any real wall clock reads after 2020-09-13 (1.6e9).
        assert!(secs > 1_600_000_000, "unix_time seconds implausible: {secs}");
    }
}
