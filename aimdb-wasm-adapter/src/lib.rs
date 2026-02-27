//! AimDB WASM Runtime Adapter
//!
//! Provides a WebAssembly runtime adapter for AimDB, enabling the full
//! dataflow engine to run inside a web browser or any WASM host.
//!
//! # Architecture
//!
//! This crate implements the four executor traits from `aimdb-executor`:
//!
//! - [`RuntimeAdapter`] — Platform identity (`"wasm"`)
//! - [`Spawn`] — Task spawning via `wasm_bindgen_futures::spawn_local`
//! - [`TimeOps`] — `Performance.now()` + `setTimeout` for async sleep
//! - [`Logger`] — Maps to `console.log/debug/warn/error`
//!
//! # Single-Threaded Safety
//!
//! `wasm32-unknown-unknown` is single-threaded by construction. The `Send + Sync`
//! bounds required by executor traits are satisfied trivially — no concurrent
//! access is possible. This is the same pattern used by `aimdb-embassy-adapter`
//! for bare-metal MCUs.
//!
//! # Buffer Implementation
//!
//! Buffers use `Rc<RefCell<…>>` instead of atomics — zero-overhead for the
//! single-threaded browser environment. All three buffer types are supported:
//! SPMC Ring, SingleLatest, and Mailbox.
//!
//! # Feature Flags
//!
//! - `wasm-runtime` (default) — Enables WASM bindings (`wasm-bindgen`,
//!   `js-sys`, `web-sys`). Disable for native-target unit tests.

#![no_std]

extern crate alloc;

pub mod buffer;
pub mod logger;
pub mod runtime;
pub mod time;

#[cfg(feature = "wasm-runtime")]
pub mod bindings;

#[cfg(feature = "wasm-runtime")]
pub mod ws_bridge;

// Re-export the adapter type at crate root
pub use runtime::WasmAdapter;

// Re-export executor traits for convenience
pub use aimdb_executor::{
    ExecutorError, ExecutorResult, Logger as LoggerTrait, Runtime, RuntimeAdapter, Spawn, TimeOps,
};

// Re-export buffer types
pub use buffer::{WasmBuffer, WasmBufferReader};

// Re-export time types
pub use time::{WasmDuration, WasmInstant};

// Generate the extension trait for convenient record configuration
aimdb_core::impl_record_registrar_ext! {
    WasmRecordRegistrarExt,
    WasmAdapter,
    WasmBuffer,
    "wasm-runtime",
    |cfg| WasmBuffer::<T>::new(cfg)
}
