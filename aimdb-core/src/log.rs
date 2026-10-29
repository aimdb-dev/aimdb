//! Crate-private logging macros.
//!
//! `log_trace!`/`log_debug!`/`log_info!`/`log_warn!`/`log_error!` forward to every enabled
//! *destination* — the `tracing` event macro under the `tracing` feature, the
//! `log` crate under the `log` feature, `defmt` under the `defmt` feature — and
//! otherwise expand to a no-op that still borrows the arguments, so call sites
//! compile identically (no unused-variable warnings) under every feature
//! combination. This replaces the per-call-site `#[cfg(feature = "tracing")]`
//! gates.
//!
//! Notes:
//! - The no-op branch *borrows* (and therefore evaluates) the arguments — keep
//!   them cheap (getters, lengths, references), never allocate in hot paths.
//! - `target: <const &str>,` before the format string sets the `tracing` /
//!   `log` target (default: the calling module, as both crates do). Events
//!   reported from a central `events.rs` pass the module that raised them, so
//!   per-module target filters keep matching. `defmt` has no runtime target
//!   and ignores it: `DEFMT_LOG` filters on the expanding module.
//! - Under `defmt`, every argument must implement `defmt::Format` (the format
//!   strings are shared: `defmt` reads `{}` and `{:?}` alike). Core's own
//!   types do (#352); a crate logging its own types derives or implements
//!   `Format` for them under its `defmt` feature, and wraps a foreign type in
//!   [`LogDisplay`] / [`LogDebug`].
//!
//! # Mirroring the feature
//!
//! These macros are `#[macro_export]`ed, so `#[cfg(feature = ...)]` inside them
//! resolves against the *calling* crate. A crate expanding the facade must
//! declare a feature of the same name and forward it, or its events silently go
//! nowhere:
//!
//! ```toml
//! tracing = ["aimdb-core/tracing"]               # no `dep:tracing`
//! log     = ["aimdb-core/log"]                   # no `dep:log`
//! defmt   = ["dep:defmt", "aimdb-core/defmt"]    # `dep:defmt` required
//! ```
//!
//! `tracing` and `log` need no dependency declared: both arms go through
//! `$crate::__private`. `defmt` does: its macros expand to absolute `defmt::`
//! paths, so the calling crate needs `defmt` itself. The *feature* is what
//! cannot be dropped — a `#[cfg]` in a `#[macro_export]`ed macro is resolved
//! where it expands, so a crate without it would emit nothing.
//!
//! That is not a silent failure any more: an undeclared feature name makes
//! `unexpected_cfgs` fire at every call site, which is a warning in an ordinary
//! build and an error under `-D warnings`, as `make clippy` runs it. A dropped
//! mirror therefore breaks CI rather than quietly muting a crate.
//! `aimdb-sync/tests/log_facade.rs` additionally proves delivery end to end.
//!
//! A crate that never builds with `defmt` (a `std`-only connector) declares the
//! name instead, so the `defmt` arm's `#[cfg]` is known to the lint:
//!
//! ```toml
//! [lints.rust]
//! unexpected_cfgs = { level = "warn", check-cfg = ['cfg(feature, values("defmt"))'] }
//! ```

#[macro_export]
macro_rules! log_trace {
    (target: $t:expr, $s:literal $(, $x:expr)* $(,)?) => {{
        let _: &str = $t;
        #[cfg(feature = "tracing")]
        $crate::__private::tracing::trace!(target: $t, $s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::trace!(target: $t, $s $(, $x)*);
        #[cfg(feature = "defmt")]
        ::defmt::trace!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log", feature = "defmt")))]
        { let _ = ($( & $x ),*); }
    }};
    ($s:literal $(, $x:expr)* $(,)?) => {
        $crate::log_trace!(target: ::core::module_path!(), $s $(, $x)*)
    };
}

#[macro_export]
macro_rules! log_debug {
    (target: $t:expr, $s:literal $(, $x:expr)* $(,)?) => {{
        let _: &str = $t;
        #[cfg(feature = "tracing")]
        $crate::__private::tracing::debug!(target: $t, $s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::debug!(target: $t, $s $(, $x)*);
        #[cfg(feature = "defmt")]
        ::defmt::debug!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log", feature = "defmt")))]
        { let _ = ($( & $x ),*); }
    }};
    ($s:literal $(, $x:expr)* $(,)?) => {
        $crate::log_debug!(target: ::core::module_path!(), $s $(, $x)*)
    };
}

#[macro_export]
macro_rules! log_info {
    (target: $t:expr, $s:literal $(, $x:expr)* $(,)?) => {{
        let _: &str = $t;
        #[cfg(feature = "tracing")]
        $crate::__private::tracing::info!(target: $t, $s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::info!(target: $t, $s $(, $x)*);
        #[cfg(feature = "defmt")]
        ::defmt::info!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log", feature = "defmt")))]
        { let _ = ($( & $x ),*); }
    }};
    ($s:literal $(, $x:expr)* $(,)?) => {
        $crate::log_info!(target: ::core::module_path!(), $s $(, $x)*)
    };
}

#[macro_export]
macro_rules! log_warn {
    (target: $t:expr, $s:literal $(, $x:expr)* $(,)?) => {{
        let _: &str = $t;
        #[cfg(feature = "tracing")]
        $crate::__private::tracing::warn!(target: $t, $s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::warn!(target: $t, $s $(, $x)*);
        #[cfg(feature = "defmt")]
        ::defmt::warn!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log", feature = "defmt")))]
        { let _ = ($( & $x ),*); }
    }};
    ($s:literal $(, $x:expr)* $(,)?) => {
        $crate::log_warn!(target: ::core::module_path!(), $s $(, $x)*)
    };
}

#[macro_export]
macro_rules! log_error {
    (target: $t:expr, $s:literal $(, $x:expr)* $(,)?) => {{
        let _: &str = $t;
        #[cfg(feature = "tracing")]
        $crate::__private::tracing::error!(target: $t, $s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::error!(target: $t, $s $(, $x)*);
        #[cfg(feature = "defmt")]
        ::defmt::error!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log", feature = "defmt")))]
        { let _ = ($( & $x ),*); }
    }};
    ($s:literal $(, $x:expr)* $(,)?) => {
        $crate::log_error!(target: ::core::module_path!(), $s $(, $x)*)
    };
}

/// Logs `T` through its `Display` impl in every destination, `defmt` included
/// (via `defmt::Display2Format`).
///
/// For a foreign type that does not implement `defmt::Format` in a `log_*!`
/// call: `log_warn!("send failed: {}", LogDisplay(&e))`. The `tracing` and
/// `log` output is unchanged; under `defmt` the text is formatted on the
/// device, not interned, so keep it to error paths.
pub struct LogDisplay<'a, T: ?Sized>(pub &'a T);

impl<T: core::fmt::Display + ?Sized> core::fmt::Display for LogDisplay<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(feature = "defmt")]
impl<T: core::fmt::Display + ?Sized> defmt::Format for LogDisplay<'_, T> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}", defmt::Display2Format(self))
    }
}

/// Like [`LogDisplay`], through `Debug` (`{:?}`; `defmt::Debug2Format`).
pub struct LogDebug<'a, T: ?Sized>(pub &'a T);

impl<T: core::fmt::Debug + ?Sized> core::fmt::Debug for LogDebug<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(feature = "defmt")]
impl<T: core::fmt::Debug + ?Sized> defmt::Format for LogDebug<'_, T> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}", defmt::Debug2Format(self))
    }
}
