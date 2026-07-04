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
//! ```no_run
//! use aimdb_core::buffer::BufferCfg;
//! use aimdb_core::AimDbBuilder;
//! use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! use aimdb_websocket_connector::WebSocketConnector;
//! # use std::sync::Arc;
//! # #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
//! # struct Temperature { celsius: f32 }
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let mut builder = AimDbBuilder::new()
//!     .runtime(Arc::new(TokioAdapter::new()?))
//!     .with_connector(
//!         WebSocketConnector::new()
//!             .bind("0.0.0.0:8080")
//!             .path("/ws")
//!             .with_late_join(true),
//!     );
//! builder.configure::<Temperature>("sensors.temp.vienna", |reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
//!         .link_to("ws://sensors/temperature/vienna")
//!         .with_serializer(|_ctx, t: &Temperature| Ok(serde_json::to_vec(t).expect("serialize")))
//!         .finish();
//! });
//! let (db, runner) = builder.build().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Client Quick Start
//!
//! ```no_run
//! use aimdb_core::buffer::BufferCfg;
//! use aimdb_core::AimDbBuilder;
//! use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! use aimdb_websocket_connector::WsClientConnector;
//! # use std::sync::Arc;
//! # #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
//! # struct Temperature { celsius: f32 }
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let mut builder = AimDbBuilder::new()
//!     .runtime(Arc::new(TokioAdapter::new()?))
//!     .with_connector(
//!         WsClientConnector::new("wss://cloud.example.com/ws"),
//!     );
//! builder.configure::<Temperature>("sensors.temp", |reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
//!         .link_to("ws-client://sensors/temp")
//!         .with_serializer(|_ctx, t: &Temperature| Ok(serde_json::to_vec(t).expect("serialize")))
//!         .finish()
//!         .link_from("ws-client://config/threshold")
//!         .with_deserializer(|_ctx, data| {
//!             serde_json::from_slice::<Temperature>(data).map_err(|e| e.to_string())
//!         })
//!         .finish();
//! });
//! let (db, runner) = builder.build().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Wire Protocol
//!
//! See [`protocol`] for the full message specification (re-exported from
//! [`aimdb_ws_protocol`]).
//!
//! ## Authentication (server only)
//!
//! See [`auth`] for the [`AuthHandler`] trait.

// ════════════════════════════════════════════════════════════════════
// Server modules (feature = "server")
// ════════════════════════════════════════════════════════════════════

#[cfg(feature = "server")]
pub mod server;

// ════════════════════════════════════════════════════════════════════
// Client module (feature = "client")
// ════════════════════════════════════════════════════════════════════

#[cfg(feature = "client")]
pub mod client;

// ════════════════════════════════════════════════════════════════════
// Shared session-engine glue (server and/or client)
// ════════════════════════════════════════════════════════════════════

/// Per-connection WS-JSON `EnvelopeCodec` shared by the server (`run_session`)
/// and client (`run_client`) ports.
#[cfg(any(feature = "server", feature = "client"))]
pub mod codec;

/// WS transport adapters (`Connection`/`Dialer`) over a real WebSocket.
#[cfg(any(feature = "server", feature = "client"))]
pub mod transport;

// Real-socket integration tests live in `tests/e2e.rs` (black-box, public API).

// ════════════════════════════════════════════════════════════════════
// Protocol (always available)
// ════════════════════════════════════════════════════════════════════

pub mod protocol;

// ════════════════════════════════════════════════════════════════════
// Public re-exports
// ════════════════════════════════════════════════════════════════════

/// The primary entry point for a WebSocket **server** connector.
///
/// This is a type alias for [`server::builder::WebSocketConnectorBuilder`].
#[cfg(feature = "server")]
pub type WebSocketConnector = server::builder::WebSocketConnectorBuilder;

#[cfg(feature = "server")]
pub use server::auth::{
    AuthError, AuthHandler, AuthRequest, ClientId, ClientInfo, NoAuth, Permissions,
};
#[cfg(feature = "server")]
pub use server::client_manager::ClientManager;

/// The primary entry point for a WebSocket **client** connector.
///
/// This is a type alias for [`client::WsClientConnectorBuilder`].
#[cfg(feature = "client")]
pub type WsClientConnector = client::WsClientConnectorBuilder;

pub use protocol::{ClientMessage, ErrorCode, QueryRecord, ServerMessage};

#[cfg(feature = "server")]
pub use server::session::{NoQuery, QueryFuture, QueryHandler};
