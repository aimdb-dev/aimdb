# Design: AimDB WASM Adapter

**Status:** 📋 Proposed  
**Milestone:** M12 — Browser Runtime  
**Revision:** 1 (2026-02-27)  
**Crate:** `aimdb-wasm-adapter` (open source, `aimdb` workspace)

---

## 1. Summary

Add a third runtime adapter (`aimdb-wasm-adapter`) that compiles AimDB to
WebAssembly, enabling the **full dataflow engine** to run inside a web browser
or any WASM host. Records, buffers, producers, consumers, and data-contract
enforcement all execute natively in WASM — eliminating the need for a parallel
validation layer (Zod, JSON Schema) on the TypeScript side.

This completes the platform matrix:

| Target | Adapter | Buffer Primitive | Spawn Mechanism |
|--------|---------|------------------|-----------------|
| MCU | `aimdb-embassy-adapter` | `embassy-sync` channels | Static task pool |
| Edge / Cloud | `aimdb-tokio-adapter` | `tokio::sync` channels | `tokio::spawn` |
| **Browser** | **`aimdb-wasm-adapter`** | **`Rc<RefCell<…>>`** | **`spawn_local`** |

---

## 2. Motivation

### 2.1 Problem: The Validation Gap

Today the TypeScript UI (`aimdb-ui`) consumes data from the WebSocket connector
with **zero runtime validation**. The defence layers are:

1. **ts-rs** generates TypeScript type definitions from Rust structs →
   compile-time only, erased at runtime.
2. **schema-registry.ts** exports Observable metadata (icons, units) →
   informational, not enforced.
3. `useWebSocketConnection.ts` does `JSON.parse(event.data)` and passes the
   result straight to React state → **any malformed message silently corrupts
   the UI**.

Approaches like Zod codegen would add runtime validation, but create a
**parallel type system** that must be kept in sync with the Rust source of
truth — the exact problem data contracts were designed to eliminate.

### 2.2 Solution: Run the Real Engine in the Browser

A WASM adapter means:

- **Contract enforcement is native** — the same `serde` deserialization and
  `Migratable` migration logic that runs on the server runs in the browser.
- **No parallel type system** — Rust structs compiled to WASM _are_ the
  validation layer. `wasm-bindgen` / `serde-wasm-bindgen` handle the
  Rust ↔ JS boundary.
- **Full buffer semantics** — SPMC Ring, SingleLatest, Mailbox work identically.
  The browser can run producers, consumers, and transforms locally.
- **Offline-first capability** — a local AimDB instance persists state even
  when the WebSocket connection is lost.
- **Two-way sync** — a browser AimDB can connect to the server's WebSocket
  connector as a client, receiving and sending records through the standard
  `link_from` / `link_to` mechanism.

### 2.3 Non-Goals (v1)

- **`wasm32-wasi` support** — target is `wasm32-unknown-unknown` (browser).
  WASI can be added later with its own feature flag.
- **SharedArrayBuffer / multi-threaded WASM** — v1 is single-threaded only,
  matching the Embassy pattern.
- **Persistence backend for WASM** — IndexedDB integration is a separate
  design. v1 uses in-memory buffers only.
- **Web Worker offloading** — all execution happens on the main thread. Worker
  support is a future optimisation.

---

## 3. Architecture

### 3.1 Crate Layout

```
aimdb-wasm-adapter/
├── Cargo.toml
├── README.md
├── src/
│   ├── lib.rs              # WasmAdapter struct + unsafe Send/Sync + re-exports
│   ├── runtime.rs          # RuntimeAdapter + Spawn impls
│   ├── time.rs             # TimeOps impl (Performance.now + setTimeout)
│   ├── logger.rs           # Logger impl (console.log/warn/error)
│   ├── buffer.rs           # WasmBuffer<T> (Rc<RefCell> single-threaded channels)
│   └── bindings.rs         # #[wasm_bindgen] TypeScript-facing API
├── tests/
│   └── wasm.rs             # wasm-bindgen-test suite
└── pkg/                    # wasm-pack build output (gitignored)
```

### 3.2 Dependency Graph

```
aimdb-wasm-adapter
├── aimdb-core          (default-features = false, features = ["alloc"])
├── aimdb-executor      (default-features = false)
├── wasm-bindgen        0.2
├── wasm-bindgen-futures 0.4     # spawn_local
├── js-sys              0.3      # Date, Promise, setTimeout
├── web-sys             0.3      # console, Performance, Window
├── serde-wasm-bindgen  0.6      # Rust ↔ JsValue conversion
├── serde               (no default features, alloc)
└── serde_json          (no default features, alloc)
```

**No dependency on `tokio`, `embassy-*`, or any OS-level crate.**

### 3.3 Feature Flags

```toml
[features]
default = ["wasm-runtime"]
wasm-runtime = ["wasm-bindgen", "wasm-bindgen-futures", "js-sys", "web-sys"]
# Future: wasi, web-worker, indexeddb-persistence
```

---

## 4. Trait Implementations

### 4.1 The `Send + Sync` Question

Every executor trait requires `Send + Sync` (inherited from `RuntimeAdapter`).
WASM (`wasm32-unknown-unknown`) is single-threaded — there are no data races
by construction. This is the **identical situation** as Embassy on bare-metal
MCUs, and the same solution applies:

```rust
pub struct WasmAdapter;

// SAFETY: wasm32-unknown-unknown is single-threaded.
// No concurrent access is possible — Send + Sync are trivially satisfied.
unsafe impl Send for WasmAdapter {}
unsafe impl Sync for WasmAdapter {}
```

