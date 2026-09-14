# AimDB Connector Development Guide (LLM Context)

**Status:** ✅ Implemented (Reference Documentation)  
**Purpose:** Reference for implementing bidirectional connectors

**Dual Runtime Support:**
- **Tokio** (`std`): Servers, edge, cloud
- **Embassy** (`no_std`): Embedded MCUs

**Reference Implementation:** MQTT connector (`aimdb-mqtt-connector/`)

---

## Core Traits

### ConnectorBuilder
```rust
pub trait ConnectorBuilder<R: aimdb_executor::Spawn + 'static>: Send + Sync {
    fn build<'a>(&'a self, db: &'a AimDb<R>) 
        -> Pin<Box<dyn Future<Output = DbResult<Arc<dyn Connector>>> + Send + 'a>>;
    fn scheme(&self) -> &str;  // e.g., "mqtt", "kafka", "modbus"
}
```

### Connector
```rust
pub trait Connector: Send + Sync {
    fn publish(&self, destination: &str, config: &ConnectorConfig, payload: &[u8])
        -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>>;
}
```

---

## Build Phase Pattern

**Sequence (in `ConnectorBuilder::build()`):**
1. Collect inbound routes: `db.collect_inbound_routes(scheme)`
2. Build router: `RouterBuilder::from_routes(inbound_routes).build()`
3. Create connector instance
4. Collect outbound routes: `db.collect_outbound_routes(scheme)`
5. **Spawn outbound publishers:** `connector.spawn_outbound_publishers(db, outbound_routes)`
6. Spawn background tasks (connection management, inbound event loop)
7. Return `Arc<dyn Connector>`

**Critical:** Must call `spawn_outbound_publishers()` or outbound data won't flow.

---

## Data Flow

**Outbound (AimDB → External):**
```
Record → Consumer → Serializer → Connector.publish() → External
```

**Inbound (External → AimDB):**
```
External → Event Handler → Router.route() → Producer → Record
```

---

## Outbound Publisher Pattern

**Required implementation:**
```rust
impl MyConnector {
    fn spawn_outbound_publishers<R: aimdb_executor::Spawn + 'static>(
        &self,
        db: &AimDb<R>,
        routes: Vec<(String, Box<dyn ConsumerTrait>, SerializerFn, Vec<(String, String)>)>,
    ) -> DbResult<()> {
        for (destination, consumer, serializer, _config) in routes {
            let connector_clone = self.clone();
            
            db.runtime().spawn(async move {
                let mut reader = consumer.subscribe_any().await.unwrap();
                
                while let Ok(value_any) = reader.recv_any().await {
                    let bytes = serializer(&*value_any).unwrap();
                    connector_clone.publish(&destination, &Default::default(), &bytes).await.ok();
                }
            })?;
        }
        Ok(())
    }
}
```

---

## User Configuration Example

```rust
let db = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(MyConnectorBuilder::new("proto://host:port"))
    .configure::<Temperature>(|reg| {
        reg.link_to("proto://sensor/temp")
           .with_serializer(|t| serde_json::to_vec(t).unwrap());
    })
    .configure::<Command>(|reg| {
        reg.link_from("proto://cmd/device")
           .with_deserializer(|b| serde_json::from_slice(b).unwrap())
           .with_buffer(BufferCfg::SingleLatest);
    })
    .build().await?;
```

---

## Critical Patterns

### Reconnection Logic
Place in spawned background task, not in `publish()`:
```rust
db.runtime().spawn(async move {
    loop {
        match connect_and_run(&url).await {
            Ok(_) => break,
            Err(e) => {
                eprintln!("Connection failed: {e:?}, retrying...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
});
```

### Inbound Routing
Use router for automatic dispatch:
```rust
// Subscribe to all router topics
let topics = router.resource_ids();
client.subscribe(&topics).await?;

// Route incoming messages
router.route(topic, payload).await?;
```

### QoS Configuration
Extract from `ConnectorConfig`:
```rust
fn publish(&self, dest: &str, config: &ConnectorConfig, payload: &[u8]) -> ... {
    let qos = config.qos.unwrap_or(0);
    let retain = config.retain.unwrap_or(false);
    client.publish(dest, qos, retain, payload).await
}
```

---

## Choosing the Transport Seam (do this first)

Before writing any integration code, answer one question about the protocol
library you are considering:

> **Does it hand me bytes, or does it hand me a client?**

The answer fixes the shape of your connector and it is not recoverable later.
A library that owns its own socket will not accept yours no matter how the
adapter layer is designed. This is a **library-selection** decision, not an
implementation decision.

### The three tiers

| Tier | Who owns the protocol | Example in this workspace | Shape you get |
|---|---|---|---|
| **1** | **AimDB** — you write the framing | TCP (`framing.rs`, length-prefix), serial (COBS `Framer`) | Symmetric. The adapter supplies bytes on both std and embedded; one implementation |
| **2** | **A sans-io library**, AimDB owns the lifecycle | KNX — `knx-pico` is sans-io, `tunnel.rs` owns tunnelling behind a three-method `TunnelIo` | Symmetric. Design 052 §2 found the two halves already 90 % shared |
| **3** | **A batteries-included client** — owns socket, TLS, reconnect | `rumqttc` (MQTT std half), `axum` / `tokio-tungstenite` (WebSocket) | **Asymmetric, or std-only.** The library dials; you cannot inject a stream |

Tiers 1 and 2 are the good cases and they cost the same to build. Tier 3 is
sometimes the right trade, a mature client buys QoS 2, a hardened TLS stack,
platform trust roots, but buy it knowingly.

### How to tell which tier a candidate library is

Read its constructor and its transport type before anything else:

- **Tier 1/2 signature** — takes a connection, a stream or nothing:
  ```rust
  ClientNoQueue::new(connection, buffer, delay, timeout, handler)  // mountain-mqtt
  ```
  Anything generic over `embedded_io_async::{Read, Write}`, or over its own
  minimal `Connection` trait, is injectable. Good.
- **Tier 3 signature** — takes options and an address:
  ```rust
  AsyncClient::new(mqtt_options, capacity)                          // rumqttc
  mqtt_options.set_transport(Transport::Tls(..))                    // closed enum
  ```
  If the transport is a **closed enum** with no "bring your own stream" variant,
  the library dials internally and the seam is fixed above it.

Also check: does it pull `tokio` (or any executor) in its own `[dependencies]`,
or only `embedded-io-async` / `embedded-hal-async`? An executor dependency in
the protocol crate is a reliable tier-3 signal.

### What each tier means for you

| | Tier 1 / 2 | Tier 3 |
|---|---|---|
| Runtime neutrality | Free — one module, no runtime `cfg` | Not achievable for that half |
| New runtime (FreeRTOS, …) | Zero connector edits — a new adapter is enough | Needs a second backend, or the connector stays std-only |
| Host tests for the embedded path | Run the same code over the std adapter's transport | Only if a second, injectable backend exists |
| Cost | You write framing or lifecycle logic | The library writes it for you |

### If you land on tier 3

Two legitimate outcomes, both present in this workspace:

- **std-only connector** — WebSocket and UDS. Honest and simple when there is no
  embedded use case. Do not invent an embedded half that nobody wants.
- **Two backends behind one type** — MQTT. `MqttConnector<B>` carries `Native`
  (`rumqttc`, std) and `Embedded<D>` (`mountain-mqtt`, any target with a
  `StreamDialer`). The seam is the *backend*, not the runtime.

What **not** to do: give the tier-3 backend a `.transport()` method that accepts
a dialer and discards it, to make the two look alike. A signature that lies is
worse than a documented asymmetry.

**See:** Design 052 (runtime-neutral connectors) for the trait set tiers 1 and 2
build on.

---

## Implementation Pattern

Write **one** connector, generic over core's I/O traits. The adapter owns
sockets, clocks and channels; the connector owns framing, protocol logic and
sugar. There is no `tokio_*` / `embassy_*` module and no runtime `cfg` on the
code path — a new platform is one adapter crate and zero connector edits.

**Features name the environment, not the runtime.** The real split is std vs
`no_std`: a `no_std` connector runs under Embassy, FreeRTOS or a host test
alike. Keep runtime names for convenience bundles only.

```toml
[features]
# The std backend, if the protocol library is tier 3 and std-only.
std = ["aimdb-core/std", "protocol-client-crate"]
# The neutral backend: `alloc` only, no executor and no network stack.
embedded = ["aimdb-core/alloc", "aimdb-core/connector-session"]
# Convenience: `embedded` plus one adapter's transports.
embassy-runtime = ["embedded", "aimdb-embassy-adapter/net"]
```

