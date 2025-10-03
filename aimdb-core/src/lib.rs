//! AimDB Core Database Engine
//!
//! This crate provides the core database engine for AimDB, supporting async
//! in-memory storage with real-time synchronization across MCU → edge → cloud
//! environments.

#![cfg_attr(not(feature = "std"), no_std)]

mod error;
mod runtime;
pub mod time;

// Public API exports
pub use error::{DbError, DbResult};
pub use runtime::{DelayCapableAdapter, RuntimeAdapter};
pub use time::{SleepCapable, TimestampProvider};