This pattern is established — Embassy has used it since day one:
```rust
// aimdb-embassy-adapter/src/runtime.rs:
unsafe impl Send for EmbassyAdapter {}
unsafe impl Sync for EmbassyAdapter {}
```

The same `unsafe impl` applies to `WasmBuffer<T>` internals (which use
`Rc<RefCell<…>>`, normally `!Send`), justified by the single-threaded
execution model.

### 4.2 `RuntimeAdapter`

```rust
impl RuntimeAdapter for WasmAdapter {
    fn runtime_name() -> &'static str { "wasm" }
}
```

### 4.3 `Spawn`

WASM has no thread pool. `wasm_bindgen_futures::spawn_local` schedules a
`Future` on the browser's microtask queue. This is analogous to Embassy's
static task pool — fire-and-forget, no join handle.

```rust
impl Spawn for WasmAdapter {
    type SpawnToken = ();  // Same as Embassy — no join handle

    fn spawn<F>(&self, future: F) -> ExecutorResult<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        wasm_bindgen_futures::spawn_local(future);
        Ok(())
    }
}
```

`spawn_local` requires `F: 'static` but **not** `F: Send`. The `Send` bound
on the trait is satisfied vacuously — all types are effectively `Send` in a
single-threaded context.

### 4.4 `TimeOps`

Browser time is floating-point milliseconds from `Performance.now()`, with
`setTimeout` for sleeping. We define lightweight wrappers:

```rust
/// Milliseconds since page load (from Performance.now())
#[derive(Clone, Debug)]
pub struct WasmInstant(f64);

/// Duration in milliseconds
#[derive(Clone, Debug)]
pub struct WasmDuration(f64);

// SAFETY: single-threaded — no concurrent access possible
unsafe impl Send for WasmInstant {}
unsafe impl Sync for WasmInstant {}
unsafe impl Send for WasmDuration {}
unsafe impl Sync for WasmDuration {}

impl TimeOps for WasmAdapter {
    type Instant = WasmInstant;
    type Duration = WasmDuration;

    fn now(&self) -> WasmInstant {
        let perf = web_sys::window()
            .expect("no window")
            .performance()
            .expect("no performance API");
        WasmInstant(perf.now())
    }

    fn duration_since(&self, later: WasmInstant, earlier: WasmInstant) -> Option<WasmDuration> {
        let diff = later.0 - earlier.0;
        if diff >= 0.0 { Some(WasmDuration(diff)) } else { None }
    }

    fn millis(&self, ms: u64) -> WasmDuration { WasmDuration(ms as f64) }
    fn secs(&self, s: u64) -> WasmDuration { WasmDuration(s as f64 * 1000.0) }
    fn micros(&self, us: u64) -> WasmDuration { WasmDuration(us as f64 / 1000.0) }

    fn sleep(&self, duration: WasmDuration) -> impl Future<Output = ()> + Send {
        // Convert setTimeout Promise to a Rust Future.
        // setTimeout never rejects, so the Ok/Err result is safe to discard.
        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
            &mut |resolve, _| {
                web_sys::window()
                    .unwrap()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        &resolve,
                        duration.0 as i32,
                    )
                    .unwrap();
            },
        ))
        .map(|_result| ())
    }
}
```

### 4.5 `Logger`

Maps directly to the browser console:

```rust
impl Logger for WasmAdapter {
    fn info(&self, message: &str)  { web_sys::console::log_1(&message.into()); }
    fn debug(&self, message: &str) { web_sys::console::debug_1(&message.into()); }
    fn warn(&self, message: &str)  { web_sys::console::warn_1(&message.into()); }
    fn error(&self, message: &str) { web_sys::console::error_1(&message.into()); }
}
```

---

## 5. Buffer Implementation

### 5.1 Design Rationale

| Approach | Pros | Cons |
|----------|------|------|
| Port `tokio::sync` channels | Feature-complete, metrics | Pulls in atomic ops, oversized for single thread |
| Use `futures::channel::mpsc` | Well-tested, async-ready | Extra dependency, `Sender` is `!Sync` |
| **`Rc<RefCell<…>>` + `Waker`** | **Zero-cost single-threaded, no atomics, no deps** | **Must `unsafe impl Send + Sync`** |
| Reuse Embassy buffer | Proven no_std pattern | Pulls in `embassy-sync`, const generics infect public API |

**Decision: `Rc<RefCell<…>>` + Waker.** This matches the browser's
single-threaded model perfectly. No atomic operations, no mutex overhead, no
dependency on `embassy-sync` or `futures`. The buffers are simple, auditable,
and fast.

### 5.2 Buffer Types

```rust
pub struct WasmBuffer<T> {
    inner: Rc<RefCell<WasmBufferInner<T>>>,
}

enum WasmBufferInner<T> {
    /// Bounded ring buffer with independent consumer cursors
    SpmcRing {
        ring: VecDeque<T>,
        capacity: usize,
        /// Each subscriber gets a cursor index
        subscribers: Vec<Weak<RefCell<SpmcCursor<T>>>>,
        wakers: Vec<Waker>,
    },
    /// Only the latest value, skip intermediates
    SingleLatest {
        value: Option<T>,
        version: u64,
        wakers: Vec<Waker>,
    },
    /// Single slot, overwrite semantics
    Mailbox {
        slot: Option<T>,
        wakers: Vec<Waker>,
    },
}

// SAFETY: wasm32 is single-threaded — Rc<RefCell<…>> cannot be accessed concurrently
unsafe impl<T> Send for WasmBuffer<T> {}
unsafe impl<T> Sync for WasmBuffer<T> {}
```