**Session transport** (a framed byte stream — serial, TCP): contribute a
`Framer` and let core's `FramedConnection` / `FramingDialer` / `FramingListener`
do the rest over the adapter's `StreamDialer` or `StreamListener`.

**Data-plane transport** (a pub/sub channel — MQTT, KNX): implement core's
`Connector` (outbound) and `Source` (inbound) over an
`embassy_sync::channel::Channel<CriticalSectionRawMutex, _, N>`, then ride
`pump_sink` / `pump_source`. `CriticalSectionRawMutex` is what makes the
channel `Sync`, and therefore what lets these be plain impls with no
force-`Send` wrapper. It is a link-time obligation on std: enable
`critical-section/std` from your own feature so no std user meets the
undefined-symbol error.

**Time:** take core's `Delay` rather than a runtime timer. `RuntimeOps::sleep`
is `dyn` and boxes per call, which a poll loop cannot afford; `Delay` is
generic and allocates nothing. The clock for elapsed time stays
`RuntimeOps::now_nanos()`, and wall-clock time is `RuntimeOps::unix_time()`.

### The `Send` rule, and its one escape hatch

`ConnectorBuilder::build` returns `Send` futures, so **every trait a generic
connector task calls through needs `+ Send` on its return type** — not just
core's. A bare `async fn` in your own trait will not do it:

```rust
-    async fn send(&mut self, frame: &[u8]) -> bool;
+    fn send(&mut self, frame: &[u8]) -> impl Future<Output = bool> + Send;
```

That fixes every trait you own. It cannot fix a **foreign** trait: nothing adds
a bound to `embedded_io_async::Read`, and a generic parameter hides whether the
concrete future is `Send`. Expressing it needs return-type notation, which is
not stable on the pinned toolchain. Where that bites, the choices are a
documented `unsafe impl Send` on the task future — sound when the trait bounds
already guarantee every held value is `Send`, as `StreamDialer`'s
`Stream: Send` does — or type-erasing the stream behind `dyn` and paying an
allocation per read. Prefer the first, at exactly one site, with the
justification written down; see `aimdb-mqtt-connector`'s `SendSession`.

Moved-in resources go in `aimdb_core::session::OneShot<T>`, which is
`Send + Sync` for `T: Send` without `unsafe`. If it refuses your type, fix the
type — a missing `+ Send` on a trait object, usually — rather than forcing the
bound.

**See:** `aimdb-serial-connector` (session), `aimdb-mqtt-connector` /
`aimdb-knx-connector` (data-plane), and `examples/embassy-mqtt-connector-demo/`.

---

## Common Pitfalls

**Missing outbound publishers:**
```rust
// ❌ Wrong - no data flows out
connector.build(db).await

// ✅ Correct - spawn publishers
let routes = db.collect_outbound_routes(scheme);
connector.spawn_outbound_publishers(db, routes)?;
```

**Not using Router:**
```rust
// ❌ Manual routing
if topic == "sensor/temp" { temp_producer.send(data).await; }

// ✅ Router handles it
router.route(topic, data).await?;
```

**A channel that cannot cross a thread:**
```rust
// ❌ `NoopRawMutex` is !Sync, so the sink and source need a force-`Send`
//    wrapper and the whole connector is welded to a single-core executor.
static CH: StaticCell<Channel<NoopRawMutex, Action, N>> = StaticCell::new();

// ✅ `CriticalSectionRawMutex` is Send + Sync, so `Connector`/`Source` are
//    plain impls. `Arc` over `StaticCell` allows several connectors per
//    process; `StaticCell` is still right for one-connector firmware.
let actions = Arc::new(Channel::<CriticalSectionRawMutex, Action, N>::new());
```

**Process-global state where per-connector state belongs:**
```rust
// ❌ The second connector silently connects as the first
static CLIENT_ID: OnceLock<String> = OnceLock::new();
let id: &'static str = CLIENT_ID.get_or_init(|| client_id.to_string());

// ✅ One small leak per connector, at build
let id: &'static str = Box::leak(client_id.to_string().into_boxed_str());
```

---

## Reference Implementation

**MQTT Connector:** `aimdb-mqtt-connector/` - Complete production reference

**Working Examples:**
- `examples/tokio-mqtt-connector-demo/` - Tokio runtime
- `examples/embassy-mqtt-connector-demo/` - Embassy runtime

