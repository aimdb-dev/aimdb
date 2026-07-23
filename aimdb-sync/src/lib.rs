//! # AimDB Sync API
//!
//! Synchronous API wrapper for AimDB that enables blocking operations
//! on the async database. Perfect for FFI, legacy codebases, and simple scripts.
//!
//! ## Overview
//!
//! This crate provides a synchronous interface to AimDB by running the
//! async runtime on a dedicated background thread and using channels
//! to bridge between synchronous and asynchronous contexts.
//!
//! ## Features
//!
//! ### Producer Operations
//! - **`set()`**: Blocking send, waits if channel is full
//! - **`set_timeout()`**: Blocking send with timeout
//! - **`try_set()`**: Non-blocking send, returns immediately
//!
//! ### Consumer Operations
//! - **`get()`**: Blocking receive, waits for value
//! - **`get_timeout()`**: Blocking receive with timeout
//! - **`try_get()`**: Non-blocking receive, returns immediately
//!
//! ### General
//! - **Thread-Safe**: All types are `Send + Sync` and can be shared across threads
//! - **Type-Safe**: Full compile-time type safety with generics
//! - **Pure Sync Context**: No `#[tokio::main]` required - works in plain `fn main()`
//!
//! ## Architecture
//!
//! ```text
//! User Threads (sync)  →  Channels  →  Runtime Thread (async)
//!                                        ↓
//!                                     AimDB (async)
//!                                        ↓
//!                                    Buffers (SPMC, etc.)
//!                                        ↓
//!                                     Channels  →  Consumer Threads (sync)
//! ```
//!
//! The runtime thread is created automatically when you call `attach()` on the builder.
//! It stays alive until `detach()` is called or the handle is dropped.
//!
//! ## Quick Start
//!
//! ```no_run
//! use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
//! use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! use aimdb_sync::{AimDbBuilderSyncExt, SyncResult};
//! use std::sync::Arc;
//!
//! #[derive(Debug, Clone)]
//! struct Temperature {
//!     celsius: f32,
//! }
//! // Guard againts the use of TokioAdapter in case of "std"
//! # #[cfg(feature = "std")]
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
//! let consumer = handle.consumer::<Temperature>("sensor.temp")?;
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
//! Both `SyncProducer` and `SyncConsumer` can be cloned and shared across threads:
//!
//! ```no_run
//! use std::thread;
//! # use aimdb_sync::{SyncConsumer, SyncProducer};
//! # #[derive(Debug, Clone)] struct Temperature { celsius: f32 }
//! # fn demo(producer: SyncProducer<Temperature>, consumer: SyncConsumer<Temperature>) {
//!
//! // Clone for use in another thread
//! let producer_clone = producer.clone();
//! let consumer_clone = consumer.clone();
//!
//! thread::spawn(move || {
//!     producer_clone.set(Temperature { celsius: 22.0 }).ok();
//! });
//!
//! thread::spawn(move || {
//!     if let Ok(temp) = consumer_clone.get() {
//!         println!("Got: {:.1}°C", temp.celsius);
//!     }
//! });
//! # }
//! ```
//!
//! ## Independent Subscriptions
//!
//! Note: Cloning a `SyncConsumer` shares the same channel, so only one thread
//! will receive each value. For independent subscriptions, create multiple consumers:
//!
//! ```no_run
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
//! ## Channel Capacity Configuration
//!
//! By default, both producers and consumers use a channel capacity of 100.
//! You can customize this per record type using the `_with_capacity` methods:
//!
//! ```no_run
//! # use aimdb_sync::{AimDbHandle, SyncResult};
//! # #[derive(Debug, Clone)] struct SensorData { value: f32 }
//! # #[derive(Debug, Clone)] struct RareEvent { code: u8 }
//! # #[derive(Debug, Clone)] struct LatestOnly { state: u8 }
//! # fn demo(handle: &AimDbHandle) -> SyncResult<()> {
//! // High-frequency sensor data needs larger buffer
//! let producer = handle.producer_with_capacity::<SensorData>("sensor.fast", 1000)?;
//!
//! // Rare events can use smaller buffer
//! let consumer = handle.consumer_with_capacity::<RareEvent>("events.rare", 10)?;
//!
//! // SingleLatest-like behavior: use capacity=1 to minimize queueing
//! let consumer = handle.consumer_with_capacity::<LatestOnly>("state.latest", 1)?;
//! # Ok(())
//! # }
//! ```
//!
//! **When to adjust capacity:**
//! - **Increase**: High-frequency data, bursty traffic, slow consumers
//! - **Decrease**: Memory-constrained, rare events, strict backpressure needed
//! - **Capacity=1**: Approximate SingleLatest semantics (see limitation below)
//! - **Default (100)**: Good for most use cases
//!
//! ## Buffer Semantics Limitation
//!
//! **Important**: The sync API adds a queueing layer (`std::sync::mpsc` channel)
//! between the database buffer and your code. This means:
//!
//! - ✅ **SPMC Ring**: Works as expected - each consumer gets independent data
//! - ✅ **Mailbox**: Works well - last value is preserved
//! - ⚠️ **SingleLatest**: Best effort only - the sync channel may queue multiple values
//!
//! ### Solutions for SingleLatest Semantics
//!
//! 1. **Use `get_latest()`** - Drains the channel to get the most recent value:
//!    ```no_run
//!    # use aimdb_sync::SyncResult;
//!    # #[derive(Debug, Clone)] struct Temperature { celsius: f32 }
//!    # fn demo(consumer: &aimdb_sync::SyncConsumer<Temperature>) -> SyncResult<()> {
//!    // Always get the latest value, skipping queued intermediates
//!    let latest = consumer.get_latest()?;
//!    # Ok(())
//!    # }
//!    ```
//!
//! 2. **Use capacity=1** - Minimize queueing:
//!    ```no_run
//!    # #[derive(Debug, Clone)] struct Temperature { celsius: f32 }
//!    # fn demo(handle: &aimdb_sync::AimDbHandle) -> aimdb_sync::SyncResult<()> {
//!    let consumer = handle.consumer_with_capacity::<Temperature>("sensor.temp", 1)?;
//!    # Ok(())
//!    # }
//!    ```
//!
//! 3. **Use the async API directly** - For perfect semantic preservation.
//!
//! The sync API is optimized for simplicity and ease of use, not for perfect
//! semantic preservation across all buffer types.
//!
//! ## Threading Model
//!
//! - **User threads**: Unlimited - any number of threads can call operations concurrently
//! - **Runtime thread**: One dedicated thread named "aimdb-sync-runtime"
//! - **Channels**: Lock-free MPSC channels for efficient communication
//!
//! ## Performance
//!
//! - **Overhead**: ~100-500μs per operation vs pure async (channel + context switch)
//! - **Throughput**: Limited by channel capacity (default: 100 items)
//! - **Latency**: Excellent for <50ms target, not suitable for hard low-latency requirements
//!
//! ## Error Handling
//!
//! All operations return [`SyncResult<T>`] with facade-specific [`SyncError`]
//! variants:
//!
//! - `RuntimeShutdown`: The runtime thread stopped
//! - `SetTimeout`: Producer timeout expired
//! - `GetTimeout`: Consumer timeout expired or no data (try_get)
//! - `AttachFailed`: Failed to start runtime thread
//! - `DetachFailed`: Failed to stop runtime thread
//! - `Db(DbError)`: Any error from the underlying database (e.g. record not
//!   registered), wrapped unchanged
//!
//! ### Error Propagation
//!
//! Producer errors are propagated synchronously back to the caller:
//! - `set()` and `set_with_timeout()` block until the produce operation completes
//!   and return any errors that occur in the async context
//! - `try_set()` sends immediately without waiting for the produce result (fire-and-forget)
//!
//! ```no_run
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
//! All types are thread-safe and can be shared across threads via `Clone`.
//! The API ensures proper resource cleanup through RAII and explicit `detach()`.

#![warn(missing_docs)]
#![warn(clippy::all)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

mod consumer;
mod error;
mod handle;
mod producer;

pub use consumer::SyncConsumer;
pub use handle::{AimDbBuilderSyncExt, AimDbHandle, AimDbSyncExt, DEFAULT_SYNC_CHANNEL_CAPACITY};
pub use producer::SyncProducer;

pub use error::{SyncError, SyncResult};

// Re-export the database error types for matching on [`SyncError::Db`].
pub use aimdb_core::{DbError, DbResult};
