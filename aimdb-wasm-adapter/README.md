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
| `runtime.rs` | `WasmAdapter` — `RuntimeAdapter` + `Spawn` impl using `wasm_bindgen_futures::spawn_local` |
| `time.rs` | `TimeOps` — `performance.now()` + `setTimeout`-based sleep via `gloo-timers` |
| `logger.rs` | `Logger` — maps log levels to `console.log / debug / warn / error` |
| `buffer.rs` | `WasmBuffer<T>` — SPMC Ring, SingleLatest, Mailbox on `Rc<RefCell<…>>` |
| `bindings.rs` | `WasmDb` — `#[wasm_bindgen]` facade: `configureRecord`, `get`, `set`, `subscribe` |
| `ws_bridge.rs` | `WsBridge` — WebSocket bridge to remote AimDB server (AimX wire protocol) |
| `react/` | React hooks — `useRecord<T>`, `useSetRecord<T>`, `useBridge` |

## JavaScript / TypeScript API

### WasmDb

```typescript
import init, { WasmDb } from '@aimdb/wasm';

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
import { WsBridge } from '@aimdb/wasm';

const bridge = WsBridge.connect(db, 'wss://api.example.com/ws', {
  subscribeTopics: ['sensors/#'],
  autoReconnect: true,
  lateJoin: true,
});

bridge.onStatusChange((status) => {
  console.log('Connection:', status); // 'Connected' | 'Reconnecting' | ...
});

bridge.write('commands.setpoint', { target: 21.0 });
bridge.disconnect();
```

### React Hooks

```tsx
import { AimDbProvider, useRecord, useSetRecord, useBridge } from '@aimdb/wasm/react';

function App() {
  return (
    <AimDbProvider config={{
      records: [
        { key: 'sensors.temperature.vienna', schemaType: 'temperature', buffer: 'SingleLatest' },
      ],
      bridge: { url: 'wss://api.example.com/ws', subscribeTopics: ['sensors/#'] },
    }}>
      <Dashboard />
    </AimDbProvider>
  );
}

function Dashboard() {
  const temp = useRecord<Temperature>('sensors.temperature.vienna');
  if (!temp) return <p>Loading…</p>;
  return <span>{temp.celsius.toFixed(1)}°C</span>;
}
```

**Available hooks:**

| Hook | Returns | Purpose |
|------|---------|---------|
| `useRecord<T>(key)` | `T \| null` | Subscribe to record, re-render on updates |
| `useSetRecord<T>(key)` | `(value: T) => void` | Write to record with contract validation |
| `useAimDb()` | `WasmDb \| null` | Raw database access for advanced usage |
| `useBridge()` | `WsBridge \| null` | Connection status and bridge control |

## Data Contract Enforcement

All `get` / `set` / `subscribe` calls go through the `Streamable` trait
defined in `aimdb-data-contracts`. The `dispatch_streamable!` macro maps
schema type names to Rust types and performs serde validation:

```
TypeScript value  →  serde_wasm_bindgen  →  Rust T: Streamable  →  buffer push
```

Adding a new contract requires only one change: implement `Streamable` for
the new type in `aimdb-data-contracts` and add it to `dispatch_streamable!`.

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
make wasm        # Build WASM adapter
make wasm-test   # Run WASM tests
make check       # Full workspace check (includes WASM)
```

## Feature Flags

| Feature | Default | Purpose |
|---------|---------|---------|
| `wasm-runtime` | ✅ | Full browser runtime (bindings, WsBridge, web-sys) |
| `alloc` | ✅ | Core buffer + record support (no_std compatible) |

## License

Apache-2.0
