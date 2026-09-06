//! COBS-framed serial/UART transport connector for AimDB — record mirroring and
//! remote access over a serial line.
//!
//! A thin, swappable transport crate (the serial sibling of `aimdb-uds-connector`):
//! it contributes only the `Dialer`/`Listener`/`Connection` triple plus thin
//! sugar; the AimX codec ([`AimxCodec`](aimdb_core::session::aimx::AimxCodec)),
//! dispatch ([`AimxDispatch`](aimdb_core::session::aimx::AimxDispatch)), and the
//! runtime-neutral session engines are reused verbatim from `aimdb-core`.
//!
//! The wire is the same compact AimX JSON as UDS, but framed with **COBS**
//! (Consistent Overhead Byte Stuffing) and a `0x00` delimiter instead of a
//! newline — self-synchronizing on a lossy/unframed serial medium. See
//! [`framing`].
//!
//! One path for both runtimes: the byte source comes from an adapter
//! (`EmbassyUart` on the MCU, `TokioByteStream` over a `SerialStream` on the
//! host) and this crate contributes only the COBS framer. A UART is
//! point-to-point, so the stream is moved in and served once.
//!
//! Both speak the `serial://` scheme by default ([`DEFAULT_SCHEME`]).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// The COBS codec, and (under either runtime feature) that codec behind core's
// `Framer` plus the `FramedConnection` aliases it forms with each adapter's
// byte source. Supersedes the two per-runtime transport modules below, which it
// will replace outright.
pub mod framing;

// Runtime-neutral `SerialClient`/`SerialServer` over an adapter's byte stream.
#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub mod connector;

/// The default scheme `SerialClient`/`SerialServer` register when none is given.
///
/// Transport-matched (like UDS's `"uds"`), so `link_to("serial://<record>")` reads
/// at the call site. Override with `.scheme(...)` when running more than one
/// remote connector.
pub const DEFAULT_SCHEME: &str = "serial";

/// Mark each record named in the policy's writable set as writable, so
/// `record.list` advertises the `writable` flag (the dispatch also enforces it).
/// Shared by both `SerialServer` halves; mirrors the UDS connector.
#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub(crate) fn apply_writable(db: &aimdb_core::AimDb, config: &aimdb_core::remote::AimxConfig) {
    for key in config.security_policy.writable_records() {
        if let Some(id) = db.inner().resolve_str(&key) {
            if let Some(storage) = db.inner().storage(id) {
                storage.set_writable_erased(true);
            }
        }
    }
}

#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use connector::{framed, OneShotDialer, OneShotListener, SerialClient, SerialServer};

#[cfg(feature = "tokio-runtime")]
pub use connector::SerialPortDialer;
