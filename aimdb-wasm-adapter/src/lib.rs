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
//! Buffers use `Rc<RefCell<…>>` instead of atomics — zero-overhead for the
//! single-threaded browser environment. All three buffer types are supported:
//! SPMC Ring, SingleLatest, and Mailbox.
//!
//! # Feature Flags
//!
//! - `wasm-runtime` (default) — Enables WASM bindings (`wasm-bindgen`,
//!   `js-sys`, `web-sys`). Disable for native-target unit tests.
//!
//! # Target Support
//!
//! With `wasm-runtime` on, this crate builds for `wasm32-*` only: the bridge
//! holds `web_sys` closures across await points, so its futures are `!Send` and
//! meet the engine's `Send` bounds only via an arch-gated `SendFuture`. Pass
//! `--target wasm32-unknown-unknown`, or `--no-default-features` for the
//! host-side buffer/runtime unit tests.

#![no_std]

// A host build with `wasm-runtime` on otherwise fails deep in the bridge with a
// wall of `E0277`s about `!Send` futures. Say why once, here, instead.
#[cfg(all(feature = "wasm-runtime", not(target_arch = "wasm32")))]
compile_error!(
    "aimdb-wasm-adapter's `wasm-runtime` feature builds only for wasm32 targets \
     (its web-sys bridge futures are !Send). Pass \
     `--target wasm32-unknown-unknown`, or `--no-default-features` for the \
     host-side buffer/runtime tests."
);

extern crate alloc;

pub mod buffer;
pub mod runtime;
pub mod time;

// The three JS-facing modules are additionally arch-gated: off wasm32 they are
// what produces the `!Send` wall, and leaving them out keeps the guard above as
// the only error a host build reports.
#[cfg(all(feature = "wasm-runtime", target_arch = "wasm32"))]
pub mod bindings;

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

impl<T> WasmRecordRegistrarExt<T> for aimdb_core::RecordRegistrar<'_, T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn buffer(&mut self, cfg: aimdb_core::buffer::BufferCfg) -> &mut Self {
        use aimdb_core::buffer::Buffer;
        let buffer = alloc::boxed::Box::new(WasmBuffer::<T>::new(&cfg));
        // Record the cfg so buffer_info() reports the real buffer
        // type/capacity for the dependency graph.
        self.buffer_with_cfg(buffer, cfg)
    }
}
