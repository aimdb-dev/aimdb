//! AimX-v2 codec + dispatch (the concrete substrate the shared session engine
//! rides for AimX remote access).
//!
//! Split by capability so the embedded *client* (a sensor dialing a gateway over
//! a real transport) gets the codec on `no_std + alloc`, while the *server*
//! dispatch stays `std`-gated until its own no_std port (Phase 6 follow-up):
//!
//! - [`AimxCodec`] — the symmetric NDJSON [`EnvelopeCodec`](crate::session::EnvelopeCodec),
//!   `no_std + alloc` (features `connector-session` + `json-serialize`). Both the
//!   proactive `run_client` and the reactive `serve` engine ride it.
//! - [`AimxDispatch`] — the server method semantics, **`std`-only** for now (it
//!   reaches into core's `record.list`/JSON API, which is still std-gated).
//!
//! The concrete **transport** (UDS dialer/listener + socket setup) no longer
//! lives here — a transport is a swappable connector crate (see
//! `aimdb-uds-connector`). Core keeps only the protocol (codec + dispatch) and
//! the generic [`SessionClientConnector`](crate::session::SessionClientConnector) /
//! [`SessionServerConnector`](crate::session::SessionServerConnector) spine.

mod codec;
pub use codec::AimxCodec;

#[cfg(feature = "std")]
mod dispatch;
#[cfg(feature = "std")]
pub use dispatch::AimxDispatch;
