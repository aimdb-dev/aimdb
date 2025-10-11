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
//!
//! - **Runtime Module**: Core async task spawning with `RuntimeAdapter`
//! - **Time Module**: Time-related capabilities like timestamps and sleep
//! - **Error Module**: Runtime error constructors and conversions
//! - Rich error descriptions leveraging std formatting capabilities
//!
//! See the repository examples for complete usage patterns.

// Tokio adapter always requires std
#[cfg(not(feature = "std"))]
compile_error!("tokio-adapter requires the std feature");

pub mod buffer;
pub mod database;
pub mod error;
pub mod outbox;
pub mod runtime;
pub mod time;

pub use buffer::TokioBuffer;
pub use error::{TokioErrorConverter, TokioErrorSupport};
pub use outbox::{create_outbox_channel, OutboxReceiver, OutboxSender, TokioSender};

#[cfg(feature = "tokio-runtime")]
pub use runtime::TokioAdapter;

#[cfg(feature = "tokio-runtime")]
pub use database::{
    TokioDatabase, TokioDatabaseBuilder, TokioDatabaseSpec, TokioDatabaseSpecBuilder,
};
