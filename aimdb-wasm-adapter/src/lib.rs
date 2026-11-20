//! AimDB WASM Runtime Adapter
//!
//! Provides a WebAssembly runtime adapter for AimDB, enabling the full
//! dataflow engine to run inside a web browser or any WASM host.
//!
//! # Architecture
//!
//! This crate implements `aimdb_core::RuntimeOps` — the one trait a runtime
//! adapter provides: identity (`"wasm"`), time (`Performance.now()` +
//! `setTimeout` sleep), and logging (`console.log/debug/warn/error`).
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
//! Buffers use `Rc<RefCell<…>>` in the single-threaded browser environment
//! and `Arc<Mutex<…>>` in native builds. All three buffer types are supported:
//! SPMC Ring, SingleLatest, and Mailbox.
//!
//! # Feature Flags
//!
//! - `wasm-runtime` (default) — Enables browser bindings, the WebSocket
//!   bridge, and browser-backed timing and logging on `wasm32`.
//!   Native builds use the portable buffer and runtime implementations.

#![no_std]

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
compile_error!(
    "The WASM adapter requires single-threaded execution. Disable the atomics target feature."
);

extern crate alloc;
#[cfg(not(target_arch = "wasm32"))]
extern crate std;

pub mod buffer;
pub mod runtime;
pub mod time;

#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
pub mod bindings;

#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
mod cancellation;

#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
pub(crate) mod schema_registry;

#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
pub mod ws_bridge;

// Re-export the adapter type at crate root
pub use runtime::WasmAdapter;

// Re-export buffer types
pub use buffer::{WasmBuffer, WasmBufferReader};

/// Buffer-construction extension for [`aimdb_core::RecordRegistrar`].
///
/// Buffer construction is the one genuinely adapter-specific registration
/// step — `source()` / `tap()` / `transform()` are inherent methods on the
/// registrar. This trait adds `.buffer(cfg)` backed by [`WasmBuffer`].
pub trait WasmRecordRegistrarExt<T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Configures a [`WasmBuffer`] from the given configuration.
    fn buffer(&mut self, cfg: aimdb_core::buffer::BufferCfg) -> &mut Self;
}

impl<T, S: aimdb_core::SchemaState> WasmRecordRegistrarExt<T>
    for aimdb_core::RecordRegistrar<'_, T, S>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn buffer(&mut self, cfg: aimdb_core::buffer::BufferCfg) -> &mut Self {
        use aimdb_core::buffer::Buffer;
        let buffer = alloc::boxed::Box::new(WasmBuffer::<aimdb_core::Stamped<T>>::new(&cfg));
        // Record the cfg so buffer_info() reports the real buffer
        // type/capacity for the dependency graph.
        self.buffer_with_cfg(buffer, cfg)
    }
}
