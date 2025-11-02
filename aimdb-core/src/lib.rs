//! AimDB Core Database Engine
//!
//! Type-safe, async in-memory database for real-time data synchronization
//! across MCU → edge → cloud environments.
//!
//! # Architecture
//!
//! - **Type-Safe Records**: `TypeId`-based routing, no string keys
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
#[cfg(feature = "std")]
pub mod remote;
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
pub use transport::{Connector, ConnectorConfig, PublishError};
pub use typed_api::{Consumer, Producer, RecordRegistrar, RecordT};
pub use typed_record::{AnyRecord, AnyRecordExt, TypedRecord};

// Connector Infrastructure exports
pub use connector::{ConnectorClient, ConnectorLink, ConnectorUrl, SerializeError};
