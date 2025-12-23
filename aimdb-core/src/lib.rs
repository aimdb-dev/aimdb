//! AimDB Core Database Engine
//!
//! # aimdb-core
//!
//! Type-safe, async in-memory database for data synchronization
//! across MCU → edge → cloud environments.
//!
//! # Architecture
//!
//! - **RecordKey/RecordId**: Stable identifiers for multi-instance records
//! - **Unified API**: Single `Database<A>` type for all operations
//! - **Runtime Agnostic**: Works with Tokio (std) or Embassy (embedded)
//! - **Producer-Consumer**: Built-in typed message passing
//!
//! See examples in the repository for usage patterns.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod buffer;
pub mod builder;
pub mod connector;
pub mod context;
pub mod database;
mod error;
pub mod ext_macros;
pub mod record_id;
#[cfg(feature = "std")]
pub mod remote;
pub mod router;
pub mod time;
pub mod transport;
pub mod typed_api;
pub mod typed_record;

// Public API exports
pub use context::RuntimeContext;
pub use error::{DbError, DbResult};

// Runtime trait re-exports from aimdb-executor
// These traits define the platform-specific execution, timing, and logging capabilities
pub use aimdb_executor::{
    ExecutorError, ExecutorResult, Logger, Runtime, RuntimeAdapter, RuntimeInfo, Spawn, TimeOps,
};

// Database implementation exports
pub use database::Database;

// Producer-Consumer Pattern exports
pub use builder::{AimDb, AimDbBuilder};
pub use connector::ConnectorBuilder;
pub use transport::{Connector, ConnectorConfig, PublishError};
pub use typed_api::{Consumer, Producer, RecordRegistrar, RecordT};
pub use typed_record::{AnyRecord, AnyRecordExt, TypedRecord};

// Connector Infrastructure exports
pub use connector::{ConnectorClient, ConnectorLink, ConnectorUrl, SerializeError};

// Router exports for connector implementations
pub use router::{Route, Router, RouterBuilder};

// Record identification exports
pub use record_id::{RecordId, RecordKey, StringKey};

// Re-export derive macro when feature is enabled
#[cfg(feature = "derive")]
pub use aimdb_derive::RecordKey as DeriveRecordKey;
