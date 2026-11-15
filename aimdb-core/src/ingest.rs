//! Countable ingress outcomes and shared, per-link statistics.

use alloc::{borrow::Cow, boxed::Box, string::String, vec::Vec};
use core::sync::atomic::Ordering::Relaxed;

/// Stable rejection categories, in admission precedence order (design 062 §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum Rejection {
    Malformed,
    UnsupportedFormat,
    ContractMismatch,
    TooNew,
    TooOld,
    ShapeMismatch,
    CodecMismatch,
    DecodeError,
}
impl Rejection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnsupportedFormat => "unsupported_format",
            Self::ContractMismatch => "contract_mismatch",
            Self::TooNew => "too_new",
            Self::TooOld => "too_old",
            Self::ShapeMismatch => "shape_mismatch",
            Self::CodecMismatch => "codec_mismatch",
            Self::DecodeError => "decode_error",
        }
    }
}
impl core::fmt::Display for Rejection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A deserialization/admission failure with a countable reason and diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestError {
    reason: Rejection,
    message: Cow<'static, str>,
}
impl IngestError {
    pub fn new(reason: Rejection, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
    pub const fn reason(&self) -> Rejection {
        self.reason
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}
impl From<String> for IngestError {
    fn from(message: String) -> Self {
        Self::new(Rejection::DecodeError, message)
    }
}
impl From<&'static str> for IngestError {
    fn from(message: &'static str) -> Self {
        Self::new(Rejection::DecodeError, message)
    }
}
impl core::fmt::Display for IngestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.reason, self.message)
    }
}
impl core::error::Error for IngestError {}

/// Successful fused ingest result. Reported only after the value was produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    upgraded_from: Option<u32>,
}
impl IngestOutcome {
    pub const fn current() -> Self {
        Self {
            upgraded_from: None,
        }
    }
    pub const fn upgraded(from: u32) -> Self {
        Self {
            upgraded_from: Some(from),
        }
    }
    pub const fn upgraded_from(self) -> Option<u32> {
        self.upgraded_from
    }
}

/// A decoded value and its admission outcome, fused with production by core.
#[derive(Debug)]
pub struct IngestValue<T> {
    pub(crate) value: T,
    pub(crate) outcome: IngestOutcome,
}
impl<T> IngestValue<T> {
    /// The admitted value, before core stamps and produces it.
    pub fn value(&self) -> &T {
        &self.value
    }
    pub const fn outcome(&self) -> IngestOutcome {
        self.outcome
    }
    pub fn into_value(self) -> T {
        self.value
    }

    pub fn current(value: T) -> Self {
        Self {
            value,
            outcome: IngestOutcome::current(),
        }
    }
    pub fn upgraded(value: T, from: u32) -> Self {
        Self {
            value,
            outcome: IngestOutcome::upgraded(from),
        }
    }
}

/// Successful upgrades from a source version declared at registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "remote", derive(serde::Serialize, serde::Deserialize))]
pub struct UpgradeCount {
    pub from: u32,
    pub count: u64,
}

/// Point-in-time ingress counters for one registered link.
/// Inspection may allocate the upgrade list; recording never allocates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "remote", derive(serde::Serialize, serde::Deserialize))]
pub struct InboundRouteStats {
    pub accepted: u64,
    pub malformed: u64,
    pub unsupported_format: u64,
    pub contract_mismatch: u64,
    pub too_new: u64,
    pub too_old: u64,
    pub shape_mismatch: u64,
    pub codec_mismatch: u64,
    pub decode_error: u64,
    pub upgraded: Vec<UpgradeCount>,
    /// Custom ingest callbacks that reported a version not declared at registration.
    pub untracked_upgrades: u64,
}

/// One link's counters in the existing record metadata/metrics response.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "remote", derive(serde::Serialize, serde::Deserialize))]
pub struct InboundRouteMetrics {
    pub link: String,
    pub stats: InboundRouteStats,
}

/// One always-on counter, on native atomics only: recording never takes a
/// lock, so dispatch from an executor of any priority cannot block on it.
///
/// Where the target has native 64-bit atomics the counter is 64 bits wide.
/// Elsewhere (e.g. thumbv7em) it is 32 bits wide and stops at `u32::MAX`
/// rather than wrapping: a lock-based 64-bit fallback could hang a
/// higher-priority executor that preempts an update.
#[cfg(target_has_atomic = "64")]
#[derive(Default)]
struct Counter(core::sync::atomic::AtomicU64);

#[cfg(target_has_atomic = "64")]
impl Counter {
    fn increment(&self) {
        self.0.fetch_add(1, Relaxed);
    }
    fn get(&self) -> u64 {
        self.0.load(Relaxed)
    }
}

#[cfg(not(target_has_atomic = "64"))]
type Counter = NarrowCounter;

