//! # aimdb-websocket-connector
//!
//! First-class WebSocket connector for AimDB — real-time bidirectional streaming.
//!
//! This crate provides two connector modes controlled by feature flags:
//!
//! - **`server`** (default) — Accepts incoming WebSocket connections via an
//!   Axum-based HTTP/WS server. Use `link_to("ws://topic")` to push data to
//!   browser clients.
//!
//! - **`client`** — Connects *out* to a remote WebSocket server (powered by
//!   `tokio-tungstenite`). Use `link_to("ws-client://host/topic")` and
//!   `link_from("ws-client://host/topic")` for direct AimDB-to-AimDB sync
//!   without an intermediary broker.
//!
//! Both modes share the same wire protocol from [`aimdb_ws_protocol`].
//!
//! ## Server Quick Start
//!
//! ```rust,ignore
//! use aimdb_tokio_adapter::TokioAdapter;
//! use aimdb_websocket_connector::WebSocketConnector;
//!
//! let db = AimDbBuilder::new()
//!     .runtime(TokioAdapter::new())
//!     .with_connector(
//!         WebSocketConnector::new()
//!             .bind("0.0.0.0:8080")
//!             .path("/ws")
//!             .with_late_join(true),
//!     )
//!     .configure::<Temperature>(TempKey::Vienna, |reg| {
//!         reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
//!             .link_to("ws://sensors/temperature/vienna")
//!             .with_serializer(|t| serde_json::to_vec(t).map_err(Into::into))
//!             .finish();
//!     })
//!     .build().await?;
//! ```
//!
//! ## Client Quick Start
//!
//! ```rust,ignore
//! use aimdb_websocket_connector::WsClientConnector;
//!
//! let db = AimDbBuilder::new()
//!     .runtime(TokioAdapter::new())
//!     .with_connector(
//!         WsClientConnector::new("wss://cloud.example.com/ws"),
//!     )
//!     .configure::<Temperature>("sensors/temp", |reg| {
//!         reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
//!             .link_to("ws-client://sensors/temp")
//!             .with_serializer(|t| serde_json::to_vec(t).map_err(Into::into))
//!             .finish()
//!             .link_from("ws-client://config/threshold")
//!             .with_deserializer(|data| serde_json::from_slice(data))
//!             .finish();
//!     })
//!     .build().await?;
//! ```
//!
//! ## Wire Protocol
//!
//! See [`protocol`] for the full message specification (re-exported from
//! [`aimdb_ws_protocol`]).
//!
//! ## Authentication (server only)
//!
//! See [`auth`] for the [`AuthHandler`][auth::AuthHandler] trait.

// ════════════════════════════════════════════════════════════════════
// Server modules (feature = "server")
// ════════════════════════════════════════════════════════════════════

#[cfg(feature = "server")]
pub mod auth;
#[cfg(feature = "server")]
pub mod builder;
#[cfg(feature = "server")]
pub mod client_manager;
#[cfg(feature = "server")]
pub mod connector;
#[cfg(feature = "server")]
pub(crate) mod server;
#[cfg(feature = "server")]
pub(crate) mod session;

// ════════════════════════════════════════════════════════════════════
// Client module (feature = "client")
// ════════════════════════════════════════════════════════════════════

#[cfg(feature = "client")]
pub mod client;

// ════════════════════════════════════════════════════════════════════
// Protocol (always available)
// ════════════════════════════════════════════════════════════════════

pub mod protocol;

// ════════════════════════════════════════════════════════════════════
// Public re-exports
// ════════════════════════════════════════════════════════════════════

/// The primary entry point for a WebSocket **server** connector.
///
/// This is a type alias for [`builder::WebSocketConnectorBuilder`].
#[cfg(feature = "server")]
pub type WebSocketConnector = builder::WebSocketConnectorBuilder;

#[cfg(feature = "server")]
pub use auth::{AuthError, AuthHandler, AuthRequest, ClientId, ClientInfo, NoAuth, Permissions};
#[cfg(feature = "server")]
pub use client_manager::ClientManager;

/// The primary entry point for a WebSocket **client** connector.
///
/// This is a type alias for [`client::WsClientConnectorBuilder`].
#[cfg(feature = "client")]
pub type WsClientConnector = client::WsClientConnectorBuilder;

pub use protocol::{ClientMessage, ErrorCode, ServerMessage};
