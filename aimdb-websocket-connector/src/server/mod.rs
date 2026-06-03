//! Server-side WebSocket connector: accepts browser/client connections and
//! bridges records to them.
//!
//! The HTTP/WS accept loop is axum's ([`http`]); each upgraded socket is driven
//! by the shared `run_session` engine over the root [`codec`](crate::codec) /
//! [`transport`](crate::transport) substrate, with [`dispatch`] supplying the
//! subscribe/write/query semantics and [`client_manager`] the cross-connection
//! fan-out bus. The outbound data plane rides [`connector`]'s `WsBusSink` through
//! the core `pump_sink`.

pub mod auth;
pub mod builder;
pub mod client_manager;
pub(crate) mod connector;
pub(crate) mod dispatch;
pub(crate) mod http;
pub(crate) mod registry;
pub(crate) mod session;
