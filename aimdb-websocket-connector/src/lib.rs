//! # aimdb-websocket-connector
//!
//! First-class WebSocket connector for AimDB — real-time bidirectional streaming.
//!
//! Replaces the `tap()` + `tokio::sync::broadcast` workaround with a proper
//! [`ConnectorBuilder<R>`][aimdb_core::ConnectorBuilder] implementation that
//! integrates cleanly with the existing MQTT/KNX infrastructure.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use aimdb_tokio_adapter::TokioAdapter;
//! use aimdb_websocket_connector::WebSocketConnector;
//!
//! let runtime = TokioAdapter::new();
//!
//! let db = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(
//!         WebSocketConnector::new()
//!             .bind("0.0.0.0:8080")
//!             .path("/ws")
//!             .with_late_join(true),
//!     )
//!     .configure::<Temperature>(TempKey::Vienna, |reg| {
//!         reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
//!             .link_from("mqtt://sensors/vienna/temperature")
//!             .with_deserializer(Temperature::from_bytes)
//!             .finish()
//!             .link_to("ws://sensors/temperature/vienna")
//!             .with_serializer(|t: &Temperature| {
//!                 serde_json::to_vec(t).map_err(Into::into)
//!             })
//!             .finish();
//!     })
//!     .build()
//!     .await?;
//! ```
//!
//! ## Wire Protocol
//!
//! See [`protocol`] for the full message specification.
//!
//! ## Authentication
//!
//! See [`auth`] for the [`AuthHandler`][auth::AuthHandler] trait.

pub mod auth;
pub mod builder;
pub mod client_manager;
pub mod connector;
pub mod protocol;
pub(crate) mod server;
pub(crate) mod session;

// ════════════════════════════════════════════════════════════════════
// Public re-exports
// ════════════════════════════════════════════════════════════════════

/// The primary entry point — use this to create a WebSocket connector.
///
/// This is a type alias for [`builder::WebSocketConnectorBuilder`].
pub type WebSocketConnector = builder::WebSocketConnectorBuilder;

pub use auth::{AuthError, AuthHandler, AuthRequest, ClientId, ClientInfo, NoAuth, Permissions};
pub use client_manager::ClientManager;
pub use protocol::{ClientMessage, ErrorCode, ServerMessage};
