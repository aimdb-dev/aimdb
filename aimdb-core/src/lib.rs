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
pub use context::{create_runtime_context, RuntimeContext};
pub use error::{DbError, DbResult};
pub use runtime::{
    AimDbService, DelayCapableAdapter, ExecutorError, ExecutorResult, RuntimeAdapter, RuntimeInfo,
    SpawnDynamically, SpawnStatically,
};
pub use time::{SleepCapable, TimestampProvider};

// Database implementation exports
pub use database::{Database, DatabaseSpec, DatabaseSpecBuilder, Record, Runnable};

// Re-export procedural macros
pub use aimdb_macros::service;

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
