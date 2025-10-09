//! AimDB Core Database Engine
//!
//! This crate provides the core database engine for AimDB, supporting async
//! in-memory storage with real-time synchronization across MCU → edge → cloud
//! environments.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod context;
pub mod database;
mod error;
pub mod runtime;
pub mod time;

// Public API exports
pub use context::RuntimeContext;
pub use error::{DbError, DbResult};
pub use runtime::{
    ExecutorError, ExecutorResult, Logger, Runtime, RuntimeAdapter, RuntimeInfo, Spawn,
    Sleeper, TimeOps, TimeSource,
};

// Database implementation exports
pub use database::{Database, DatabaseSpec, DatabaseSpecBuilder, Record, Runnable};

/// Runs a database instance
///
/// This function provides a unified interface for running database instances
/// across different runtime environments.
///
/// # Arguments
/// * `db` - A database instance that implements the Runnable trait
///
/// # Example
/// ```rust,no_run
/// # async fn example(db: impl aimdb_core::Runnable) {
/// aimdb_core::run(db).await;
/// # }
/// ```
pub async fn run<DB: Runnable>(db: DB) {
    db.run().await
}
