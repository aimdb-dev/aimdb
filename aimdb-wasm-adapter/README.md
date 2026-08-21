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

## Architecture

The adapter is split into several focused modules:

| Module | Purpose |
|--------|---------|
| `runtime.rs` | `WasmAdapter` — the zero-sized adapter type |
| `time.rs` | `RuntimeOps` — `performance.now()`, `Date.now()`, `setTimeout` sleep, and `console.*` logging via `globalThis` (Window + Worker) |
| `buffer.rs` | `WasmBuffer<T>` — SPMC Ring, SingleLatest, Mailbox on `Rc<RefCell<…>>` |
| `schema_registry.rs` | `SchemaRegistry` — type-erased dispatch from a schema name to a `Streamable` type |
| `bindings.rs` | `WasmDb` — `#[wasm_bindgen]` facade: `configureRecord`, `get`, `set`, `subscribe`, `discover` |
| `ws_bridge.rs` | `WsBridge` — WebSocket bridge to remote AimDB server (AimX wire protocol) |

`bindings.rs`, `schema_registry.rs`, and `ws_bridge.rs` are `wasm32`-only — see
[Target support](#target-support).

## JavaScript / TypeScript API

### WasmDb

```typescript
import init, { WasmDb } from '@aimdb/aimdb-wasm-adapter';

await init();
const db = new WasmDb();

// Configure records with Rust data contracts
db.configureRecord('sensors.temperature.vienna', {
  schemaType: 'temperature',
  buffer: 'SingleLatest',
});

await db.build();

// Get (returns deserialized JS object validated by Rust serde)
const temp = db.get('sensors.temperature.vienna');
console.log(temp.celsius);

// Set (Rust serde validates the payload)
db.set('sensors.temperature.vienna', { celsius: 22.5, timestamp: Date.now() });

// Subscribe (callback fires on every buffer push)
const unsub = db.subscribe('sensors.temperature.vienna', (value) => {
  console.log('New temperature:', value.celsius);
});
```

### WsBridge

Connect the browser-local AimDB to a remote server:

```typescript
import { WsBridge } from '@aimdb/aimdb-wasm-adapter';

const bridge = WsBridge.connect(db, 'wss://api.example.com/ws', {
  subscribeTopics: ['sensors.#'],
  autoReconnect: true,
  lateJoin: true,
});

bridge.onStatusChange((status) => {
  console.log('Connection:', status); // 'Connected' | 'Reconnecting' | ...
});

// Delivery gaps: the server sent updates the mirror never received (a slow
// consumer overran the server-side buffer). Without this, a gap looks exactly
// like an idle producer.
bridge.onGap((topic, skipped) => {
  console.warn(`${topic}: lost ${skipped} update(s)`);
});
bridge.droppedUpdates(); // cumulative count since connect

bridge.write('commands.setpoint', { target: 21.0 });
bridge.disconnect();
```

### React

There is no React entry point in this package and never was: `wasm-pack` packs
only `pkg/`, so no `.tsx` reaches the artifact. Build hooks over `WasmDb` /
`WsBridge` in your own app — four details are worth getting right, and each is
there because its absence produced a real bug: a `cancelled` guard against
StrictMode double mounts, refs rather than state for the cleanup closure,
`bridge.disconnect()` before `db.free()`, and a not-ready fallback. See
`weather-mesh-client` for a worked implementation.

## Data Contract Enforcement

All `get` / `set` / `subscribe` calls go through the `Streamable` trait defined
in `aimdb-data-contracts`. `SchemaRegistry::register::<T>()` stores type-erased
closures per schema name, so a name arriving from JS or off the wire dispatches
to the right Rust type and gets serde validation:

```
TypeScript value  →  serde_wasm_bindgen  →  Rust T: Streamable  →  buffer push
```

Two rules follow from how the registry is keyed:

- **Register the newest type per schema name.** Entries key on `T::NAME` with no
  version component, so a v1 and a v2 of one contract collide and the last one
  wins — visible only as a browser rendering nothing. A colliding name trips a
  `debug_assert!`.
- **`Migratable` chains do not run in the browser.** Inbound payloads decode
  straight into `T`, not through `Linkable::from_bytes`. AimX is a normalized
  plane; migration belongs at a server's ingest boundary. So a browser's version
  tolerance comes from the server it mirrors, not the contracts crate beside it.

## Target support

With `wasm-runtime` (the default) enabled this crate builds for `wasm32-*` only:
the bridge holds `web_sys` closures across await points, so its futures are
`!Send` and rely on an `unsafe impl` sound only on single-threaded wasm. A host
build stops on one `compile_error!` saying so. For the portable buffer/runtime
unit tests, drop the feature:

```bash
cargo test -p aimdb-wasm-adapter --no-default-features --lib
```

## Build

```bash
# Install dependencies
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# Compile to WASM
wasm-pack build --target web --out-dir pkg

# Run headless browser tests
wasm-pack test --headless --chrome
```

From the workspace root (`make` targets):

```bash
make wasm            # Build WASM adapter
make wasm-test-deps  # Chrome + version-matched chromedriver (one-off)
make wasm-test       # Run the browser suite (CI: `wasm-browser-tests`)
make check           # Full workspace check — wasm32 `cargo check` only, no browser
```

`webdriver.json` passes `--no-sandbox` / `--disable-dev-shm-usage` to headless
Chrome. `wasm-bindgen-test-runner` does not set them itself and without them
the sandbox fails to start on CI images that restrict unprivileged user
namespaces.

## Feature Flags

| Feature | Default | Purpose |
|---------|---------|---------|
| `wasm-runtime` | ✅ | Full browser runtime (bindings, WsBridge, web-sys) — `wasm32` targets only |
| `alloc` | ✅ | Core buffer + record support (no_std compatible) |

## A note on the published npm package

`@aimdb/aimdb-wasm-adapter` exists on npm (0.1.1, 2026-03-10) and the name
overstates what it is. A `wasm-pack` artifact of this crate is never a generic
adapter — the `SchemaRegistry` is populated at build time, so the blob only
understands the schemas one application compiled in. A second browser app should
publish under its own scoped name rather than add versions here, and this
package should be deprecated before someone installs it expecting something
reusable. The examples above use the name because it is what resolves today.

## License

Apache-2.0
