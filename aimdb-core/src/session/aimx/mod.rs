//! AimX-v2 transport + codec (Phase 3, std-only) — the concrete substrate the
//! shared session engine rides for AimX remote access.
//!
//! Client-first scope: ships the dialing transport ([`UdsDialer`] +
//! [`UdsConnection`]) and the symmetric [`AimxCodec`] (both engine directions),
//! enough to drive `run_client`. The accepting `UdsListener` and the server-side
//! `Dispatch` land with the server port; the substrate here is role-neutral and
//! is reused by both sides.

mod codec;
mod transport;

pub use codec::AimxCodec;
pub use transport::{UdsConnection, UdsDialer};
