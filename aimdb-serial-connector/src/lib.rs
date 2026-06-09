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
//! # Two halves
//!
//! - **`tokio-runtime`** (std, host/gateway): real serial via `tokio-serial`,
//!   riding the generic [`SessionClientConnector`](aimdb_core::session::SessionClientConnector)
//!   / [`SessionServerConnector`](aimdb_core::session::SessionServerConnector).
//!   See [`tokio_transport`].
//! - **`embassy-runtime`** (`no_std + alloc`, MCU): generic over
//!   [`embedded_io_async`] UART halves; the COBS `Framer` plus thin sugar over the
//!   centralized Embassy session spine in `aimdb-embassy-adapter`, which owns the
//!   force-`Send` plumbing, the framed connection, and all the `unsafe` — this
//!   crate carries none. See [`embassy_transport`].
//!
//! Both speak the `serial://` scheme by default ([`DEFAULT_SCHEME`]).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod framing;

#[cfg(feature = "tokio-runtime")]
pub mod tokio_transport;

#[cfg(feature = "embassy-runtime")]
pub mod embassy_transport;

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
pub(crate) fn apply_writable<R>(db: &aimdb_core::AimDb<R>, config: &aimdb_core::remote::AimxConfig)
where
    R: aimdb_core::RuntimeAdapter + 'static,
{
    for key in config.security_policy.writable_records() {
        if let Some(id) = db.inner().resolve_str(&key) {
            if let Some(storage) = db.inner().storage(id) {
                storage.set_writable_erased(true);
            }
        }
    }
}

// Prefer the tokio names when both halves are compiled (e.g. host tests).
#[cfg(all(feature = "tokio-runtime", not(feature = "embassy-runtime")))]
pub use tokio_transport::{SerialClient, SerialDialer, SerialListener, SerialServer};

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use embassy_transport::{
    SerialClient as EmbassySerialClient, SerialServer as EmbassySerialServer,
};
#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_transport::{
    SerialClient as TokioSerialClient, SerialDialer, SerialListener,
    SerialServer as TokioSerialServer,
};

#[cfg(all(feature = "embassy-runtime", not(feature = "tokio-runtime")))]
pub use embassy_transport::{SerialClient, SerialServer};