### 5.3 Reader Implementation

Readers implement `BufferReader<T>` by returning a `Future` that either
resolves immediately (data available) or registers a `Waker` and returns
`Poll::Pending`. When `push()` is called on the buffer, all registered
wakers are woken, continuing the reader futures on the next microtask.

```rust
pub struct WasmBufferReader<T> {
    buffer: Rc<RefCell<WasmBufferInner<T>>>,
    // Reader-specific state (cursor for SpmcRing, version for SingleLatest)
    state: ReaderState,
}

// SAFETY: wasm32 is single-threaded — no concurrent access possible
unsafe impl<T> Send for WasmBufferReader<T> {}
unsafe impl<T> Sync for WasmBufferReader<T> {}
```

### 5.4 Macro Invocation

```rust
aimdb_core::impl_record_registrar_ext! {
    WasmRecordRegistrarExt,
    WasmAdapter,
    WasmBuffer,
    "wasm-runtime",
    |cfg| WasmBuffer::<T>::new(cfg)
}
```

This generates `buffer()`, `source()`, `tap()`, `transform()`, and
`transform_join()` methods — identical API surface to Tokio and Embassy.

---

## 6. TypeScript Bindings (`#[wasm_bindgen]`)

### 6.1 Design Principle

Expose a **high-level facade** to TypeScript, not the raw Rust builder pattern.
JavaScript users should not deal with `Arc`, `RecordRegistrar`, or feature
flags. The facade wraps the Rust API and handles `JsValue` ↔ Rust conversion
via `serde-wasm-bindgen`.

### 6.2 Core API

```typescript
// ── Instantiation ──────────────────────────────────────────────
import { WasmDb } from '@aimdb/wasm';

const db = new WasmDb();

// ── Record configuration (key = RecordKey, schemaType = contract) ──
db.configureRecord('sensors.temperature.vienna', {
  schemaType: 'temperature',        // selects the Rust struct for validation
  buffer: 'SingleLatest',           // or { type: 'SpmcRing', capacity: 100 }
});

db.configureRecord('sensors.temperature.berlin', {
  schemaType: 'temperature',        // same contract, different record
  buffer: 'SingleLatest',
});

// ── Read (validated by Rust serde) ─────────────────────────────
const temp = await db.get('sensors.temperature.vienna');
// temp: { celsius: number, timestamp: number } — or null if not yet produced

// ── Write (validated by Rust serde) ─────────────────────────────
db.set('sensors.temperature.vienna', { celsius: 22.5, timestamp: Date.now() });
// throws if payload fails Rust deserialization → contract enforcement

// ── Subscribe (reactive) ───────────────────────────────────────
const unsub = db.subscribe('sensors.temperature.vienna', (value) => {
  // Fires on every buffer push — value is already validated
  console.log(value.celsius);
});
unsub(); // cleanup

// ── Lifecycle ──────────────────────────────────────────────────
db.free();  // Release WASM memory
```

### 6.3 Rust-Side Binding Implementation

```rust
#[wasm_bindgen]
pub struct WasmDb {
    inner: AimDb<WasmAdapter>,
}

#[wasm_bindgen]
impl WasmDb {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmDb, JsError> {
        let adapter = WasmAdapter;
        let db = AimDbBuilder::new()
            .runtime(Arc::new(adapter))
            .build()?;
        Ok(WasmDb { inner: db })
    }

    /// Get the current value of a record by its RecordKey (returns JsValue or undefined)
    pub fn get(&self, record_key: &str) -> Result<JsValue, JsError> {
        // Uses AimDb key lookup + serde_wasm_bindgen::to_value
        ...
    }

    /// Set a record value by its RecordKey (validates via Rust serde deserialization)
    pub fn set(&self, record_key: &str, value: JsValue) -> Result<(), JsError> {
        // serde_wasm_bindgen::from_value → T, then push to buffer
        // If deserialization fails → JsError with contract violation message
        ...
    }

    /// Subscribe to record updates by RecordKey — returns a closure to unsubscribe
    pub fn subscribe(
        &self,
        record_key: &str,
        callback: js_sys::Function,
    ) -> Result<JsValue, JsError> {
        // Creates a BufferReader, spawns a loop that calls callback on each recv()
        // Returns a JS function that aborts the loop
        ...
    }
}
```

### 6.4 Record Registration by Key

Records are identified by their **`RecordKey`** — the same string used
throughout AimDB (e.g. `"sensors.temperature.vienna"`). The key uniquely
identifies a record *instance*, while the schema type (from
`SchemaType::NAME`) identifies the *contract*. Multiple records can share
the same contract type.

**Strategy A: Pre-compiled contract registry (recommended for v1)**

All known data contracts from `aimdb-data-contracts` are compiled into the
WASM module. The `configureRecord()` call takes a record key and a schema
type name — the key is used as the AimDB `RecordKey`, and the type name
selects the Rust struct for serde validation:

```typescript
// Record key = AimDB RecordKey, schemaType = SchemaType::NAME
db.configureRecord('sensors.temperature.vienna', {
  schemaType: 'temperature',          // selects Temperature struct for validation
  buffer: 'SingleLatest',
});

db.configureRecord('sensors.temperature.berlin', {
  schemaType: 'temperature',          // same contract, different record
  buffer: { type: 'SpmcRing', capacity: 50 },
});

db.configureRecord('sensors.humidity.vienna', {
  schemaType: 'humidity',
  buffer: 'SingleLatest',
});
```

