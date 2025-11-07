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
//! ```rust,ignore
//! use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
//! use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! use aimdb_sync::AimDbBuilderSyncExt;
//! use serde::{Serialize, Deserialize};
//! use std::sync::Arc;
//!
//! #[derive(Debug, Clone, Serialize, Deserialize)]
//! struct Temperature {
//!     celsius: f32,
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Build and attach database (NO #[tokio::main] NEEDED!)
//! let adapter = Arc::new(TokioAdapter);
//! let mut builder = AimDbBuilder::new().runtime(adapter);
//!
//! builder.configure::<Temperature>(|reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 10 });
//! });
//!
//! let handle = builder.attach()?;
//!
//! // Create producer and consumer
//! let producer = handle.producer::<Temperature>()?;
//! let consumer = handle.consumer::<Temperature>()?;
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
//! ```rust,ignore
//! use std::thread;
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
//! ```
//!
//! ## Independent Subscriptions
//!
//! Note: Cloning a `SyncConsumer` shares the same channel, so only one thread
//! will receive each value. For independent subscriptions, create multiple consumers:
//!
//! ```rust,ignore
//! let consumer1 = handle.consumer::<Temperature>()?;
//! let consumer2 = handle.consumer::<Temperature>()?;
//!
//! // Both receive independent copies of all values
//! ```
//!
//! ## Channel Capacity Configuration
//!
//! By default, both producers and consumers use a channel capacity of 100.
//! You can customize this per record type using the `_with_capacity` methods:
//!
//! ```rust,ignore
//! // High-frequency sensor data needs larger buffer
//! let producer = handle.producer_with_capacity::<SensorData>(1000)?;
//!
//! // Rare events can use smaller buffer
//! let consumer = handle.consumer_with_capacity::<RareEvent>(10)?;
//!
//! // SingleLatest-like behavior: use capacity=1 to minimize queueing
//! let consumer = handle.consumer_with_capacity::<LatestOnly>(1)?;
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
//!    ```rust,ignore
//!    // Always get the latest value, skipping queued intermediates
//!    let latest = consumer.get_latest()?;
//!    ```
//!
//! 2. **Use capacity=1** - Minimize queueing:
//!    ```rust,ignore
//!    let consumer = handle.consumer_with_capacity::<T>(1)?;
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
//! All operations return `DbResult<T>` which wraps standard `DbError` variants:
//!
//! - `RuntimeShutdown`: The runtime thread stopped
//! - `SetTimeout`: Producer timeout expired
//! - `GetTimeout`: Consumer timeout expired or no data (try_get)
//! - `AttachFailed`: Failed to start runtime thread
//! - `DetachFailed`: Failed to stop runtime thread
//! - `RecordNotFound`: Attempted to produce/consume unregistered type
//! - Plus any other errors from the underlying `produce()` operation
//!
//! ### Error Propagation
//!
//! Producer errors are propagated synchronously back to the caller:
//! - `set()` and `set_with_timeout()` block until the produce operation completes
//!   and return any errors that occur in the async context
//! - `try_set()` sends immediately without waiting for the produce result (fire-and-forget)
//!
//! ```rust,ignore
//! // Errors are properly propagated to the caller
//! match producer.set(data) {
//!     Ok(()) => println!("Successfully produced"),
//!     Err(DbError::RecordNotFound { .. }) => eprintln!("Type not registered"),
//!     Err(e) => eprintln!("Production failed: {}", e),
//! }
//! ```
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
pub use handle::{AimDbBuilderSyncExt, AimDbHandle, AimDbSyncExt, DEFAULT_SYNC_CHANNEL_CAPACITY};
pub use producer::SyncProducer;

// Re-export error types from aimdb-core
pub use aimdb_core::{DbError, DbResult};
