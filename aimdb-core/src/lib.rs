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

pub mod builder;
pub mod context;
pub mod database;
pub mod emitter;
mod error;
pub mod metrics;
pub mod producer_consumer;
pub mod runtime;
pub mod time;
pub mod tracked_fn;
pub mod typed_record;

// Public API exports
pub use context::RuntimeContext;
pub use error::{DbError, DbResult};
pub use runtime::{
    ExecutorError, ExecutorResult, Logger, Runtime, RuntimeAdapter, RuntimeInfo, Sleeper, Spawn,
    TimeOps, TimeSource,
};

// Database implementation exports
pub use database::{Database, DatabaseSpec, DatabaseSpecBuilder};

// Producer-Consumer Pattern exports
pub use builder::{AimDb, AimDbBuilder};
pub use emitter::Emitter;
pub use metrics::CallStats;
pub use producer_consumer::{RecordRegistrar, RecordT};
pub use tracked_fn::TrackedAsyncFn;
pub use typed_record::{AnyRecord, AnyRecordExt, TypedRecord};
