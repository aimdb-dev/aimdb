//! # AimDB Sync API
//!
//! Synchronous API wrapper for AimDB that enables blocking operations
//! on the async database. Perfect for FFI, legacy codebases, and simple scripts.
//!
//! ## Overview
//!
//! This crate provides a synchronous interface to AimDB by running the
//! async runtime on a dedicated background thread, blocking on it directly
//! for reads that must wait for data.
//!
//! ## Features
//!
//! ### Producer Operations
//! - **`set()`**: Blocking send, waits if channel is full
//! - **`try_set()`**: Non-blocking send, returns immediately
//!
//! ### Consumer Operations
//! - **`get()`**: Blocking receive, waits for value
//! - **`get_with_timeout()`**: Blocking receive with timeout
//! - **`try_get()`**: Non-blocking receive, returns immediately
//!
//! ### General
//! - **Thread-Safe**: `SyncProducer` is `Send + Sync` and can be cloned and shared across
//!   threads; `SyncConsumer` is `Send` only — move it to a thread, don't share it
//! - **Type-Safe**: Full compile-time type safety with generics
//! - **Pure Sync Context**: No `#[tokio::main]` required - works in plain `fn main()`
//!
//! ## Architecture
//!
//! ```text
//! User Threads (sync)  →  Runtime Thread (async)
//!                                 ↓
//!                         AimDB (async)
//!                                 ↓
//!                         Buffers (SPMC, etc.)
//!                                 ↓
//!                         Consumer Threads (sync)
//! ```
//!
//! The runtime thread is created automatically when you call `attach()` on the builder.
//! It stays alive until [`AimDbHandle::shutdown`] — or [`AimDbHandle::detach`],
//! which is the same call for an owner that has the handle by value — or until
//! the handle is dropped.
//!
//! ## Quick Start
//!
#![cfg_attr(feature = "std", doc = "```no_run")]
#![cfg_attr(not(feature = "std"), doc = "```ignore")]
//! use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
//! use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
//! use std::sync::Arc;
//!
//! #[derive(Debug, Clone)]
//! struct Temperature {
//!     celsius: f32,
//! }
//! # fn main() -> SyncResult<()> {
//! // Build and attach database (NO #[tokio::main] NEEDED!)
//! let adapter = Arc::new(TokioAdapter::new()?);
//! let mut builder = AimDbBuilder::new().runtime(adapter);
//!
//! builder.configure::<Temperature>("sensor.temp", |reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 10 });
//! });
//!
//! let handle = builder.attach()?;
//!
//! // Create producer and consumer
//! let producer = handle.producer::<Temperature>("sensor.temp")?;
//! let mut consumer = handle.consumer::<Temperature>("sensor.temp")?;
//!
//! // Producer: blocking operations
//! producer.set(Temperature { celsius: 25.0 })?;
//!
//! // Consumer: blocking operations
//! let temp = consumer.get()?;
//! println!("Temperature: {:.1}°C", temp.celsius);
//!
//! // Clean shutdown
//! handle.detach()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Multi-threaded Usage
//!
//! `SyncProducer` can be cloned and shared across threads:
//!
#![cfg_attr(feature = "std", doc = "```no_run")]
#![cfg_attr(not(feature = "std"), doc = "```ignore")]
//! use std::thread;
//! # use aimdb_sync::{SyncConsumer, SyncProducer};
//! # #[derive(Debug, Clone)] struct Temperature { celsius: f32 }
//! # fn demo(producer: SyncProducer<Temperature>, mut consumer: SyncConsumer<Temperature>) {
//!
//! // Clone for use in another thread
//! let producer_clone = producer.clone();
//!
//! thread::spawn(move || {
//!     producer_clone.set(Temperature { celsius: 22.0 }).ok();
//! });
//!
//! if let Ok(temp) = consumer.get() {
//!     println!("Got: {:.1}°C", temp.celsius);
//! };
//! # }
//! ```
//!
//! ## Independent Subscriptions
//!
//! For independent subscriptions, create multiple consumers:
//!
#![cfg_attr(feature = "std", doc = "```no_run")]
#![cfg_attr(not(feature = "std"), doc = "```ignore")]
//! # use aimdb_sync::{AimDbHandle, SyncResult};
//! # #[derive(Debug, Clone)] struct Temperature { celsius: f32 }
//! # fn demo(handle: &AimDbHandle) -> SyncResult<()> {
//! let consumer1 = handle.consumer::<Temperature>("sensor.temp")?;
//! let consumer2 = handle.consumer::<Temperature>("sensor.temp")?;
//!
//! // Both receive independent copies of all values
//! # Ok(())
//! # }
//! ```
//!
//! ## Threading Model
//!
//! - **User threads**: Unlimited - any number of threads can call operations concurrently
//! - **Runtime thread**: One dedicated thread named "aimdb-sync-runtime"
//!
//! ## Performance
//!
//! - **Latency**: Excellent for <50ms target, not suitable for hard low-latency requirements
//!
//! ## Error Handling
//!
//! All operations return [`SyncResult<T>`] with facade-specific [`SyncError`]
//! variants:
//!
//! - `RuntimeShutdown`: The runtime thread stopped
//! - `ForkedChild`: Created before a `fork()`, and this is the child — the
//!   runtime thread it needs did not survive, so a `set()` that would have
//!   returned `Ok` into a buffer nobody drains is refused instead (std, Unix)
//! - `SetTimeout`: Producer timeout expired
//! - `GetTimeout`: Consumer timeout expired or no data (try_get)
//! - `AttachFailed`: Failed to start runtime thread, carrying the `DbError`
//!   that caused it — so `kind()` reports a bad record graph as
//!   `Configuration` rather than as an internal fault
//! - `DetachFailed`: Failed to stop runtime thread
//! - `Db(DbError)`: Any error from the underlying database (e.g. record not
//!   registered), wrapped unchanged
//!
//! ### Error Propagation
//!
//! Producer errors are propagated synchronously back to the caller:
//! - `set()` blocks until the produce operation completes and returns any errors
//!   that occur
//! - `try_set()` returns immediately: `Ok(())` if the record's buffer accepted the
//!   value, `SyncError::SetTimeout` if it didn't (bounded, non-overwriting buffer, full)
//!
#![cfg_attr(feature = "std", doc = "```no_run")]
#![cfg_attr(not(feature = "std"), doc = "```ignore")]
//! # use aimdb_sync::{DbError, SyncError, SyncProducer};
//! # use aimdb_core::{log_error};
//! # #[derive(Debug, Clone)] struct Temperature { celsius: f32 }
//! # fn demo(producer: &SyncProducer<Temperature>, data: Temperature) {
//! // Errors are properly propagated to the caller
//! match producer.set(data) {
//!     Ok(()) => println!("Successfully produced"),
//!     Err(SyncError::Db(DbError::RecordKeyNotFound { .. })) => log_error!("Record not registered"),
//!     Err(e) => log_error!("Production failed: {}", e),
//! }
//! # }
//! ```
//!
//! ## Safety
//!
//! `SyncProducer` is `Clone`, `Send + Sync` — share it freely across threads.
//! `SyncConsumer` is `Send` only, not `Clone` — it is a subscription with its own
//! cursor, so move it to a thread and give each thread its own via a separate
//! `handle.consumer()` call. Every consumer then sees every value. To *split* one
//! stream across workers instead, share one as `Arc<Mutex<SyncConsumer<T>>>`.
//! The API ensures proper resource cleanup through RAII and an explicit
//! [`AimDbHandle::shutdown`].
//!
//! ## Shutting down from another language
//!
//! Four properties make a binding possible, and none of them is visible in a
//! signature — so each is pinned by `tests/shutdown_contract_test.rs` rather
//! than remembered:
//!
//! 1. [`AimDbHandle::shutdown`] takes `&self`. A C ABI's free function and a
//!    `#[pymethods]` method never receive `self` by value, and a `&mut self`
//!    shutdown collides at run time with a publish already in flight.
//! 2. It is idempotent, so a `with` block ending inside a signal handler is not
//!    an error.
//! 3. It is safe to call while producers publish: they reach the database
//!    through their own [`Weak`](alloc::sync::Weak) and never take the lock it
//!    holds.
//! 4. [`AimDbHandle::is_closed`] reads an atomic and never that lock, so a
//!    caller holding an interpreter lock may ask it while a shutdown is joining
//!    — including from aimdb's own runtime thread, where a logging bridge runs.
//!
//! A shutdown also releases the database, which closes the buffers and so wakes
//! a consumer parked in `get()`. Shut down first, then join your reader
//! threads.
//!
//! **A panic from this crate is a bug, not an error channel.** Every failure is
//! a [`SyncError`]. The blocking surface is compiled under
//! `deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)` so the
//! property is checked rather than remembered. This matters most to callers
//! reaching the crate from another language: a panic unwinding past an FFI
//! boundary is undefined behaviour, and a consumer whose profile sets
//! `panic = "abort"` turns any panic into the whole process dying.
//!
//! Two things this does not promise. `block_on` panics if called from inside a
//! Tokio runtime — the blocking surface must be entered from a thread that is
//! not already running one. And the guarantee stops at this crate's edge: a
//! dependency reached through it can still panic on its own.

#![warn(missing_docs)]
#![warn(clippy::all)]
// A panic on this surface is not an error channel, it is a bug. Callers reach
// this crate from other languages, where unwinding past the boundary is
// undefined behaviour and `panic = "abort"` in a consumer's profile turns any
// panic into the whole process dying. Checked rather than remembered.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "std")]
mod consumer;
mod error;
#[cfg(feature = "std")]
mod fork;
#[cfg(feature = "std")]
mod handle;
#[cfg(feature = "std")]
mod producer;
#[cfg(feature = "std")]
mod runtime;

#[cfg(feature = "std")]
pub use consumer::SyncConsumer;
#[cfg(feature = "std")]
pub use handle::{AimDbBuilderSyncExt, AimDbHandle, AimDbSyncExt};
#[cfg(feature = "std")]
pub use producer::SyncProducer;

pub use error::{SyncError, SyncResult};

// Re-export the database error types for matching on [`SyncError::Db`].
pub use aimdb_core::{DbError, DbResult};
