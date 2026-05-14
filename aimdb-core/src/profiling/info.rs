//! Serializable snapshot of stage profiling metrics, for remote introspection.

extern crate alloc;
use alloc::{string::String, vec::Vec};

use serde::{Deserialize, Serialize};

use crate::profiling::{RecordProfilingMetrics, StageEntry};

/// A point-in-time snapshot of one execution stage's timing metrics.
///
/// Carried in `RecordMetadata::stage_profiling` and exposed by the
/// `get_stage_profiling` MCP tool. All times are wall-clock nanoseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageProfilingInfo {
    /// Stage kind: `"source"`, `"tap"`, `"link"`, or `"transform"`.
    pub stage_type: String,
    /// Registration index within the stage kind (0-based).
    pub index: usize,
    /// Name assigned via `.with_name("...")`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Number of recorded invocations.
    pub call_count: u64,
    /// Cumulative wall-clock time, nanoseconds.
    pub total_time_ns: u64,
    /// Mean wall-clock time per invocation, nanoseconds.
    pub avg_time_ns: u64,
    /// Fastest recorded invocation, nanoseconds.
    pub min_time_ns: u64,
    /// Slowest recorded invocation, nanoseconds.
    pub max_time_ns: u64,
}

impl StageProfilingInfo {
    fn from_entry(stage_type: &str, index: usize, entry: &StageEntry) -> Self {
        let m = &entry.metrics;
        Self {
            stage_type: String::from(stage_type),
            index,
            name: entry.name.clone(),
            call_count: m.call_count(),
            total_time_ns: m.total_time_ns(),
            avg_time_ns: m.avg_time_ns(),
            min_time_ns: m.min_time_ns(),
            max_time_ns: m.max_time_ns(),
        }
    }
}

impl RecordProfilingMetrics {
    /// Returns a serializable snapshot of every registered stage's metrics,
    /// ordered sources → taps → links.
    pub fn snapshot(&self) -> Vec<StageProfilingInfo> {
        let mut out = Vec::new();
        for (kind, stages) in [
            ("source", self.sources()),
            ("tap", self.taps()),
            ("link", self.links()),
        ] {
            for (i, entry) in stages.iter().enumerate() {
                out.push(StageProfilingInfo::from_entry(kind, i, entry));
            }
        }
        out
    }
}
