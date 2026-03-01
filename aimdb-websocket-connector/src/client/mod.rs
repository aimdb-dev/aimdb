//! WebSocket **client** connector — connects out to a remote WS server.
//!
//! This module provides [`WsClientConnectorBuilder`] which implements the
//! standard [`ConnectorBuilder<R>`][aimdb_core::ConnectorBuilder] trait.
//!
//! # URL Scheme
//!
//! Routes use the `ws-client://` scheme to distinguish from the server-side
//! `ws://` scheme:
//!
//! ```text
//! link_to("ws-client://sensors/temperature")   // push local data to remote
//! link_from("ws-client://config/threshold")     // receive remote data locally
//! ```
//!
//! # Architecture
//!
//! ```text
//! AimDB (local) ←─ WsClientConnector ──WebSocket──→ AimDB (remote server)
//!     │                                                    │
//!     ├─ link_to  → serialize → ClientMessage::Write  ───→│
//!     └─ link_from ← deserialize ← ServerMessage::Data ←──│
//! ```

mod builder;
mod connector;

pub use builder::WsClientConnectorBuilder;
pub use connector::WsClientConnectorImpl;
