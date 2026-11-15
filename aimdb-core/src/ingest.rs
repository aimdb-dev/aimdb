//! Countable ingress outcomes and shared, per-link statistics.

use alloc::{borrow::Cow, string::String, vec::Vec};

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
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// Lifetime follows the registered link, not a connection or dispatcher.
/// A short lock keeps 64-bit counters coherent on embedded targets too.
/// Neither recording method calls user code or allocates while holding it.
pub(crate) struct InboundStats(spin::Mutex<InboundRouteStats>);
impl InboundStats {
    pub(crate) fn new(versions: &[u32]) -> Self {
        let mut upgraded: Vec<_> = versions
            .iter()
            .map(|&from| UpgradeCount { from, count: 0 })
            .collect();
        upgraded.sort_unstable_by_key(|c| c.from);
        upgraded.dedup_by_key(|c| c.from);
        Self(spin::Mutex::new(InboundRouteStats {
            upgraded,
            ..Default::default()
        }))
    }
    pub(crate) fn snapshot(&self) -> InboundRouteStats {
        self.0.lock().clone()
    }
    pub(crate) fn accept(&self, outcome: IngestOutcome) {
        let mut stats = self.0.lock();
        stats.accepted = stats.accepted.saturating_add(1);
        if let Some(from) = outcome.upgraded_from() {
            if let Some(counter) = stats.upgraded.iter_mut().find(|c| c.from == from) {
                counter.count = counter.count.saturating_add(1);
            } else {
                stats.untracked_upgrades = stats.untracked_upgrades.saturating_add(1);
            }
        }
    }
    pub(crate) fn reject(&self, reason: Rejection) {
        let mut stats = self.0.lock();
        let counter = match reason {
            Rejection::Malformed => &mut stats.malformed,
            Rejection::UnsupportedFormat => &mut stats.unsupported_format,
            Rejection::ContractMismatch => &mut stats.contract_mismatch,
            Rejection::TooNew => &mut stats.too_new,
            Rejection::TooOld => &mut stats.too_old,
            Rejection::ShapeMismatch => &mut stats.shape_mismatch,
            Rejection::CodecMismatch => &mut stats.codec_mismatch,
            Rejection::DecodeError => &mut stats.decode_error,
        };
        *counter = counter.saturating_add(1);
    }
}
