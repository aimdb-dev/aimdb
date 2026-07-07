//! Simulation capability for data contracts (dev-tier, `feature = "simulatable"`).
//!
//! Implementing [`Simulatable`] unlocks exactly one verb —
//! [`SimulatableRegistrarExt::simulate`] — which installs a source that emits
//! synthetic samples on a timer. This is the **dev tier**: it must never ship in
//! a production binary. Sim-to-real selection is a compile-time `#[cfg]` in the
//! application, never a runtime flag, and `rand` is the tracer CI uses to prove
//! production dependency graphs are sim-free (see `make check-no-sim`).

use serde::{Deserialize, Serialize};

use aimdb_core::typed_api::RecordRegistrar;

use crate::SchemaType;

// ═══════════════════════════════════════════════════════════════════
// SIMULATABLE TRAIT
// ═══════════════════════════════════════════════════════════════════

/// Dev-only capability: generate realistic synthetic data.
///
/// This is an intrinsic capability of the schema type itself, not a policy
/// decision. If a type can be simulated, implement this — then call
/// [`SimulatableRegistrarExt::simulate`] on the registrar.
pub trait Simulatable: SchemaType {
    /// Type-specific generation parameters (a temperature defines walk bounds,
    /// a GPS track defines waypoints, …). Use [`RandomWalkParams`] for scalar
    /// walks, or define your own.
    type Params: Clone + Send + Sync + Default + 'static;

    /// Generate the next sample.
    ///
    /// - `params`: type-specific generation parameters.
    /// - `previous`: last generated value, enabling walks/trends.
    /// - `rng`: caller-supplied RNG (bring [`rand::RngExt`] into scope for
    ///   `.random()`).
    /// - `timestamp_ms`: Unix millis supplied by the driving loop.
    fn simulate<R: rand::Rng>(
        params: &Self::Params,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp_ms: u64,
    ) -> Self;
}

// ═══════════════════════════════════════════════════════════════════
// PROFILE + OFF-THE-SHELF PARAMS
// ═══════════════════════════════════════════════════════════════════

/// Loop policy + generation params for one record.
#[derive(Clone, Debug, Default)]
pub struct SimProfile<P> {
    /// Interval between samples (milliseconds).
    pub interval_ms: u64,
    /// Type-specific generation parameters.
    pub params: P,
}

/// Off-the-shelf [`Simulatable::Params`] for scalar signals that wander around
/// a base value: set `type Params = RandomWalkParams` and derive the next
/// sample from `previous` plus a bounded random step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RandomWalkParams {
    /// Base/center value for the walk.
    pub base: f64,
    /// Maximum deviation from base.
    pub variation: f64,
    /// Linear trend applied per sample (positive = increasing).
    pub trend: f64,
    /// Step-size multiplier for the random walk (0.0–1.0).
    pub step: f64,
}

impl Default for RandomWalkParams {
    fn default() -> Self {
        Self {
            base: 0.0,
            variation: 1.0,
            trend: 0.0,
            step: 0.2,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// REGISTRAR EXTENSION: `.simulate(profile, rng)`
// ═══════════════════════════════════════════════════════════════════

/// Adds `.simulate(profile, rng)` to [`RecordRegistrar`] for [`Simulatable`] types.
pub trait SimulatableRegistrarExt<'a, T>
where
    T: Simulatable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Install a source that emits `T::simulate(...)` every `interval_ms`,
    /// driving the caller-supplied RNG (OS entropy on std, seeded PRNG on
    /// no_std, fixed seed in tests).
    ///
    /// Installs a **source**, so single-writer-per-key is enforced by `build()`
    /// exactly as for a hardware `.source()` — the two are mutually exclusive by
    /// the app's `#[cfg]`, never both present in one binary.
    fn simulate<R>(
        &mut self,
        profile: SimProfile<T::Params>,
        rng: R,
    ) -> &mut RecordRegistrar<'a, T>
    where
        R: rand::Rng + Send + 'static;
}

impl<'a, T> SimulatableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Simulatable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn simulate<R>(
        &mut self,
        profile: SimProfile<T::Params>,
        mut rng: R,
    ) -> &mut RecordRegistrar<'a, T>
    where
        R: rand::Rng + Send + 'static,
    {
        self.source(move |ctx, producer| async move {
            let mut prev: Option<T> = None;
            loop {
                let now_ms = ctx
                    .time()
                    .unix_time()
                    .map(|(s, ns)| s.saturating_mul(1000) + (ns / 1_000_000) as u64)
                    .unwrap_or(0);
                let next = T::simulate(&profile.params, prev.as_ref(), &mut rng, now_ms);
                producer.produce(next.clone());
                prev = Some(next);
                ctx.time().sleep_millis(profile.interval_ms.max(1)).await;
            }
        })
        .with_name("simulate")
    }
}

// ═══════════════════════════════════════════════════════════════════
// TESTS
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngExt, SeedableRng};

    #[derive(Clone, Debug, PartialEq)]
    struct Scalar(f64);

    impl SchemaType for Scalar {
        const NAME: &'static str = "scalar";
    }

    impl Simulatable for Scalar {
        type Params = RandomWalkParams;

        fn simulate<R: rand::Rng>(
            params: &Self::Params,
            previous: Option<&Self>,
            rng: &mut R,
            _timestamp_ms: u64,
        ) -> Self {
            let base = previous.map(|p| p.0).unwrap_or(params.base);
            let delta = (rng.random::<f64>() - 0.5) * params.variation * params.step + params.trend;
            Scalar(base + delta)
        }
    }

    #[test]
    fn random_walk_defaults() {
        let p = RandomWalkParams::default();
        assert_eq!(p.variation, 1.0);
        assert_eq!(p.step, 0.2);
        assert_eq!(p.base, 0.0);
        assert_eq!(p.trend, 0.0);
    }

    #[test]
    fn sim_profile_default_is_zero_interval() {
        let profile: SimProfile<RandomWalkParams> = SimProfile::default();
        assert_eq!(profile.interval_ms, 0);
        assert_eq!(profile.params, RandomWalkParams::default());
    }

    #[test]
    fn simulate_is_deterministic_for_fixed_seed() {
        let params = RandomWalkParams::default();
        // SmallRng is seedable and available with `default-features = false`.
        let mut a = rand::rngs::SmallRng::seed_from_u64(42);
        let mut b = rand::rngs::SmallRng::seed_from_u64(42);

        let mut prev_a: Option<Scalar> = None;
        let mut prev_b: Option<Scalar> = None;
        for ts in 0..5 {
            let sa = Scalar::simulate(&params, prev_a.as_ref(), &mut a, ts);
            let sb = Scalar::simulate(&params, prev_b.as_ref(), &mut b, ts);
            assert_eq!(sa, sb, "same seed must yield the same walk");
            prev_a = Some(sa);
            prev_b = Some(sb);
        }
    }
}
