//! AimDB Client Library
//!
//! This library provides a client implementation for the AimX remote access
//! protocol, enabling connections to running AimDB instances via Unix domain
//! sockets or serial.
//!
//! ## Overview
//!
//! The client library offers:
//! - **Connection Management**: [`AimxConnection`] over the shared session engine
//! - **Protocol Implementation**: the reshaped AimX-v2 handshake + RPC/streaming
//! - **Instance Discovery**: Automatic detection of running AimDB instances
//! - **Record Operations**: list, get, set, subscribe, drain, graph, query
//!
//! ## Usage
//!
//! ```no_run
//! use aimdb_client::AimxConnection;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to an AimDB instance by endpoint (a bare path is the `unix://`
//!     // shorthand; `serial://DEVICE?baud=N` reaches a board). Performs `hello`.
//!     let conn = AimxConnection::connect("unix:///tmp/aimdb.sock").await?;
//!
//!     // List all records
//!     let records = conn.list_records().await?;
//!     println!("Found {} records", records.len());
//!
//!     // Get a specific record
//!     let value = conn.get_record("server::Temperature").await?;
//!     println!("Temperature: {:?}", value);
//!
//!     Ok(())
//! }
//! ```

// Instance discovery is a Unix-socket filesystem scan, so it rides the UDS
// transport feature.
#[cfg(feature = "transport-uds")]
pub mod discovery;
pub mod endpoint;
pub mod engine;
pub mod error;
pub mod protocol;

// Re-export main types for convenience. `AimxConnection` is the engine-based
// client (the synchronous `AimxClient` was retired with the AimX server port).
#[cfg(feature = "transport-uds")]
pub use discovery::{discover_instances, find_instance, InstanceInfo};
pub use endpoint::{dial, parse_endpoint, ParsedEndpoint, Scheme};
pub use engine::{AimxConnection, DrainResponse};
pub use error::{ClientError, ClientResult};
pub use protocol::{
    cli_hello, parse_message, serialize_message, Event, EventMessage, RecordMetadata, Request,
    RequestExt, Response, ResponseExt, WelcomeMessage, CLIENT_NAME, PROTOCOL_VERSION,
};
