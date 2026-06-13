//! Remote access subsystem for AimDB (AimX protocol)
//!
//! Provides introspection and management APIs over Unix domain sockets,
//! enabling external tools (CLI, dashboards, MCP adapters) to interact
//! with running AimDB instances.
//!
//! # Protocol
//!
//! AimX v2 uses NDJSON (newline-delimited JSON) tagged frames over a session
//! transport (Unix domain sockets via `aimdb-uds-connector`, serial via
//! `aimdb-serial-connector`). The envelope codec lives in
//! [`crate::session::aimx`]; see `docs/design/remote-access-via-connectors.md`
//! for the architecture. The v2 wire is not backward-compatible with the
//! legacy AimX v1 framing.
//!
//! # Security
//!
//! - **Read-only by default**: No writes unless explicitly enabled
//! - **UDS permissions**: Primary security mechanism (file permissions)
//! - **Optional auth tokens**: Additional authentication layer
//! - **Per-record write permissions**: Explicit opt-in required
//!
//! # Usage
//!
//! Remote access is registered like any other connector — via `with_connector`
//! using `aimdb_uds_connector::UdsServer` (this replaced the former
//! `AimDbBuilder::with_remote_access(config)`):
//!
//! ```rust,ignore
//! use aimdb_core::remote::{AimxConfig, SecurityPolicy};
//! use aimdb_uds_connector::UdsServer;
//!
//! let config = AimxConfig::uds_default()
//!     .socket_path("/var/run/aimdb/aimdb.sock")
//!     .security_policy(SecurityPolicy::ReadOnly)
//!     .max_connections(16)
//!     .max_subs_per_connection(32);
//!
//! let (db, runner) = AimDbBuilder::new()
//!     .runtime(tokio_adapter)
//!     .with_connector(UdsServer::from_config(config))
//!     .build()
//!     .await?;
//! ```

mod config;
mod error;
mod metadata;
mod protocol;
mod query;

pub use config::{AimxConfig, SecurityPolicy};
pub use error::{RemoteError, RemoteResult};
pub use metadata::RecordMetadata;
pub use protocol::{
    ErrorObject, Event, HelloMessage, Request, Response, WelcomeMessage, PROTOCOL_VERSION,
};
pub use query::{QueryHandlerFn, QueryHandlerParams};

// Internal exports for implementation
#[cfg(feature = "connector-session")]
pub(crate) mod stream;
