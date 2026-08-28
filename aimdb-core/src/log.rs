//! Crate-private logging macros.
//!
//! `log_trace!`/`log_debug!`/`log_info!`/`log_warn!`/`log_error!` forward to every enabled
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
//! # Mirroring the feature
//!
//! These macros are `#[macro_export]`ed, so `#[cfg(feature = ...)]` inside them
//! resolves against the *calling* crate. A crate expanding the facade must
//! declare a feature of the same name and forward it, or its events silently go
//! nowhere:
//!
//! ```toml
//! tracing = ["aimdb-core/tracing"]   # no `dep:tracing`
//! log     = ["aimdb-core/log"]       # no `dep:log`
//! ```
//!
//! Neither arm needs the dependency declared: both go through `$crate::__private`.
//! The *feature* is what cannot be dropped — a `#[cfg]` in a `#[macro_export]`ed
//! macro is resolved where it expands, so a crate without it would emit nothing.
//!
//! That is not a silent failure any more: an undeclared feature name makes
//! `unexpected_cfgs` fire at every call site, which is a warning in an ordinary
//! build and an error under `-D warnings`, as `make clippy` runs it. A dropped
//! mirror therefore breaks CI rather than quietly muting a crate.
//! `aimdb-sync/tests/log_facade.rs` additionally proves delivery end to end.

#[macro_export]
macro_rules! log_trace {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        $crate::__private::tracing::trace!($s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::trace!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log")))]
        { let _ = ($( & $x ),*); }
    }};
}

#[macro_export]
macro_rules! log_debug {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        $crate::__private::tracing::debug!($s $(, $x)*);
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
        $crate::__private::tracing::info!($s $(, $x)*);
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
        $crate::__private::tracing::warn!($s $(, $x)*);
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
        $crate::__private::tracing::error!($s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::error!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log")))]
        { let _ = ($( & $x ),*); }
    }};
}
