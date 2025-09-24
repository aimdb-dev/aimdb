//! AimDB Core Database Engine
//!
//! This crate provides the core database engine for AimDB, supporting async
//! in-memory storage with real-time synchronization across MCU → edge → cloud
//! environments.

mod error;

// Public API exports
pub use error::{DbError, DbResult};
