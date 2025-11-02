//! AimDB Client Library
//!
//! This library provides a client implementation for the AimX v1 remote access protocol,
//! enabling connections to running AimDB instances via Unix domain sockets.
//!
//! ## Overview
//!
//! The client library offers:
//! - **Connection Management**: Async client for Unix domain socket communication
//! - **Protocol Implementation**: AimX v1 handshake and message handling
//! - **Instance Discovery**: Automatic detection of running AimDB instances
//! - **Record Operations**: List, get, set, subscribe to records
//!
//! ## Usage
//!
//! ```no_run
//! use aimdb_client::AimxClient;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to an AimDB instance
//!     let mut client = AimxClient::connect("/tmp/aimdb.sock").await?;
//!     
//!     // List all records
//!     let records = client.list_records().await?;
//!     println!("Found {} records", records.len());
//!     
//!     // Get a specific record
//!     let value = client.get_record("server::Temperature").await?;
//!     println!("Temperature: {:?}", value);
//!     
//!     Ok(())
//! }
//! ```

pub mod connection;
pub mod discovery;
pub mod error;
pub mod protocol;

// Re-export main types for convenience
pub use connection::AimxClient;
pub use discovery::{discover_instances, find_instance, InstanceInfo};
pub use error::{ClientError, ClientResult};
pub use protocol::{
    cli_hello, parse_message, serialize_message, Event, EventMessage, RecordMetadata, Request,
    RequestExt, Response, ResponseExt, WelcomeMessage, CLIENT_NAME, PROTOCOL_VERSION,
};
