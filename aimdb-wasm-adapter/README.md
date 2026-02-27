# aimdb-wasm-adapter

WebAssembly runtime adapter for AimDB — browser-native async runtime support.

## Overview

This crate provides a WASM runtime adapter that enables the full AimDB dataflow
engine to run inside a web browser (or any `wasm32-unknown-unknown` host).

Records, buffers, producers, consumers, and data-contract enforcement all
execute natively in WASM — eliminating the need for a parallel validation
layer (Zod, JSON Schema) on the TypeScript side.

## Platform Matrix

| Target | Adapter | Buffer Primitive | Spawn Mechanism |
|--------|---------|------------------|-----------------|
| MCU | `aimdb-embassy-adapter` | `embassy-sync` channels | Static task pool |
| Edge / Cloud | `aimdb-tokio-adapter` | `tokio::sync` channels | `tokio::spawn` |
| **Browser** | **`aimdb-wasm-adapter`** | **`Rc<RefCell<…>>`** | **`spawn_local`** |

## Usage

```rust
use aimdb_wasm_adapter::WasmAdapter;
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use std::sync::Arc;

let adapter = WasmAdapter;
let db = AimDbBuilder::new()
    .runtime(Arc::new(adapter))
    .build()
    .unwrap();
```

## Build

```bash
# Install target
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# Build
wasm-pack build --target web --out-dir pkg

# Test
wasm-pack test --headless --chrome
```

## License

Apache-2.0