**Documentation:**
- [Architecture Overview](./architecture.md)
- [Router Design](./router.md)
- [Producer-Consumer Pattern](./producer-consumer.md)

---

**Note:** Always refer to the MQTT connector implementation in `aimdb-mqtt-connector/` for complete, tested patterns. It demonstrates all the concepts in this guide for both Tokio and Embassy runtimes.

### 1. **Maintain API Parity**

✅ **Do**: Same user-facing API
```rust
// Works with both Tokio and Embassy
.with_connector(MyConnectorBuilder::new("url"))
```

❌ **Don't**: Different APIs per runtime
```rust
// Bad - different APIs
.with_tokio_connector(...)
.with_embassy_connector(...)
```

### 2. **Handle Reconnection in Background Task**

Reconnection logic belongs in the spawned event loop task, **not** in `publish()`.

✅ **Do**: Reconnect in background task
```rust
// In spawned background task
tokio::spawn(async move {
    loop {
        match connect_and_run(&broker_url).await {
            Ok(_) => { /* Connection closed gracefully */ }
            Err(e) => {
                eprintln!("Connection failed: {:?}, reconnecting...", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
});
```

❌ **Don't**: Block publish() waiting for reconnection
```rust
fn publish(...) {
    // Bad - blocks all publishers
    if !self.connected {
        reconnect().await?;
    }
    client.publish(...).await
}
```

**Why?** The background task maintains the connection continuously. The `publish()` method should fail fast if disconnected, letting the application decide how to handle it.

### 3. **Use Router for Inbound Messages**

```rust
// Automatically routes to correct producer
router.route(topic, payload).await?;
```

### 4. **Subscribe to Router Topics**

```rust
let topics = router.resource_ids();
for topic in &topics {
    client.subscribe(topic).await?;
}
```

### 5. **Proper Error Handling**

```rust
// Convert protocol errors to PublishError
.map_err(|e| {
    eprintln!("Protocol error: {:?}", e);
    PublishError::ConnectionFailed
})
```

### 6. **Explicit Outbound Publisher Spawning**

Always implement and call `spawn_outbound_publishers()` in `ConnectorBuilder::build()`:

✅ **Do**: Spawn publishers explicitly
```rust
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R: RuntimeAdapter + 'static>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        // ... setup code ...
        
        let connector = MyConnector { /* fields */ };
        
        // Collect and spawn outbound routes
        let outbound_routes = db.collect_outbound_routes(self.protocol_name());
        connector.spawn_outbound_publishers(db, outbound_routes)?;
        
        Ok(Arc::new(connector))
    }
}
```

❌ **Don't**: Forget to spawn outbound publishers
```rust
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R: RuntimeAdapter + 'static>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        // ... setup code ...
        
        // Missing: No outbound publisher spawning!
        Ok(Arc::new(MyConnector { /* fields */ }))
    }
}
```

**Why?** Outbound publishers consume from AimDB records and publish to external systems. Without explicit spawning, records configured with `.link_to()` won't actually send data.

### 7. **Logging Strategy**

**Tokio (std):**
```rust
use tracing::{info, warn, error};
info!("Connected to broker");
```

**Embassy (no_std):**
```rust
#[cfg(feature = "defmt")]
defmt::info!("Connected to broker");
```

### 7. **Static Allocation for Embassy**

```rust
// Good - uses StaticCell
static CHANNEL: StaticCell<Channel<...>> = StaticCell::new();

// Bad - stack allocation won't work
let channel = Channel::new(); // Doesn't live long enough
```

### 8. **Quality of Service Configuration**

Pass through `ConnectorConfig` to support protocol-specific options:

```rust
impl Connector for MyConnectorImpl {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,  // ← Use this!
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Extract QoS, retain, timeout, etc.
        let qos = config.qos.unwrap_or(0);
        let retain = config.retain.unwrap_or(false);
        
        // Pass to protocol client
        client.publish(destination, qos, retain, payload).await
    }
}
```

Users configure it per link:
```rust
.link_to("mqtt://sensors/temp")
.with_config(ConnectorConfig {
    qos: Some(2),        // Exactly-once delivery
    retain: Some(true),  // Keep last message
})
```

---

## Connector Implementation Checklist

