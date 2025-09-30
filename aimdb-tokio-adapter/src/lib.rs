//! Tokio Adapter for AimDB
//!
//! This crate provides Tokio-specific extensions for AimDB, enabling the database
//! to run on standard library environments using the Tokio async runtime.
//!
//! # Features
//!
//! - **Tokio Integration**: Seamless integration with Tokio async executor
//! - **Timeout Support**: Comprehensive timeout handling with `tokio::time`
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
//! - Runtime error constructors for Tokio-specific failures
//! - Automatic conversions from tokio error types
//! - Rich error descriptions leveraging std formatting capabilities
//!
//! # Usage
//!
//! ```rust,no_run
//! use aimdb_core::DbError;
//! use aimdb_tokio_adapter::{TokioErrorSupport, TokioErrorConverter};
//! use std::time::Duration;
//!
//! // Create runtime-specific errors
//! let timeout_error = DbError::from_timeout_error(0x01, Duration::from_millis(5000));
//! let runtime_error = DbError::from_runtime_error(0x01, "Runtime not available");
//! let task_error = DbError::from_task_error(0x02, "Task cancelled");
//! let io_error = DbError::from_io_error(0x01, "Connection refused");
//!
//! // Use converter functions
//! let timeout = TokioErrorConverter::timeout_error(Duration::from_millis(1000));
//! let unavailable = TokioErrorConverter::runtime_unavailable();
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

mod error;

pub use error::{TokioErrorConverter, TokioErrorSupport};