Rust-side dispatch uses `SchemaType::NAME` to select the concrete type,
and the record key as the `StringKey` for AimDB's `configure()`:

```rust
/// Register a record by key + schema type name.
/// The key becomes the AimDB RecordKey; the schema type selects the Rust struct.
fn configure_record(
    db: &mut AimDbBuilder<WasmAdapter>,
    record_key: &str,
    schema_type: &str,
    cfg: BufferCfg,
) -> Result<(), JsError> {
    let key = StringKey::intern(record_key.to_string());
    match schema_type {
        Temperature::NAME => db.configure::<Temperature>(key, |reg| {
            reg.buffer(cfg.clone());
        }),
        Humidity::NAME => db.configure::<Humidity>(key, |reg| {
            reg.buffer(cfg.clone());
        }),
        GpsLocation::NAME => db.configure::<GpsLocation>(key, |reg| {
            reg.buffer(cfg);
        }),
        _ => return Err(JsError::new(&format!("Unknown schema type: {schema_type}"))),
    };
    Ok(())
}
```

This means `get()`, `set()`, and `subscribe()` all use the record key:

```typescript
db.set('sensors.temperature.vienna', { celsius: 22.5, timestamp: Date.now() });
const temp = await db.get('sensors.temperature.vienna');
db.subscribe('sensors.temperature.vienna', (value) => { ... });
```

**Strategy B: Dynamic JSON records (future)**

Register records with a JSON Schema at runtime, validated against
`serde_json::Value`. No compile-time Rust struct needed. This requires
the `DynRecord` concept tracked separately.

### 6.5 React Integration

For `aimdb-ui`, provide a thin React hook wrapping the WASM API.
The hook takes a **record key** (the AimDB `RecordKey` string):

```typescript
// useAimDb.ts — library code, ships with @aimdb/wasm npm package
import { WasmDb } from '@aimdb/wasm';

const dbInstance = new WasmDb();

/**
 * Subscribe to a record by its AimDB RecordKey.
 * Returns the current value (validated by Rust serde) or null.
 */
export function useRecord<T>(recordKey: string): T | null {
  const [value, setValue] = useState<T | null>(null);

  useEffect(() => {
    const unsub = dbInstance.subscribe(recordKey, (v: T) => setValue(v));
    return () => unsub();
  }, [recordKey]);

  return value;
}

// Usage in component — record key follows AimDB naming convention:
function TemperatureCard({ city }: { city: string }) {
  const temp = useRecord<Temperature>(`sensors.temperature.${city}`);
  if (!temp) return <Skeleton />;
  return <span>{temp.celsius}°C</span>;
}
```

---

## 7. WebSocket Sync Strategy

### 7.1 Operational Modes

The WASM adapter supports three operational modes. The mode is selected at
instantiation and determines whether the browser AimDB instance runs
standalone, connects to a server, or gracefully transitions between both.

---

#### Mode 1: Local-only

```
┌─────────────────────────────────┐
│           Browser Tab            │
│                                  │
│  ┌───────────┐  ┌────────────┐  │
│  │  aimdb-ui  │→│ AimDB WASM │  │
│  │  (React)   │←│  (local)   │  │
│  └───────────┘  └────────────┘  │
│                                  │
│  Records produced & consumed     │
│  entirely within the browser.    │
└─────────────────────────────────┘
```

AimDB runs entirely in the browser with no network dependency. Records are
configured, produced, and consumed locally using the same buffer semantics
(SPMC Ring, SingleLatest, Mailbox) as server-side deployments.

**Use cases:**
- **Demos & marketing pages** — show live AimDB behaviour without a backend.
  Simulated data can be produced via `source()` using `Simulatable` contracts.
- **Unit / integration testing** — `aimdb-ui` components can be tested against
  a real (local) AimDB instance in `vitest` / `wasm-bindgen-test`, replacing
  mock data with contract-validated records.
- **Offline-capable apps** — sensors write data locally (e.g. via Web
  Bluetooth or manual entry); the UI reacts through subscriptions.
- **Prototyping** — experiment with record schemas, buffer configurations,
  and transforms without deploying a server.

**TypeScript API:**

```typescript
import { WasmDb } from '@aimdb/wasm';

const db = new WasmDb();  // no server URL → local-only

db.configureRecord('sensors.temperature.indoor', {
  schemaType: 'temperature',
  buffer: 'SingleLatest',
});

// Produce locally (e.g. from a BLE sensor, manual input, or simulation)
db.set('sensors.temperature.indoor', { celsius: 22.5, timestamp: Date.now() });

// Subscribe — fires immediately since the buffer has a value
db.subscribe('sensors.temperature.indoor', (temp) => {
  console.log(temp.celsius);  // 22.5
});
```

**Characteristics:**
- Zero network I/O — no WebSocket, no HTTP.
- Full contract enforcement — `set()` validates via Rust serde.
- All buffer types available — SPMC Ring for history, SingleLatest for
  current-value dashboards, Mailbox for commands.
- `source()` / `tap()` / `transform()` work natively — can run local
  dataflow pipelines in the browser.

---

#### Mode 2: Synchronized

