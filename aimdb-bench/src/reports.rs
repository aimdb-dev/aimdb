//! Result structs for B0 benchmark output.
//!
//! Serialized as JSON for storage in `data/baselines/`.  B1/B2 results are
//! managed by Criterion's built-in baseline system (`target/criterion/`).

use serde::{Deserialize, Serialize};

/// B0 allocation report for a single workload profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocReport {
    /// Profile name (e.g. "Telemetry", "State", "Command").
    pub profile: String,
    /// Buffer type (e.g. "SpmcRing", "SingleLatest", "Mailbox").
    pub buffer_type: String,
    /// Total allocations in the measured batch.
    pub total_allocs: u64,
    /// Total bytes allocated in the measured batch.
    pub total_bytes: u64,
    /// Number of messages in the batch.
    pub batch_size: usize,
    /// Mean allocations per message.
    pub allocs_per_msg: f64,
    /// Mean bytes allocated per message.
    pub bytes_per_msg: f64,
}

impl AllocReport {
    /// Construct from raw counter snapshot and batch metadata.
    pub fn new(
        profile: impl Into<String>,
        buffer_type: impl Into<String>,
        batch_size: usize,
        total_allocs: u64,
        total_bytes: u64,
    ) -> Self {
        let n = batch_size as f64;
        Self {
            profile: profile.into(),
            buffer_type: buffer_type.into(),
            total_allocs,
            total_bytes,
            batch_size,
            allocs_per_msg: total_allocs as f64 / n,
            bytes_per_msg: total_bytes as f64 / n,
        }
    }

    /// Print a human-readable one-liner to stdout.
    pub fn print(&self) {
        println!(
            "[B0] {:12} ({:12}): {:.3} allocs/msg  ({} total allocs, {} B/msg avg, {} B total, batch={})",
            self.profile,
            self.buffer_type,
            self.allocs_per_msg,
            self.total_allocs,
            self.bytes_per_msg as u64,
            self.total_bytes,
            self.batch_size,
        );
    }
}

/// Serialize `reports` as pretty JSON and write to
/// `aimdb-bench/target/bench-results/<name>.json` (anchored to the crate
/// dir regardless of the caller's working directory) — collapses the
/// `to_string_pretty` + `create_dir_all` + `write` tail that used to be
/// hand-rolled at the end of every B0 bench binary.
///
/// # Panics
/// Panics if serialization or the filesystem write fails — a bench binary
/// with unwritable results is a setup bug worth failing loudly on, not
/// silently continuing past.
pub fn write_reports(name: &str, reports: &[AllocReport]) {
    let json = serde_json::to_string_pretty(reports).expect("failed to serialize reports");
    let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target/bench-results");
    std::fs::create_dir_all(out_dir).expect("failed to create results directory");
    let out_path = format!("{out_dir}/{name}.json");
    std::fs::write(&out_path, &json).expect("failed to write results");
    println!("\nResults written to {out_path}");
}
