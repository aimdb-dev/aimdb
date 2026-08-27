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
//! # Mirroring the feature
//!
//! These macros are `#[macro_export]`ed, so `#[cfg(feature = ...)]` inside them
//! resolves against the *calling* crate. A crate expanding the facade must
//! declare a feature of the same name and forward it, or its events silently go
//! nowhere:
//!
//! ```toml
//! tracing = ["dep:tracing", "aimdb-core/tracing"]
//! log     = ["aimdb-core/log"]   # no `dep:log`: the arm goes through `__private`
//! ```
//!
//! `aimdb-sync` is the only such crate today; its `tests/log_facade.rs` guards it.

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
