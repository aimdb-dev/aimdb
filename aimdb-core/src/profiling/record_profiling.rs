//! Per-record container of stage profiling metrics.

use alloc::{string::String, sync::Arc, vec::Vec};

use crate::profiling::StageMetrics;
use crate::StageKind;

/// One registered stage: its shared metrics plus an optional human-readable name
/// set via `.with_name("...")`.
#[derive(Debug)]
pub struct StageEntry {
    /// Shared timing counters for this stage.
    pub metrics: Arc<StageMetrics>,
    /// Name assigned via `RecordRegistrar::with_name`, if any.
    pub name: Option<String>,
}

impl StageEntry {
    fn new() -> Self {
        Self {
            metrics: Arc::new(StageMetrics::new()),
            name: None,
        }
    }
}

/// All stage profiling metrics for a single record, indexed by registration order
/// within each stage kind (`sources[0]` is the first `.source()`, `taps[1]` the
/// second `.tap()`, etc.).
#[derive(Debug, Default)]
pub struct RecordProfilingMetrics {
    sources: Vec<StageEntry>,
    taps: Vec<StageEntry>,
    links: Vec<StageEntry>,
    transforms: Vec<StageEntry>,
}

impl RecordProfilingMetrics {
    /// Creates an empty container.
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if no stages have been registered.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
            && self.taps.is_empty()
            && self.links.is_empty()
            && self.transforms.is_empty()
    }

    /// Registers a new source stage; returns its index and shared metrics handle.
    pub fn push_source(&mut self) -> (usize, Arc<StageMetrics>) {
        Self::push(&mut self.sources)
    }

    /// Registers a new tap stage; returns its index and shared metrics handle.
    pub fn push_tap(&mut self) -> (usize, Arc<StageMetrics>) {
        Self::push(&mut self.taps)
    }

    /// Registers a new link stage; returns its index and shared metrics handle.
    pub fn push_link(&mut self) -> (usize, Arc<StageMetrics>) {
        Self::push(&mut self.links)
    }

    pub fn push_transform(&mut self) -> (usize, Arc<StageMetrics>) {
        Self::push(&mut self.transforms)
    }

    fn push(vec: &mut Vec<StageEntry>) -> (usize, Arc<StageMetrics>) {
        let idx = vec.len();
        let entry = StageEntry::new();
        let metrics = entry.metrics.clone();
        vec.push(entry);
        (idx, metrics)
    }

    /// The source stage at `idx`, if registered.
    pub fn source(&self, idx: usize) -> Option<&StageEntry> {
        self.sources.get(idx)
    }

    /// The tap stage at `idx`, if registered.
    pub fn tap(&self, idx: usize) -> Option<&StageEntry> {
        self.taps.get(idx)
    }

    /// The link stage at `idx`, if registered.
    pub fn link(&self, idx: usize) -> Option<&StageEntry> {
        self.links.get(idx)
    }

    /// The transform stage at `idx`, if registered.
    pub fn transform(&self, idx: usize) -> Option<&StageEntry> {
        self.transforms.get(idx)
    }

    /// Number of registered tap stages.
    pub fn tap_count(&self) -> usize {
        self.taps.len()
    }

    /// All source stages, in registration order.
    pub fn sources(&self) -> &[StageEntry] {
        &self.sources
    }

    /// All tap stages, in registration order.
    pub fn taps(&self) -> &[StageEntry] {
        &self.taps
    }

    /// All link stages, in registration order.
    pub fn links(&self) -> &[StageEntry] {
        &self.links
    }

    pub fn transforms(&self) -> &[StageEntry] {
        &self.transforms
    }

    /// Assigns a name to a previously registered stage. No-op if `idx` is out of range.
    pub fn set_stage_name(&mut self, kind: StageKind, idx: usize, name: &str) {
        let vec = match kind {
            StageKind::Source => &mut self.sources,
            StageKind::Tap => &mut self.taps,
            StageKind::Link => &mut self.links,
            StageKind::Transform => &mut self.transforms,
        };
        if let Some(entry) = vec.get_mut(idx) {
            entry.name = Some(String::from(name));
        }
    }

    /// Resets every stage's counters.
    pub fn reset_all(&self) {
        for e in self
            .sources
            .iter()
            .chain(&self.taps)
            .chain(&self.links)
            .chain(&self.transforms)
        {
            e.metrics.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_assigns_sequential_indices() {
        let mut m = RecordProfilingMetrics::new();
        assert!(m.is_empty());
        let (i0, _) = m.push_tap();
        let (i1, _) = m.push_tap();
        assert_eq!((i0, i1), (0, 1));
        assert_eq!(m.tap_count(), 2);
        assert!(!m.is_empty());
        assert!(m.tap(0).is_some());
        assert!(m.tap(2).is_none());
    }

    #[test]
    fn set_stage_name() {
        let mut m = RecordProfilingMetrics::new();
        let _ = m.push_source();
        m.set_stage_name(StageKind::Source, 0, "sensor_reader");
        assert_eq!(m.source(0).unwrap().name.as_deref(), Some("sensor_reader"));
        // out of range is a no-op
        m.set_stage_name(StageKind::Source, 5, "ignored");
    }

    #[test]
    fn reset_all_clears_metrics() {
        let mut m = RecordProfilingMetrics::new();
        let (_, src) = m.push_source();
        let (_, tap) = m.push_tap();
        src.record(10);
        tap.record(20);
        m.reset_all();
        assert_eq!(src.call_count(), 0);
        assert_eq!(tap.call_count(), 0);
    }

    #[test]
    fn push_transform_records_metrics_and_reports_call_count() {
        let mut m = RecordProfilingMetrics::new();
        let (idx, metrics) = m.push_transform();
        assert_eq!(idx, 0);
        for v in [10u64, 30, 20] {
            metrics.record(v);
        }
        let entry = m.transform(0).expect("transform stage registered");
        assert_eq!(entry.metrics.call_count(), 3);
        assert_eq!(entry.metrics.total_time_ns(), 60);
        assert_eq!(entry.metrics.min_time_ns(), 10);
        assert_eq!(entry.metrics.max_time_ns(), 30);
        assert_eq!(m.transforms().len(), 1);
    }

    #[test]
    fn set_stage_name_transform_lands_on_correct_index() {
        let mut m = RecordProfilingMetrics::new();
        // Interleave with other stage kinds so index namespaces don't collide.
        let _ = m.push_source();
        let _ = m.push_transform();
        let _ = m.push_transform();
        m.set_stage_name(StageKind::Transform, 1, "second_transform");

        assert_eq!(m.transform(0).unwrap().name, None);
        assert_eq!(
            m.transform(1).unwrap().name.as_deref(),
            Some("second_transform")
        );
        // Naming a transform must not leak into the source namespace.
        assert_eq!(m.source(0).unwrap().name, None);
    }

    #[test]
    fn adjacent_record_profiling_metrics_do_not_cross_talk() {
        let mut a = RecordProfilingMetrics::new();
        let mut b = RecordProfilingMetrics::new();
        let (_, metrics_a) = a.push_transform();
        let (_, metrics_b) = b.push_transform();

        metrics_a.record(100);

        assert_eq!(a.transform(0).unwrap().metrics.call_count(), 1);
        assert_eq!(
            b.transform(0).unwrap().metrics.call_count(),
            0,
            "recording on one record's transform stage must not affect another record's"
        );
        assert_eq!(metrics_b.call_count(), 0);
    }
}
