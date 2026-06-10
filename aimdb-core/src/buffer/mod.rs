//! Runtime-agnostic buffer traits and configuration for async producer-consumer dispatch
//!
//! This module defines the buffer abstraction without runtime-specific implementations.
//! Actual buffer implementations are provided by adapter crates:
//! - `aimdb-tokio-adapter` - Tokio-based buffers (std environments)
//! - `aimdb-embassy-adapter` - Embassy-based buffers (embedded no_std)
//!
//! # Buffer Types
//!
//! Three buffering strategies are supported:
//! - **SPMC Ring**: Bounded backlog with per-consumer lag tolerance
//! - **SingleLatest**: Only newest value kept (no backlog)
//! - **Mailbox**: Single-slot with overwrite semantics
//!
//! # Design Philosophy
//!
//! Each record type can choose the appropriate buffer based on its data flow:
//! - High-frequency telemetry → SPMC ring
//! - Configuration state → SingleLatest
//! - Commands/triggers → Mailbox
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │           aimdb-core (trait layer)              │
//! │  BufferBackend<T> + BufferReader<T> + BufferCfg │
//! └────────────────┬────────────────────────────────┘
//!                  │
//!      ┌───────────┴───────────┐
//!      │                       │
//!      ▼                       ▼
//! ┌─────────────┐     ┌──────────────────┐
//! │ tokio impl  │     │ embassy impl     │
//! │ (std)       │     │ (no_std)         │
//! └─────────────┘     └──────────────────┘
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_core::buffer::BufferCfg;
//!
//! // High-frequency sensor data
//! reg.buffer(BufferCfg::SpmcRing { capacity: 2048 })
//!    .source(|em, data| async { ... })
//!    .tap(|em, data| async { ... });
//!
//! // Configuration updates
//! reg.buffer(BufferCfg::SingleLatest)
//!    .source(|em, cfg| async { ... })
//!    .tap(|em, cfg| async { ... });
//! ```

// Module structure
mod cfg;
#[cfg(feature = "metrics")]
mod counters;
mod traits;
mod writer;

// Public API exports
pub use cfg::BufferCfg;
pub use traits::{Buffer, BufferReader, DynBuffer};

// Crate-private — used by Producer<T> to push without per-call lookup
pub(crate) use traits::WriteHandle;
pub(crate) use writer::RecordWriter;

// JSON streaming support
#[cfg(feature = "remote-access")]
pub use traits::JsonBufferReader;

// Buffer metrics (feature-gated; works in no_std with portable-atomic)
#[cfg(feature = "metrics")]
pub use counters::BufferCounters;
#[cfg(feature = "metrics")]
pub use traits::{BufferMetrics, BufferMetricsSnapshot};

// Re-export buffer-specific errors from core error module
// These are type aliases for convenience
pub use crate::DbError as BufferError;

/// Result type for buffer operations
pub type BufferResult<T> = Result<T, crate::DbError>;
