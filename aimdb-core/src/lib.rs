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
pub mod runtime;
pub mod time;
pub mod transport;
pub mod typed_api;
pub mod typed_record;

// Re-export procedural macros
pub use aimdb_macros::service;

// Public API exports
pub use context::RuntimeContext;
pub use error::{DbError, DbResult};
pub use runtime::{
    ExecutorError, ExecutorResult, Logger, Runtime, RuntimeAdapter, RuntimeInfo, Sleeper, Spawn,
    TimeOps, TimeSource,
};

// Database implementation exports
pub use database::Database;

// Producer-Consumer Pattern exports
pub use builder::{AimDb, AimDbBuilder};
pub use transport::{Connector, ConnectorConfig, HttpConnector, KafkaConnector, PublishError};
pub use typed_api::{Consumer, Producer, RecordRegistrar, RecordT};
pub use typed_record::{AnyRecord, AnyRecordExt, TypedRecord};

// Connector Infrastructure exports
pub use connector::{ConnectorClient, ConnectorLink, ConnectorUrl, SerializeError};
