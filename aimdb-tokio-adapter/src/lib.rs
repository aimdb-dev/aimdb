//! Tokio Adapter for AimDB
//!
//! This crate provides Tokio-specific extensions for AimDB, enabling the database
//! to run on standard library environments using the Tokio async runtime.
//!
//! # Features
//!
//! - **Tokio Integration**: Seamless integration with Tokio async executor
//! - **Time Support**: Timestamp, sleep, and delayed task capabilities with `tokio::time`
//! - **Error Handling**: Tokio-specific error conversions and handling
//! - **Std Compatible**: Designed for environments with full standard library
//!
//! # Architecture
//!
//! Tokio is a std async runtime, so this adapter is designed for standard
//! environments and works with the std version of aimdb-core by default.
//!
//! The adapter extends AimDB's core functionality without requiring tokio
//! dependencies in the core crate. It provides:
//!
//! - **Runtime Module**: Core async task spawning with `RuntimeAdapter`
//! - **Time Module**: Time-related capabilities like timestamps and sleep
//! - **Error Module**: Runtime error constructors and conversions
//! - Rich error descriptions leveraging std formatting capabilities
//!
//! # Usage
//!
//! ```rust,no_run
//! use aimdb_tokio_adapter::TokioAdapter;
//! use aimdb_executor::{RuntimeAdapter, DelayCapableAdapter, ExecutorResult};
//! use aimdb_core::{DbResult, time::{SleepCapable, TimestampProvider}};
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> DbResult<()> {
//!     // Create adapter
//!     let adapter = TokioAdapter::new()?;
//!
//!     // Spawn async tasks (spawn_task expects DbResult)
//!     let result = adapter.spawn_task(async {
//!         Ok::<i32, aimdb_core::DbError>(42)
//!     }).await?;
//!
//!     // Use time capabilities
//!     let timestamp = adapter.now();
//!     adapter.sleep(Duration::from_millis(100)).await;
//!
//!     // Use delayed task spawning (expecting ExecutorResult)
//!     adapter.spawn_delayed_task(
//!         async { Ok::<(), aimdb_executor::ExecutorError>(()) },
//!         Duration::from_millis(500)
//!     ).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! # Error Code Ranges
//!
//! The adapter uses specific error code ranges for different runtime components:
//!
//! - **Runtime**: 0x7100-0x71FF  
//! - **Timeout**: 0x7200-0x72FF
//! - **Task**: 0x7300-0x73FF
//! - **I/O**: 0x7400-0x74FF

#[cfg(feature = "tokio-runtime")]
pub mod database;
pub mod error;
pub mod runtime;
pub mod time;

pub use error::{TokioErrorConverter, TokioErrorSupport};

#[cfg(feature = "tokio-runtime")]
pub use runtime::TokioAdapter;

#[cfg(feature = "tokio-runtime")]
pub use database::{
    TokioDatabase, TokioDatabaseBuilder, TokioDatabaseSpec, TokioDatabaseSpecBuilder,
};