```
┌─────────────────────────────────┐              ┌──────────────────────┐
│           Browser Tab            │    ws://     │      Server          │
│                                  │              │                      │
│  ┌───────────┐  ┌────────────┐  │  subscribe   │  ┌────────────────┐  │
│  │  aimdb-ui  │→│ AimDB WASM │──┼─────────────→│  │  AimDB         │  │
│  │  (React)   │←│  (local)   │←─┼──────────────│  │  + WS          │  │
│  └───────────┘  └────────────┘  │  data/snapshot│  │  Connector     │  │
│                       │         │              │  └───────┬────────┘  │
│                       │ write   │              │          │           │
│                       └─────────┼─────────────→│    MQTT / KNX /     │
│                                 │              │    Persistence       │
└─────────────────────────────────┘              └──────────────────────┘
```

The browser AimDB instance connects to the server's WebSocket connector
via `WsBridge`. Server records are mirrored into local buffers. UI components
subscribe to local records — they never interact with the WebSocket directly.

**Use cases:**
- **Production dashboards** — `aimdb-ui` receives live sensor data from the
  server's MQTT/KNX mesh via the WebSocket connector, with contract
  validation at the WASM boundary before data reaches React.
- **Control panels** — the user writes a setpoint or config record locally;
  `WsBridge` forwards the `write` ClientMessage to the server, which routes
  it to MQTT/KNX via `link_from("ws://…")`.
- **Multi-tab consistency** — each browser tab runs its own AimDB WASM
  instance, each with its own `WsBridge` connection. The server is the
  single source of truth; tabs converge via late-join snapshots.

**TypeScript API:**

```typescript
import { WasmDb, WsBridge } from '@aimdb/wasm';

const db = new WasmDb();

// Configure records matching the server's outbound topics
db.configureRecord('sensors.temperature.vienna', {
  schemaType: 'temperature',
  buffer: 'SingleLatest',
});
db.configureRecord('sensors.humidity.vienna', {
  schemaType: 'humidity',
  buffer: 'SingleLatest',
});

// Connect to the server's WebSocket connector
const bridge = WsBridge.connect(db, 'wss://api.cloud.aimdb.dev/ws', {
  subscribeTopics: ['sensors/#'],   // MQTT-style wildcard patterns
  autoReconnect: true,              // reconnect with exponential backoff
  lateJoin: true,                   // request snapshots on (re)connect
});

// Subscribe to local records — updated by WsBridge from server push
db.subscribe('sensors.temperature.vienna', (temp) => {
  console.log(temp.celsius);  // pushed from server → local buffer → callback
});

// Write travels: local buffer → WsBridge → server → MQTT/KNX
db.set('commands.setpoint.room1', { target_celsius: 21.0, timestamp: Date.now() });

// Lifecycle
bridge.disconnect();
db.free();
```

**Data flow — server → browser:**

1. Server AimDB produces a `Temperature` record (e.g. from MQTT inbound).
2. The outbound `link_to("ws://sensors/temperature/vienna")` triggers the
   WS connector's `broadcast()`.
3. `ServerMessage::Data { topic, payload, ts }` is sent over WebSocket.
4. `WsBridge.on_message` receives the JSON frame, dispatches by `topic`.
5. The bridge resolves the `topic` to a local `RecordKey`, deserializes the
   `payload` via the record's contract type (Rust serde — this is where
   contract enforcement happens), and pushes to the local `WasmBuffer`.
6. React components subscribed via `useRecord()` re-render with the
   validated value.

**Data flow — browser → server:**

1. UI calls `db.set('commands.setpoint.room1', { ... })`.
2. The local buffer receives the value (contract-validated by serde).
3. `WsBridge` detects the local write (via a `tap()` on the record) and
   sends a `ClientMessage::Write { topic, payload }` over WebSocket.
4. Server's WS connector routes it through the standard `Router` — same
   path as any `link_from("ws://…")` record.
5. The server pushes it to MQTT, KNX, persistence, or another AimDB record
   — depending on the server-side configuration.

**Resilience:**
- **Reconnection**: Exponential backoff (default: 500ms → 1s → 2s → 4s →
  8s), matching the current `useWebSocketConnection.ts` strategy.
- **Late-join**: On reconnect, the bridge sends `ClientMessage::Subscribe`
  which triggers server-side `snapshot` responses for each topic —
  re-seeding local buffers with current values.
- **Offline writes**: Writes during disconnection are buffered locally.
  On reconnect, the bridge flushes pending writes to the server (FIFO).
  Buffer capacity is configurable; overflow policy matches the buffer type
  (drop oldest for SPMC Ring, overwrite for SingleLatest/Mailbox).

---

#### Mode 3: Hybrid (offline-first with sync)

```
┌─────────────────────────────────┐              ┌──────────────────────┐
│           Browser Tab            │              │      Server          │
│                                  │   online     │                      │
│  ┌───────────┐  ┌────────────┐  │◄═══════════►│  AimDB + WS          │
│  │  aimdb-ui  │→│ AimDB WASM │  │              │  Connector            │
│  │  (React)   │←│  (local)   │  │   offline    └──────────────────────┘
│  └───────────┘  └────────────┘  │◄── ─ ─ ─ ─ ►  (unavailable)
│                                  │
│  Local records always available. │
│  Server sync when possible.      │
└─────────────────────────────────┘
```

Hybrid mode combines Modes 1 and 2. The browser AimDB always has local
records, and the `WsBridge` connects to the server when available. If the
server is unreachable, the UI continues working with locally buffered data.
When the connection is restored, it re-syncs via late-join snapshots.

