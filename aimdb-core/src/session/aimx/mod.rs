//! AimX codec + dispatch — the concrete protocol substrate the session engines
//! ride for AimX remote access.
//!
//! - [`AimxCodec`] — the symmetric NDJSON [`EnvelopeCodec`](crate::session::EnvelopeCodec),
//!   `no_std + alloc` (features `connector-session` + `json-serialize`); used by
//!   both the `run_client` and `serve` engines.
//! - [`AimxDispatch`] — the server method semantics, `std`-only (it reaches into
//!   core's `record.list` / JSON API).
//!
//! The transport (UDS) lives in a separate connector crate
//! (`aimdb-uds-connector`); core keeps only the protocol plus the generic
//! [`SessionClientConnector`](crate::session::SessionClientConnector) /
//! [`SessionServerConnector`](crate::session::SessionServerConnector) spine.

mod codec;
pub use codec::AimxCodec;

#[cfg(feature = "std")]
mod dispatch;
#[cfg(feature = "std")]
pub use dispatch::AimxDispatch;
