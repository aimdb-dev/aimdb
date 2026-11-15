//! Countable ingress outcomes and shared, per-link statistics.

use crate::envelope::{ContractRef, EnvelopeError, PayloadCodec};
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

/// What a rejected frame claimed and what its link accepts.
///
/// Plain data: [`IngestError`] formats the diagnostic text only when it is
/// displayed, so rejecting a frame on an admitting link does not allocate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionContext {
    /// The frame's contract; `None` when its header did not parse.
    pub received: Option<ContractRef>,
    pub accepted_name: &'static str,
    pub accepted_name_id: u32,
    pub accepted_min: u32,
    pub accepted_max: u32,
    pub detail: AdmissionDetail,
}

/// The specific finding of an admission rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdmissionDetail {
    /// The error's [`message`](IngestError::message) says the rest.
    Message,
    /// The envelope header did not parse.
    Header(EnvelopeError),
    /// The frame names another contract.
    Name,
    Shape {
        received: u64,
        expected: u64,
    },
    Codec {
        received: PayloadCodec,
        expected: PayloadCodec,
    },
}

/// A deserialization/admission failure with a countable reason and diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestError {
    reason: Rejection,
    message: Cow<'static, str>,
    admission: Option<AdmissionContext>,
}
impl IngestError {
    pub fn new(reason: Rejection, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            reason,
            message: message.into(),
            admission: None,
        }
    }
    /// A rejection by an admitting link. A `&'static str` message, or none
    /// with a structured [`AdmissionDetail`], keeps this allocation-free.
    pub fn rejected(
        reason: Rejection,
        admission: AdmissionContext,
        message: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            reason,
            message: message.into(),
            admission: Some(admission),
        }
    }
    pub const fn reason(&self) -> Rejection {
        self.reason
    }
    /// The free-text part of the diagnostic. [`detail`](Self::detail) and
    /// `Display` add the admission context, when there is one.
    pub fn message(&self) -> &str {
        &self.message
    }
    pub const fn admission(&self) -> Option<&AdmissionContext> {
        self.admission.as_ref()
    }
    /// The full diagnostic without the reason, formatted on display.
    pub const fn detail(&self) -> IngestDetail<'_> {
        IngestDetail(self)
    }
}

/// Displays an [`IngestError`]'s diagnostic; see [`IngestError::detail`].
#[derive(Clone, Copy, Debug)]
pub struct IngestDetail<'a>(&'a IngestError);

impl core::fmt::Display for IngestDetail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Some(admission) = &self.0.admission else {
            return f.write_str(&self.0.message);
        };
        let accepts = |f: &mut core::fmt::Formatter<'_>| {
            write!(
                f,
                "accepts {}@{}..={}",
                admission.accepted_name, admission.accepted_min, admission.accepted_max
            )
        };
        let Some(received) = admission.received else {
            match admission.detail {
                AdmissionDetail::Header(error) => write!(f, "invalid envelope header ({error}), ")?,
                _ => f.write_str("no contract received, ")?,
            }
            return accepts(f);
        };
        if received.name_id() == admission.accepted_name_id {
            write!(f, "received {}", admission.accepted_name)?;
        } else {
            write!(f, "received name_id {:#010x}", received.name_id())?;
        }
        write!(f, "@{}, ", received.version())?;
        accepts(f)?;
        f.write_str(": ")?;
        match admission.detail {
            AdmissionDetail::Message => f.write_str(&self.0.message),
            AdmissionDetail::Header(error) => write!(f, "invalid envelope header ({error})"),
            AdmissionDetail::Name => {
                write!(f, "expected name_id {:#010x}", admission.accepted_name_id)
            }
            AdmissionDetail::Shape { received, expected } => {
                write!(
                    f,
                    "received shape {received:#018x}, expected {expected:#018x}"
                )
            }
            AdmissionDetail::Codec { received, expected } => {
                write!(f, "received codec {received:?}, expected {expected:?}")
            }
        }
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for IngestDetail<'_> {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(f, "{}", defmt::Display2Format(self));
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
        write!(f, "{}: {}", self.reason, self.detail())
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
    use alloc::string::ToString;

    #[test]
    fn an_admission_rejection_formats_its_context_on_display() {
        let admission = AdmissionContext {
            received: Some(ContractRef::new(7, 3, None)),
            accepted_name: "temperature",
            accepted_name_id: 7,
            accepted_min: 1,
            accepted_max: 2,
            detail: AdmissionDetail::Message,
        };
        let error = IngestError::rejected(Rejection::TooNew, admission, "no downgrade");
        assert_eq!(error.message(), "no downgrade");
        assert_eq!(
            error.to_string(),
            "too_new: received temperature@3, accepts temperature@1..=2: no downgrade"
        );
        let other = AdmissionContext {
            received: Some(ContractRef::new(9, 1, None)),
            detail: AdmissionDetail::Name,
            ..admission
        };
        assert_eq!(
            IngestError::rejected(Rejection::ContractMismatch, other, "")
                .detail()
                .to_string(),
            "received name_id 0x00000009@1, accepts temperature@1..=2: \
             expected name_id 0x00000007"
        );
        let header = AdmissionContext {
            received: None,
            detail: AdmissionDetail::Header(EnvelopeError::Malformed),
            ..admission
        };
        assert_eq!(
            IngestError::rejected(Rejection::Malformed, header, "")
                .detail()
                .to_string(),
            "invalid envelope header (Malformed), accepts temperature@1..=2"
        );
        // Without a context the message is the whole diagnostic.
        assert_eq!(IngestError::from("bad").to_string(), "decode_error: bad");
    }

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
