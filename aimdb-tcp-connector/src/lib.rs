//! Length-prefixed TCP transport connector for AimDB remote access.
//!
//! This crate contributes only the length-prefix framing plus thin
//! `TcpClient`/`TcpServer` sugar; the socket comes from an adapter
//! (`TokioNet`, `EmbassyNet`) through core's `StreamDialer`/`StreamListener`.
//! AimX protocol bytes still come from `AimxCodec`, and the session engines
//! still live in `aimdb-core`.
//!
//! Names above are unlinked on purpose: they exist only behind the `connector`
//! feature, and a link to them fails `cargo doc` on a build without it.
//!
//! TCP is a byte stream, so the transport frames every AimX envelope as
//! `u32` big-endian payload length followed by the payload bytes. See
//! [`framing`].

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod framing;

// `TcpClient`/`TcpServer` over an adapter's stream transports.
#[cfg(feature = "connector")]
pub mod connector;

/// Default connector scheme.
///
/// Record links such as `link_to("tcp://record")` route through this scheme.
pub const DEFAULT_SCHEME: &str = "tcp";

/// Mark each record named in the policy's writable set as writable, so
/// `record.list` advertises the writable flag. The dispatch also enforces it.
#[cfg(feature = "connector")]
pub(crate) fn apply_writable(db: &aimdb_core::AimDb, config: &aimdb_core::remote::AimxConfig) {
    for key in config.security_policy.writable_records() {
        if let Some(id) = db.inner().resolve_str(&key) {
            if let Some(storage) = db.inner().storage(id) {
                storage.set_writable_erased(true);
            }
        }
    }
}

#[cfg(feature = "connector")]
pub use connector::{
    framed_dialer, framed_dialer_at, framed_dialer_bounded, framed_listener,
    framed_listener_bounded, split_host_port, EndpointError, TcpClient, TcpServer, DEFAULT_PORT,
};
