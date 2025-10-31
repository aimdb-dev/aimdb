//! Remote access subsystem for AimDB (AimX protocol)
//!
//! Provides introspection and management APIs over Unix domain sockets,
//! enabling external tools (CLI, dashboards, MCP adapters) to interact
//! with running AimDB instances.
//!
//! # Protocol
//!
//! AimX v1 uses NDJSON (newline-delimited JSON) over Unix domain sockets.
//! See `docs/design/remote-access/aimx-v1.md` for full specification.
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
//! ```rust,ignore
//! use aimdb_core::remote::{AimxConfig, SecurityPolicy};
//!
//! let db = AimDbBuilder::new()
//!     .runtime(tokio_adapter)
//!     .with_remote_access(
//!         AimxConfig::uds_default()
//!             .socket_path("/var/run/aimdb/aimdb.sock")
//!             .security_policy(SecurityPolicy::ReadOnly)
//!             .max_connections(16)
//!             .subscription_queue_size(100)
//!     )
//!     .build()?;
//! ```

mod config;
mod error;
mod metadata;
mod protocol;

pub use config::{AimxConfig, SecurityPolicy};
pub use error::{RemoteError, RemoteResult};
pub use metadata::RecordMetadata;
pub use protocol::{Event, HelloMessage, Request, Response, WelcomeMessage};

// Internal exports for implementation
pub(crate) mod handler;
pub(crate) mod supervisor;
