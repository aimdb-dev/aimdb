//! Instance time: one monotonic clock per database instance (design 058 §3).
//!
//! A [`Stamp`] is nanoseconds since the instance started (`build()`). Stamps
//! are scoped to one instance lifetime: comparing stamps across a restart, or
//! across two instances, is meaningless.

/// Nanoseconds since the instance started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Stamp(u64);

impl Stamp {
    pub const fn from_nanos(n: u64) -> Self {
        Self(n)
    }

    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// `self − earlier`, saturating at zero.
    pub fn since(self, earlier: Stamp) -> core::time::Duration {
        core::time::Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// Clamps a `u128` nanosecond count (e.g. `Duration::as_nanos`) to a stamp,
    /// saturating at `u64::MAX`.
    pub(crate) fn saturating_from_nanos(n: u128) -> Self {
        Self(u64::try_from(n).unwrap_or(u64::MAX))
    }
}

/// The instance's clock, created once by `build()` through
/// [`RuntimeOps::new_clock`](crate::RuntimeOps::new_clock).
///
/// The one source of time reads: two time sources with distinct guarantees.
pub trait InstanceClock: Send + Sync {
    /// Monotonic, instance-relative: time since the clock was created.
    /// Nondecreasing (consecutive readings may be equal); saturates at
    /// `u64::MAX`.
    fn now(&self) -> Stamp;

    /// Absolute wall-clock time as `(seconds, nanoseconds)` since the Unix
    /// epoch. Independent of [`now`](Self::now)'s origin: it may be adjusted
    /// (clock corrections) and is `None` where the platform has no wall clock
    /// (e.g. a bare MCU with no RTC anchor). Never derive it from a stamp.
    fn unix_time(&self) -> Option<(u64, u32)>;
}

/// Converts an elapsed time to a stamp, saturating at `u64::MAX`.
///
/// For adapters: `Duration::as_nanos` is `u128`.
pub fn stamp_from_duration(d: core::time::Duration) -> Stamp {
    Stamp::saturating_from_nanos(d.as_nanos())
}

/// Converts elapsed milliseconds as `f64` (e.g. `performance.now()`
/// differences) to a stamp, clamped to `0..=u64::MAX`; `NaN` maps to zero.
pub fn stamp_from_millis_f64(ms: f64) -> Stamp {
    let ns = ms * 1_000_000.0;
    // `as` saturates on float → int and maps NaN to 0.
    Stamp(ns as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn since_saturates_at_zero() {
        let a = Stamp::from_nanos(10);
        let b = Stamp::from_nanos(25);
        assert_eq!(b.since(a), Duration::from_nanos(15));
        assert_eq!(a.since(b), Duration::ZERO);
    }

    #[test]
    fn duration_conversion_saturates() {
        assert_eq!(stamp_from_duration(Duration::from_nanos(7)).as_nanos(), 7);
        assert_eq!(stamp_from_duration(Duration::MAX).as_nanos(), u64::MAX);
    }

    #[test]
    fn millis_f64_conversion_clamps() {
        assert_eq!(stamp_from_millis_f64(1.5).as_nanos(), 1_500_000);
        assert_eq!(stamp_from_millis_f64(-3.0).as_nanos(), 0);
        assert_eq!(stamp_from_millis_f64(f64::NAN).as_nanos(), 0);
        assert_eq!(stamp_from_millis_f64(f64::INFINITY).as_nanos(), u64::MAX);
    }
}
