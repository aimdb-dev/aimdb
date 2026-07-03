//! AimX codec + dispatch — the concrete protocol substrate the session engines
//! ride for AimX remote access.
//!
//! - [`AimxCodec`] — the symmetric NDJSON [`EnvelopeCodec`](crate::session::EnvelopeCodec),
//!   `no_std + alloc` (features `connector-session` + `remote`); used by
//!   both the `run_client` and `serve` engines.
//! - [`AimxDispatch`] — the server method semantics, `no_std + alloc` (features
//!   `connector-session` + `remote`); it reaches into core's
//!   `record.list` / JSON API, which are gated on `remote` too.
//!
//! The transport (UDS) lives in a separate connector crate
//! (`aimdb-uds-connector`); core keeps only the protocol plus the generic
//! [`SessionClientConnector`](crate::session::SessionClientConnector) /
//! [`SessionServerConnector`](crate::session::SessionServerConnector) spine.

mod codec;
pub use codec::AimxCodec;

#[cfg(feature = "remote")]
mod dispatch;
#[cfg(feature = "remote")]
pub use dispatch::AimxDispatch;