**Use cases:**
- **Field worker apps** — a technician configures HVAC setpoints on a
  tablet. Changes apply locally immediately (local buffer → UI update) and
  sync to the server when connectivity returns.
- **Progressive web apps (PWAs)** — the app is installable and works offline.
  Sensor readings cached in local buffers are available for review even
  without a network connection.
- **Unreliable networks** — edge deployments with intermittent
  connectivity (construction sites, industrial floors, rural IoT).

**TypeScript API:**

```typescript
import { WasmDb, WsBridge } from '@aimdb/wasm';

const db = new WasmDb();

db.configureRecord('sensors.temperature.vienna', {
  schemaType: 'temperature',
  buffer: { type: 'SpmcRing', capacity: 200 },  // keep history locally
});

// WsBridge attempts connection immediately but doesn't block
const bridge = WsBridge.connect(db, 'wss://api.cloud.aimdb.dev/ws', {
  subscribeTopics: ['sensors/#'],
  autoReconnect: true,
  lateJoin: true,
});

// This works immediately — even before / without server connection
db.subscribe('sensors.temperature.vienna', (temp) => {
  renderDashboard(temp);
});

// Connection status is observable
bridge.onStatusChange((status) => {
  // status: 'connecting' | 'connected' | 'disconnected' | 'reconnecting'
  updateConnectionIndicator(status);
});
```

**State transitions:**

```
                    ┌───────────┐  connected   ┌───────────┐
         ┌────────►│ connecting ├─────────────►│ connected │
         │         └─────┬─────┘              └─────┬─────┘
         │               │ timeout/error             │ close/error
         │               ▼                           ▼
         │         ┌───────────────┐          ┌──────────────┐
         └─────────┤ disconnected  │◄─────────┤ reconnecting │
         reconnect └───────────────┘  max     └──────────────┘
                                     retries      │     ▲
                                                  │     │
                                                  └─────┘
                                                  backoff
```

While `disconnected` or `reconnecting`, the local AimDB instance keeps
functioning. Subscriptions fire on local writes, transforms execute, and
the UI remains interactive. The only difference is that no server data
arrives and outbound writes are queued.

---

#### Mode Selection Summary

| | Mode 1: Local | Mode 2: Synced | Mode 3: Hybrid |
|---|---|---|---|
| **Network** | None | Required | Optional |
| **Server dependency** | None | Hard | Soft (graceful degradation) |
| **Contract enforcement** | Local serde | Local serde + server serde | Local serde + server serde when connected |
| **Offline writes** | Always works | Fails if disconnected | Queued, flushed on reconnect |
| **Late-join** | N/A | On connect | On connect / reconnect |
| **Data source** | Local `set()` / `source()` | Server push via `WsBridge` | Both |
| **Typical use** | Demos, tests, offline apps | Production dashboards | Field apps, PWAs, unreliable networks |
| **API** | `new WasmDb()` | `new WasmDb()` + `WsBridge.connect(db, url)` | Same as Mode 2 (degrades automatically) |

The API is incremental: every app starts as Mode 1 by constructing `WasmDb`.
Adding `WsBridge.connect()` upgrades to Mode 2 or 3 depending on the
network — no code change needed to handle offline fallback.

### 7.2 `WsBridge` Implementation

The bridge is the Rust-side component behind Modes 2 and 3. It wraps
`web_sys::WebSocket` and maps the server's wire protocol to local buffer
operations:

```rust
pub struct WsBridge {
    ws: web_sys::WebSocket,
    db: Rc<WasmDb>,          // Shared — caller retains access for get/set/subscribe
    config: BridgeConfig,
    state: Rc<RefCell<BridgeState>>,
}

pub struct BridgeConfig {
    pub url: String,
    pub subscribe_topics: Vec<String>,       // MQTT wildcard patterns
    pub auto_reconnect: bool,
    pub late_join: bool,
    pub max_offline_queue: usize,            // pending writes while disconnected
    pub backoff: Vec<u32>,                   // ms: [500, 1000, 2000, 4000, 8000]
}

struct BridgeState {
    status: ConnectionStatus,
    pending_writes: VecDeque<ClientMessage>,  // queued during disconnect
    backoff_index: usize,
}
```

The bridge hooks into browser WebSocket callbacks:

- **`on_open`** — sends `ClientMessage::Subscribe { topics }` to begin
  receiving data. Flushes any queued writes from `pending_writes`.
- **`on_message`** — parses `ServerMessage`, dispatches by variant:
  - `Data { topic, payload, ts }` / `Snapshot { topic, payload }` →
    resolves `topic` to a `RecordKey`, deserializes `payload` via the
    record's contract type, pushes to local `WasmBuffer`.
  - `Subscribed { topics }` → log/event for UI connection indicator.
  - `Error { code, topic, message }` → log to console; surface via
    `bridge.onError()` callback if registered.
- **`on_close`** — transition to `reconnecting`, schedule `setTimeout`
  with backoff, retry.
- **`on_error`** — ignored (the `close` event always follows).

Topic-to-RecordKey resolution uses the same mapping established during
`configureRecord()`. If a `data` message arrives for an unknown topic,
it is logged and discarded (no panic).

### 7.3 Wire Protocol Compatibility

The bridge speaks the exact protocol defined in
`aimdb-websocket-connector/src/protocol.rs`:

| Direction | Message | Bridge Behaviour |
|-----------|---------|------------------|
| Server → Client | `data` | Deserialize payload → push to local buffer |
| Server → Client | `snapshot` | Same as `data` (late-join seed) |
| Server → Client | `subscribed` | Emit `onStatusChange('connected')` |
| Server → Client | `error` | Log + invoke error callback |
| Server → Client | `pong` | Reset keepalive timer |
| Client → Server | `subscribe` | Sent on connect with `config.subscribe_topics` |
| Client → Server | `unsubscribe` | Sent on `bridge.unsubscribe(topics)` |
| Client → Server | `write` | Sent when local record is written (via tap) |
| Client → Server | `ping` | Periodic keepalive (default: 30s) |

No custom protocol extensions are needed — the WASM adapter is a standard
WebSocket connector client.

---

## 8. Build & Packaging

### 8.1 Build Toolchain

```bash
# Install
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# Build
cd aimdb-wasm-adapter
wasm-pack build --target web --out-dir pkg

# Output:
# pkg/aimdb_wasm_adapter.js      (JS glue)
# pkg/aimdb_wasm_adapter_bg.wasm (WASM binary)
# pkg/aimdb_wasm_adapter.d.ts    (TypeScript declarations)
# pkg/package.json               (npm-publishable)
```

### 8.2 npm Distribution

`wasm-pack` generates a ready-to-publish npm package. In `aimdb-ui`:

```json
{
  "dependencies": {
    "aimdb-wasm": "file:../../aimdb/aimdb-wasm-adapter/pkg"
  }
}
```

