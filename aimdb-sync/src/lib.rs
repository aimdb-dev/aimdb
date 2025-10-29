//! # AimDB Sync API
//!
//! Synchronous API wrapper for AimDB that enables blocking operations
//! on the async database.
//!
//! ## Overview
//!
//! This crate provides a synchronous interface to AimDB by running the
//! async runtime on a dedicated background thread and using channels
//! to bridge between synchronous and asynchronous contexts.
//!
//! ## Features
//!
//! - **Blocking Operations**: `set()` and `get()` methods that block until complete
//! - **Timeout Support**: `set_timeout()` and `get_timeout()` with configurable timeouts
//! - **Non-blocking Try**: `try_set()` and `try_get()` for opportunistic operations
//! - **Thread-Safe**: All types are `Send + Sync` and can be shared across threads
//! - **Type-Safe**: Full compile-time type safety with generics
//! - **Zero-Copy**: Values are moved (not cloned) through channels
//!
//! ## Architecture
//!
//! ```text
//! User Threads (sync)  →  Channels  →  Runtime Thread (async)
//!                                        ↓
//!                                     AimDB (async)
//! ```
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
//! use aimdb_tokio_adapter::TokioAdapter;
//! use aimdb_sync::AimDbBuilderSyncExt;  // Import the extension trait
//! use serde::{Serialize, Deserialize};
//! use std::sync::Arc;
//!
//! #[derive(Debug, Clone, Serialize, Deserialize)]
//! struct Temperature {
//!     celsius: f32,
//! }
//!
//! // Configure Temperature as a record type (not shown - see aimdb-core docs)
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Build and attach database using builder pattern
//! let handle = AimDbBuilder::new()
//!     .runtime(Arc::new(TokioAdapter::new()?))
//!     .configure::<Temperature>(|reg| {
//!         reg.buffer(BufferCfg::SpmcRing { capacity: 10 });
//!         // Configure producers, consumers, connectors, etc.
//!     })
//!     .attach()?;  // Build and attach in one step!
//!
//! // Create producer and consumer
//! let producer = handle.producer::<Temperature>()?;
//! let consumer = handle.consumer::<Temperature>()?;
//!
//! // Use blocking operations
//! producer.set(Temperature { celsius: 25.0 })?;
//! let temp = consumer.get()?;
//!
//! // Clean shutdown
//! handle.detach()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Threading
//!
//! - **User threads**: Unlimited - any number of threads can call `set()`/`get()` concurrently
//! - **Runtime thread**: One dedicated thread runs the Tokio async runtime
//! - **Channels**: Lock-free MPSC channels bridge sync and async contexts
//!
//! ## Performance
//!
//! - **Target overhead**: <1ms per operation vs pure async
//! - **Zero-copy**: Values are moved through channels, not cloned
//! - **Lock-free**: Tokio channels use lock-free algorithms
//!
//! ## Safety
//!
//! All types are thread-safe and can be shared across threads via `Clone`.
//! The API ensures proper resource cleanup through RAII and explicit `detach()`.

#![warn(missing_docs)]
#![warn(clippy::all)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod consumer;
mod handle;
mod producer;

pub use consumer::SyncConsumer;
pub use handle::{AimDbBuilderSyncExt, AimDbHandle, AimDbSyncExt};
pub use producer::SyncProducer;

// Re-export error types from aimdb-core
pub use aimdb_core::{DbError, DbResult};
