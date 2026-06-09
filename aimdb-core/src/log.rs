//! Crate-private logging macros (design 034 / #132).
//!
//! `log_debug!`/`log_info!`/`log_warn!`/`log_error!` forward to the matching
//! `tracing` event macro when the `tracing` feature is enabled; otherwise they
//! expand to a no-op that still borrows the arguments, so call sites compile
//! identically (no unused-variable warnings) under every feature combination.
//! This replaces the per-call-site `#[cfg(feature = "tracing")]` gates.
//!
//! Notes:
//! - The no-op branch *borrows* (and therefore evaluates) the arguments — keep
//!   them cheap (getters, lengths, references), never allocate in hot paths.
//! - `defmt` is intentionally not folded in: most call sites use `{:?}` with
//!   types that do not implement `defmt::Format` (e.g. `DbError`, `String`,
//!   `Vec<String>`). The few sites that mirror events to defmt (router.rs)
//!   keep explicit `#[cfg(feature = "defmt")]` gates next to these macros.

macro_rules! log_debug {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::debug!($s $(, $x)*);
        #[cfg(not(feature = "tracing"))]
        { let _ = ($( & $x ),*); }
    }};
}

macro_rules! log_info {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::info!($s $(, $x)*);
        #[cfg(not(feature = "tracing"))]
        { let _ = ($( & $x ),*); }
    }};
}

macro_rules! log_warn {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::warn!($s $(, $x)*);
        #[cfg(not(feature = "tracing"))]
        { let _ = ($( & $x ),*); }
    }};
}

macro_rules! log_error {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::error!($s $(, $x)*);
        #[cfg(not(feature = "tracing"))]
        { let _ = ($( & $x ),*); }
    }};
}