- [ ] Create crate with `std` and `embedded` features (runtime names are bundles)
- [ ] Implement `ConnectorBuilder<R>` trait with `build()` and `scheme()`
- [ ] Implement `Connector` trait with `publish()`
- [ ] In `build()`: Collect inbound routes via `db.collect_inbound_routes(scheme)`
- [ ] In `build()`: Build `Router` from inbound routes
- [ ] In `build()`: Create protocol client instance
- [ ] In `build()`: Spawn background task for connection management
- [ ] In `build()`: Spawn inbound event router task
- [ ] In `build()`: Subscribe client to all router topics
- [ ] In `build()`: Collect outbound routes via `db.collect_outbound_routes(scheme)`
- [ ] In `build()`: Call `spawn_outbound_publishers(db, outbound_routes)`
- [ ] Implement `spawn_outbound_publishers()` method
- [ ] Implement reconnection logic in background task
- [ ] Add comprehensive error handling and logging
- [ ] Test cross-compilation for embedded targets (if supporting Embassy)

---

## Common Pitfalls

### 1. **Forgetting to Spawn Background Tasks**

❌ **Wrong:**
```rust
// Task is never spawned - connector won't work!
async fn background_task() { /* ... */ }
```

✅ **Correct:**
```rust
db.runtime().spawn(SendFutureWrapper(async move {
    background_task().await
}));
```

### 2. **Not Using Router for Inbound Messages**

❌ **Wrong:**
```rust
// Manually finding producers - doesn't scale
if topic == "sensor/temp" {
    temp_producer.produce(payload).await?;
}
```

✅ **Correct:**
```rust
// Router automatically finds the right producer
router.route(topic, payload).await?;
```

### 3. **Missing SendFutureWrapper in Embassy**

❌ **Wrong:**
```rust
Box::pin(async move { /* ... */ })
// Error: future is not `Send`
```

✅ **Correct:**
```rust
Box::pin(SendFutureWrapper(async move { /* ... */ }))
```

### 4. **Stack Allocation in Embassy**

❌ **Wrong:**
```rust
let channel = Channel::new(); // Doesn't live long enough
```

✅ **Correct:**
```rust
static CHANNEL: StaticCell<Channel<...>> = StaticCell::new();
let channel = CHANNEL.init(Channel::new());
```

### 5. **Forgetting to Spawn Outbound Publishers**

❌ **Wrong:**
```rust
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        let connector = MyConnector { /* ... */ };
        // Missing spawn_outbound_publishers!
        Ok(Arc::new(connector))
    }
}
// Result: Records linked with .link_to() don't publish data
```

✅ **Correct:**
```rust
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        let connector = MyConnector { /* ... */ };
        
        // Always spawn outbound publishers
        let routes = db.collect_outbound_routes(self.protocol_name());
        connector.spawn_outbound_publishers(db, routes)?;
        
        Ok(Arc::new(connector))
    }
}
```

**Symptom:** Inbound messages work (external → AimDB), but outbound messages fail silently (AimDB → external). Records configured with `.link_to("mqtt", "topic")` don't send data.

**Why?** The `ConsumerTrait`-based outbound routing requires explicit spawning. Unlike inbound routing (which works via `Router`), outbound publishers must be spawned during connector build.

---

## Resources

### AimDB Documentation
- [Architecture Overview](./architecture.md)
- [Router Design](./router.md)
- [Producer-Consumer Pattern](./producer-consumer.md)
- [Embassy Integration](./embassy-integration.md)

### Example Connectors
- [MQTT Connector](../../aimdb-mqtt-connector/) - Complete reference implementation
- [Tokio MQTT Example](../../examples/tokio-mqtt-connector-demo/)
- [Embassy MQTT Example](../../examples/embassy-mqtt-connector-demo/)

### External References
- [Embassy Documentation](https://embassy.dev/)
- [Tokio Documentation](https://tokio.rs/)
- [Rust Embedded Book](https://docs.rust-embedded.org/)

---

## Support

For questions or contributions:
- **GitHub Issues**: https://github.com/aimdb-dev/aimdb/issues
- **Discussions**: https://github.com/aimdb-dev/aimdb/discussions
- **Examples**: See `examples/` directory

---

**Happy Connector Building!** 🚀

The MQTT connector implementation serves as the reference for all patterns described in this guide. When in doubt, refer to `aimdb-mqtt-connector/` for a complete, production-ready example.
