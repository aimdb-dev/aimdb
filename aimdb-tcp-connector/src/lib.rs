//! Length-prefixed TCP transport connector for AimDB remote access.
//!
//! This crate contributes only the TCP transport triple plus thin
//! [`TcpClient`]/[`TcpServer`] sugar. AimX protocol bytes still come from
//! [`AimxCodec`](aimdb_core::session::aimx::AimxCodec), and the session engines
//! still live in `aimdb-core`.
//!
//! TCP is a byte stream, so the transport frames every AimX envelope as
//! `u32` big-endian payload length followed by the payload bytes. See
//! [`framing`].

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod framing;

// `TcpClient`/`TcpServer` over an adapter's stream transports.
#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub mod connector;

/// Default connector scheme.
///
/// Record links such as `link_to("tcp://record")` route through this scheme.
pub const DEFAULT_SCHEME: &str = "tcp";

/// Mark each record named in the policy's writable set as writable, so
/// `record.list` advertises the writable flag. The dispatch also enforces it.
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
pub use connector::{
    framed_dialer, framed_dialer_at, framed_listener, split_host_port, TcpClient, TcpServer,
    DEFAULT_PORT,
};