Or published to npm as `@aimdb/wasm` (preferred — see Open Question #4).

### 8.3 Bundle Size Budget

| Component | Estimated Size (gzipped) |
|-----------|--------------------------|
| WASM binary (core + 3 contracts + buffers) | ~80–120 KB |
| JS glue (wasm-bindgen) | ~5 KB |
| `serde_json` in WASM | ~30 KB |
| **Total** | **~115–155 KB** |

For comparison: Zod (~13 KB) + manual schema code (~5 KB) = ~18 KB, but
provides only validation — no buffers, no sync, no migration, no offline.

### 8.4 Makefile Integration

```makefile
# Addition to /aimdb_ws/aimdb/Makefile

.PHONY: wasm
wasm:  ## Build WASM adapter
	cd aimdb-wasm-adapter && wasm-pack build --target web --out-dir pkg

.PHONY: wasm-test
wasm-test:  ## Run WASM tests in headless browser
	cd aimdb-wasm-adapter && wasm-pack test --headless --chrome
```

---

## 9. Testing Strategy

### 9.1 Unit Tests (Rust, native target)

Buffer logic, time conversion, and record configuration run as normal Rust
tests (`cargo test -p aimdb-wasm-adapter`). The `Send + Sync` unsafe impls
are excluded — these only matter under the `wasm32` target.

### 9.2 WASM Integration Tests

Use `wasm-bindgen-test` with a headless browser:

```rust
#[cfg(target_arch = "wasm32")]
mod wasm_tests {
    use wasm_bindgen_test::*;
    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn test_buffer_push_subscribe() {
        let db = WasmDb::new().unwrap();
        db.configure_record(
            "sensors.temperature.test",
            &serde_wasm_bindgen::to_value(&serde_json::json!({
                "schemaType": "temperature",
                "buffer": "SingleLatest"
            })).unwrap(),
        ).unwrap();

        let (tx, rx) = futures::channel::oneshot::channel();
        let cb = Closure::once(move |val: JsValue| { tx.send(val).unwrap(); });
        db.subscribe("sensors.temperature.test", cb.as_ref().unchecked_ref()).unwrap();

        db.set("sensors.temperature.test", serde_wasm_bindgen::to_value(
            &Temperature::new(22.5, 1234567890000)
        ).unwrap()).unwrap();

        let received = rx.await.unwrap();
        let temp: Temperature = serde_wasm_bindgen::from_value(received).unwrap();
        assert_eq!(temp.celsius, 22.5);
    }

    #[wasm_bindgen_test]
    fn test_contract_enforcement_rejects_invalid() {
        let db = WasmDb::new().unwrap();
        db.configure_record(
            "sensors.temperature.test",
            &serde_wasm_bindgen::to_value(&serde_json::json!({
                "schemaType": "temperature",
                "buffer": "SingleLatest"
            })).unwrap(),
        ).unwrap();

        // Missing required field → JsError
        let bad = js_sys::Object::new();
        js_sys::Reflect::set(&bad, &"celsius".into(), &22.5.into()).unwrap();
        // Missing timestamp → deserialization fails
        let result = db.set("sensors.temperature.test", bad.into());
        assert!(result.is_err());
    }

    #[wasm_bindgen_test]
    async fn test_sleep_resolves() {
        let adapter = WasmAdapter;
        let start = adapter.now();
        adapter.sleep(adapter.millis(50)).await;
        let elapsed = adapter.duration_since(adapter.now(), start).unwrap();
        assert!(elapsed.0 >= 45.0); // Allow 5ms jitter
    }
}
```

### 9.3 CI Integration

Add to the `check` target in the Makefile and GitHub Actions:

```yaml
wasm-check:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
      with:
        targets: wasm32-unknown-unknown
    - uses: nicolo-ribaudo/setup-wasm-pack@v1
    - run: wasm-pack test --headless --chrome -- -p aimdb-wasm-adapter
```

---

## 10. Migration Path for `aimdb-ui`

### Phase 1: Drop-in Replacement (Low Risk)

Keep the existing WebSocket hook architecture. Replace `JSON.parse` + blind
cast with WASM-validated deserialization:

```typescript
// Before (useWebSocketConnection.ts)
ws.onmessage = (event) => {
  const data = JSON.parse(event.data);       // unvalidated
  const normalized = normalizeMessage(data);  // manual shape check
  onMessageRef.current(normalized);
};

// After
import { validate } from '@aimdb/wasm';

ws.onmessage = (event) => {
  const result = validate(event.data);        // Rust serde in WASM
  if (result.ok) {
    onMessageRef.current(result.value);
  } else {
    console.warn('Contract violation:', result.error);
  }
};
```

### Phase 2: Local DB Instance

Replace the WebSocket hooks entirely. AimDB WASM manages the WebSocket
connection, buffering, and reactive subscriptions:

```typescript
// Before: useWebSocket() → useWebSocketConnection() → manual state management
// After:  useRecord() → AimDB WASM handles everything

function Dashboard() {
  const temp = useRecord<Temperature>('sensors.temperature.vienna');
  const humidity = useRecord<Humidity>('sensors.humidity.vienna');
  // Reactive, validated, offline-capable
}
```

### Phase 3: Full Bidirectional Sync

Enable browser-to-server writes. The `WsBridge` manages the connection
lifecycle including reconnection and late-join. The UI becomes a full
AimDB node in the mesh:

```
MQTT Sensors → Server AimDB → WS Connector → Browser AimDB → React UI
                                    ↑                  │
                                    └──────────────────┘
                                      (bidirectional)
```

---

## 11. Risk Analysis

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| `Send + Sync` unsoundness | Low | High | Single-threaded by construction; same pattern as Embassy; CI runs WASM tests with `wasm-bindgen-test` |
| Bundle size too large | Medium | Medium | Feature-gate contracts; tree-shake unused types; consider `serde_json` alternatives (`miniserde`, `nanoserde`) |
| `Performance.now()` precision | Low | Low | Only used for relative timing; sub-ms precision is sufficient |
| Browser API unavailability (SSR) | Medium | Low | Feature-gate `web-sys` calls; provide no-op stubs for SSR/Node |
| `spawn_local` backpressure | Medium | Medium | Buffer producers that yield; configurable channel capacity; drop-slow-consumer policy (same as server) |
| WASM init async requirement | Low | Low | `wasm-bindgen` handles init; React `Suspense` for loading state |

---

## 12. Open Questions

1. **Should `WasmBuffer` use dynamic sizing (Vec-backed) or const generics
   (Embassy-style)?**
   Recommendation: Dynamic. Browser has plentiful heap; const generics
   complicate the JS API and provide no benefit without embedded memory
   constraints.

2. **Should the `WsBridge` reuse `aimdb-client` (AimX protocol) or speak
   the WebSocket connector protocol directly?**
   Recommendation: WebSocket connector protocol. `aimdb-client` uses Unix
   sockets (not available in browsers). The WS connector protocol
   (`ServerMessage`/`ClientMessage`) is JSON-based and designed for this.

3. **How should schema registration work for user-defined contracts not in
   `aimdb-data-contracts`?**
   v1: Only pre-compiled contracts. v2: A `DynRecord` type backed by
   `serde_json::Value` with optional JSON Schema validation.

4. **npm package name: `aimdb-wasm` or `@aimdb/wasm`?**
   Scoped name (`@aimdb/wasm`) is preferred if publishing to npm.

---

## 13. Implementation Plan

| Phase | Scope | Effort |
|-------|-------|--------|
| **P1: Skeleton** | Crate scaffolding, `WasmAdapter` struct, unsafe Send/Sync, `RuntimeAdapter` + `Logger` impls, compiles to `wasm32-unknown-unknown` | 1 day |
| **P2: Time + Spawn** | `TimeOps` with `Performance.now()` / `setTimeout`, `Spawn` with `spawn_local`, `wasm-bindgen-test` suite | 1 day |
| **P3: Buffers** | `WasmBuffer<T>` (all 3 types), `BufferReader<T>`, macro invocation, basic round-trip tests | 2 days |
| **P4: Bindings** | `#[wasm_bindgen]` API — `WasmDb`, `get`/`set`/`subscribe`, `serde-wasm-bindgen` bridge | 2 days |
| **P5: Contract Integration** | Wire up `aimdb-data-contracts` types (Temperature, Humidity, GpsLocation), validation tests | 1 day |
| **P6: WsBridge** | WebSocket client in WASM, `ServerMessage`/`ClientMessage` protocol, reconnection | 2 days |
| **P7: React Hooks** | `useRecord()`, `useAimDb()`, integration with `aimdb-ui` | 1 day |
| **P8: CI & Docs** | GitHub Actions WASM job, README, wasm-pack publish, Makefile targets | 1 day |
| **Total** | | **~11 days** |

---

## 14. References

- Embassy adapter (precedent for single-threaded unsafe Send/Sync):
  `aimdb-embassy-adapter/src/runtime.rs`
- WebSocket connector protocol:
  `aimdb-websocket-connector/src/protocol.rs`
- Executor trait definitions:
  `aimdb-executor/src/lib.rs`
- Extension macro:
  `aimdb-core/src/ext_macros.rs`
- Data contracts codegen:
  `aimdb-data-contracts/tests/export_ts.rs`
- wasm-bindgen guide:
  https://rustwasm.github.io/docs/wasm-bindgen/
- `serde-wasm-bindgen`:
  https://docs.rs/serde-wasm-bindgen
