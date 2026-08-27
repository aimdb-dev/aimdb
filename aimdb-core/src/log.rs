//! Crate-private logging macros.
//!
//! `log_debug!`/`log_info!`/`log_warn!`/`log_error!` forward to every enabled
//! *destination* — the `tracing` event macro under the `tracing` feature, the
//! `log` crate under the `log` feature — and otherwise expand to a no-op that
//! still borrows the arguments, so call sites compile identically (no
//! unused-variable warnings) under every feature combination. This replaces the
//! per-call-site `#[cfg(feature = "tracing")]` gates.
//!
//! Notes:
//! - The no-op branch *borrows* (and therefore evaluates) the arguments — keep
//!   them cheap (getters, lengths, references), never allocate in hot paths.
//! - `defmt` is intentionally not folded in: most call sites use `{:?}` with
//!   types that do not implement `defmt::Format` (e.g. `DbError`, `String`,
//!   `Vec<String>`). The few sites that mirror events to defmt (router.rs)
//!   keep explicit `#[cfg(feature = "defmt")]` gates next to these macros.
//!
//! # Two destinations, and the feature that selects each
//!
//! These macros are `#[macro_export]`ed, so their bodies expand in the *calling*
//! crate and every `#[cfg(feature = ...)]` inside them is resolved against that
//! crate's feature set — not against `aimdb-core`'s. A crate that expands the
//! facade therefore has to declare a feature of the same name and forward it:
//!
//! ```toml
//! tracing = ["dep:tracing", "aimdb-core/tracing"]
//! log     = ["aimdb-core/log"]   # no `dep:log` — see below
//! ```
//!
//! Forgetting the mirror is silent: the arm simply never expands and that
//! crate's events go nowhere. `aimdb-sync` is the only crate outside
//! `aimdb-core` that expands the facade today, and `tests/log_facade_target.rs`
//! asserts its events arrive.
//!
//! The `log` arm names `$crate::__private::log`, a re-export, so a facade user
//! needs the *feature* but not the *dependency*. The `tracing` arm still names
//! `::tracing` and so still requires `dep:tracing` downstream; routing it
//! through `__private` the same way is a separate, non-breaking cleanup.
//!
//! # Emitting to both
//!
//! `tracing` and `log` may both be on, and then both arms run. That is the
//! point when a process installed two destinations, and a duplicate when it
//! installed one — see the `log` feature section of the crate docs for the ways
//! a single-destination process ends up seeing an event twice, and how to
//! switch them off.

#[macro_export]
macro_rules! log_debug {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::debug!($s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::debug!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log")))]
        { let _ = ($( & $x ),*); }
    }};
}

#[macro_export]
macro_rules! log_info {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::info!($s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::info!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log")))]
        { let _ = ($( & $x ),*); }
    }};
}

#[macro_export]
macro_rules! log_warn {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::warn!($s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::warn!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log")))]
        { let _ = ($( & $x ),*); }
    }};
}

#[macro_export]
macro_rules! log_error {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::error!($s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::error!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log")))]
        { let _ = ($( & $x ),*); }
    }};
}