/// The 32-bit, saturating [`Counter`] of targets without 64-bit atomics.
#[cfg(any(test, not(target_has_atomic = "64")))]
#[derive(Default)]
struct NarrowCounter(core::sync::atomic::AtomicU32);

#[cfg(any(test, not(target_has_atomic = "64")))]
impl NarrowCounter {
    fn increment(&self) {
        // `Err` is the counter already at its maximum.
        let _ = self
            .0
            .fetch_update(Relaxed, Relaxed, |count| count.checked_add(1));
    }
    fn get(&self) -> u64 {
        u64::from(self.0.load(Relaxed))
    }
}

/// Lifetime follows the registered link, not a connection or dispatcher.
///
/// Lock-free: one native atomic [`Counter`] per value, recorded with relaxed
/// ordering. Recording never allocates, locks, or calls user code, so
/// dispatch from executors of any priority cannot block on it. On targets
/// without native 64-bit atomics each counter saturates at `u32::MAX`.
///
/// A snapshot reads each counter independently: during concurrent dispatch it
/// may already count a message in `accepted` but not yet in its upgrade
/// counter. Once dispatch is idle, a snapshot is exact.
pub(crate) struct InboundStats {
    accepted: Counter,
    /// Indexed by [`rejection_index`].
    rejected: [Counter; REJECTIONS],
    untracked_upgrades: Counter,
    /// Sorted by source version, fixed at registration.
    upgraded: Box<[(u32, Counter)]>,
}

const REJECTIONS: usize = 8;

const fn rejection_index(reason: Rejection) -> usize {
    match reason {
        Rejection::Malformed => 0,
        Rejection::UnsupportedFormat => 1,
        Rejection::ContractMismatch => 2,
        Rejection::TooNew => 3,
        Rejection::TooOld => 4,
        Rejection::ShapeMismatch => 5,
        Rejection::CodecMismatch => 6,
        Rejection::DecodeError => 7,
    }
}

impl InboundStats {
    pub(crate) fn new(versions: &[u32]) -> Self {
        let mut sorted: Vec<u32> = versions.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        Self {
            accepted: Counter::default(),
            rejected: Default::default(),
            untracked_upgrades: Counter::default(),
            upgraded: sorted
                .into_iter()
                .map(|from| (from, Counter::default()))
                .collect(),
        }
    }
    pub(crate) fn snapshot(&self) -> InboundRouteStats {
        let rejected = |reason| self.rejected[rejection_index(reason)].get();
        // No `..`: a new counter must be read here explicitly.
        InboundRouteStats {
            accepted: self.accepted.get(),
            malformed: rejected(Rejection::Malformed),
            unsupported_format: rejected(Rejection::UnsupportedFormat),
            contract_mismatch: rejected(Rejection::ContractMismatch),
            too_new: rejected(Rejection::TooNew),
            too_old: rejected(Rejection::TooOld),
            shape_mismatch: rejected(Rejection::ShapeMismatch),
            codec_mismatch: rejected(Rejection::CodecMismatch),
            decode_error: rejected(Rejection::DecodeError),
            upgraded: self
                .upgraded
                .iter()
                .map(|(from, count)| UpgradeCount {
                    from: *from,
                    count: count.get(),
                })
                .collect(),
            untracked_upgrades: self.untracked_upgrades.get(),
        }
    }
    pub(crate) fn accept(&self, outcome: IngestOutcome) {
        self.accepted.increment();
        if let Some(from) = outcome.upgraded_from() {
            match self.upgraded.binary_search_by_key(&from, |(v, _)| *v) {
                Ok(i) => self.upgraded[i].1.increment(),
                Err(_) => self.untracked_upgrades.increment(),
            };
        }
    }
    pub(crate) fn reject(&self, reason: Rejection) {
        self.rejected[rejection_index(reason)].increment();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_narrow_counter_stops_at_its_maximum() {
        let counter = NarrowCounter::default();
        counter.increment();
        assert_eq!(counter.get(), 1);
        counter.0.store(u32::MAX - 1, Relaxed);
        counter.increment();
        counter.increment();
        assert_eq!(counter.get(), u64::from(u32::MAX));
    }

    #[test]
    fn snapshot_copies_every_counter() {
        let stats = InboundStats::new(&[3, 1, 3]);
        stats.accept(IngestOutcome::current());
        stats.accept(IngestOutcome::upgraded(1));
        stats.accept(IngestOutcome::upgraded(2)); // not declared
        stats.reject(Rejection::TooNew);
        stats.reject(Rejection::DecodeError);
        let snapshot = stats.snapshot();
        assert_eq!(
            snapshot,
            InboundRouteStats {
                accepted: 3,
                too_new: 1,
                decode_error: 1,
                upgraded: alloc::vec![
                    UpgradeCount { from: 1, count: 1 },
                    UpgradeCount { from: 3, count: 0 },
                ],
                untracked_upgrades: 1,
                ..Default::default()
            }
        );
    }
}
