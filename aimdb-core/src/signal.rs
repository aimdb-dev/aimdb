//! Signal gauge handle: per-record domain-signal statistics.
//!
//! `RecordRegistrar::signal_gauge` hands back a [`SignalGaugeHandle`]. Feeding
//! values in with [`update`](SignalGaugeHandle::update) folds them into the
//! record's `SignalStats` (last/min/max/mean), which surface on `record.list` /
//! `record.get` and stage-profiling output.
//!
//! Like [`with_name`](crate::typed_api::RecordRegistrar::with_name), the handle
//! is **always available**: when the `observability` feature is off (or no gauge
//! was registered) it is inert and `update` is a no-op, so callers — including
//! the `.observe()` extension in `aimdb-data-contracts` — never `#[cfg]` on
//! core's features.

/// A handle to a per-record signal gauge.
///
/// Cheap to clone (an `Arc` bump, or nothing when inert). Obtained from
/// [`RecordRegistrar::signal_gauge`](crate::typed_api::RecordRegistrar::signal_gauge).
#[derive(Clone)]
pub struct SignalGaugeHandle {
    #[cfg(feature = "observability")]
    stats: Option<alloc::sync::Arc<crate::profiling::SignalStats>>,
}

impl SignalGaugeHandle {
    /// An inert handle that records nothing (feature off, or no gauge registered).
    #[cfg_attr(feature = "observability", allow(dead_code))]
    pub(crate) fn inert() -> Self {
        Self {
            #[cfg(feature = "observability")]
            stats: None,
        }
    }

    /// A live handle backed by shared statistics.
    #[cfg(feature = "observability")]
    pub(crate) fn live(stats: alloc::sync::Arc<crate::profiling::SignalStats>) -> Self {
        Self { stats: Some(stats) }
    }

    /// Records one signal sample. No-op on an inert handle.
    pub fn update(&self, value: f64) {
        #[cfg(feature = "observability")]
        if let Some(stats) = &self.stats {
            stats.update(value);
        }
        #[cfg(not(feature = "observability"))]
        let _ = value;
    }
}
