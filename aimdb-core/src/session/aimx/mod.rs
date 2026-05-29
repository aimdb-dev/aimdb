//! AimX-v2 transport + codec + dispatch (Phase 3, std-only) — the concrete
//! substrate the shared session engine rides for AimX remote access.
//!
//! Role-neutral substrate ([`UdsConnection`] + the symmetric [`AimxCodec`]) plus
//! both sides: the dialing transport ([`UdsDialer`]) that drives `run_client`,
//! and the accepting transport ([`UdsListener`]) + server [`AimxDispatch`] that
//! [`build_aimx_server`] drives via `serve`.

mod codec;
mod dispatch;
mod transport;

pub use codec::AimxCodec;
pub use dispatch::{build_aimx_server, AimxDispatch};
pub use transport::{UdsConnection, UdsDialer, UdsListener};
