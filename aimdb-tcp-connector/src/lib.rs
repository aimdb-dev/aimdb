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

#[cfg(feature = "tokio-runtime")]
pub mod tokio_transport;

#[cfg(feature = "embassy-runtime")]
pub mod embassy_transport;

/// Default connector scheme.
///
/// Record links such as `link_to("tcp://record")` route through this scheme.
pub const DEFAULT_SCHEME: &str = "tcp";

/// Mark each record named in the policy's writable set as writable, so
/// `record.list` advertises the writable flag. The dispatch also enforces it.
#[cfg(feature = "tokio-runtime")]
pub(crate) fn apply_writable(db: &aimdb_core::AimDb, config: &aimdb_core::remote::AimxConfig) {
    for key in config.security_policy.writable_records() {
        if let Some(id) = db.inner().resolve_str(&key) {
            if let Some(storage) = db.inner().storage(id) {
                storage.set_writable_erased(true);
            }
        }
    }
}

#[cfg(all(feature = "tokio-runtime", not(feature = "embassy-runtime")))]
pub use tokio_transport::{TcpClient, TcpConnection, TcpDialer, TcpListener, TcpServer};

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use embassy_transport::{TcpClient as EmbassyTcpClient, TcpConnection as EmbassyTcpConnection};
#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_transport::{
    TcpClient as TokioTcpClient, TcpConnection as TokioTcpConnection, TcpDialer, TcpListener,
    TcpServer as TokioTcpServer,
};

#[cfg(all(feature = "embassy-runtime", not(feature = "tokio-runtime")))]
pub use embassy_transport::{TcpClient, TcpConnection};
