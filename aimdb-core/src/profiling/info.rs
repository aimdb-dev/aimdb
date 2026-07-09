//! Serializable snapshot of stage profiling metrics, for remote introspection.

use alloc::{string::String, vec::Vec};

use serde::{Deserialize, Serialize};

use crate::profiling::{RecordProfilingMetrics, SignalGauge, StageEntry};

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

/// A point-in-time snapshot of one record's signal gauge (`.observe()`).
///
/// Carried in `RecordMetadata::signal_stats` so a record's live domain signal
/// (last/min/max/mean) surfaces on `record.list`/`record.get` and the MCP tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalStatsInfo {
    /// Signal label (`Observable::SIGNAL`, defaults to the schema name).
    pub signal: String,
    /// Unit label (`Observable::UNIT`), omitted when empty.
    ///
    /// `default` matches the skip: `Observable::UNIT` defaults to `""`, so a
    /// snapshot without a unit must deserialize (aimdb-client/aimdb-mcp read
    /// this back from `record.list`/`record.get`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit: String,
    /// Number of samples observed.
    pub count: u64,
    /// Most recent sample.
    pub last: f64,
    /// Smallest sample observed.
    pub min: f64,
    /// Largest sample observed.
    pub max: f64,
    /// Mean of all samples.
    pub mean: f64,
}

impl SignalStatsInfo {
    fn from_gauge(gauge: &SignalGauge) -> Self {
        let s = &gauge.stats;
        Self {
            signal: gauge.name.clone(),
            unit: gauge.unit.clone(),
            count: s.count(),
            last: s.last(),
            min: s.min(),
            max: s.max(),
            mean: s.mean(),
        }
    }
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
            ("transform", self.transforms()),
        ] {
            for (i, entry) in stages.iter().enumerate() {
                out.push(StageProfilingInfo::from_entry(kind, i, entry));
            }
        }
        out
    }

    /// Returns a serializable snapshot of every registered signal gauge.
    pub fn signal_snapshot(&self) -> Vec<SignalStatsInfo> {
        self.signals()
            .iter()
            .map(SignalStatsInfo::from_gauge)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    /// `unit` is skipped when empty (`Observable::UNIT` defaults to `""`), so
    /// the emitted JSON must deserialize back without it — aimdb-client and
    /// aimdb-mcp read these snapshots from `record.list`/`record.get`.
    #[test]
    fn empty_unit_roundtrips() {
        let info = SignalStatsInfo {
            signal: "temperature".to_string(),
            unit: String::new(),
            count: 3,
            last: 1.0,
            min: 0.5,
            max: 2.0,
            mean: 1.2,
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("unit"), "empty unit must be omitted: {json}");

        let back: SignalStatsInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }
}
