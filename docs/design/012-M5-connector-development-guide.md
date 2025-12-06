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

## Tokio Implementation Pattern

**Dependencies:**
```toml
[features]
tokio-runtime = ["std", "tokio", "protocol-client-crate"]

[dependencies]
tokio = { workspace = true, optional = true }
# Add protocol-specific client library
```

**Key patterns:**
- Use `std` types: `std::sync::Arc`, `std::string::String`
- Spawn: `tokio::spawn(async move { ... })`
- Logging: `tracing::{info, warn, error}`
- Async client libraries (e.g., `rumqttc`)

**See:** `aimdb-mqtt-connector/` for complete Tokio implementation

---

## Embassy Implementation Pattern

**Dependencies:**
```toml
[features]
embassy-runtime = ["aimdb-core/alloc", "embassy-net", "embassy-sync"]

[dependencies]
embassy-net = { workspace = true, optional = true }
embassy-sync = { workspace = true, optional = true }
static-cell = "2.0"
```

**Key patterns:**
- Use `alloc` types: `alloc::sync::Arc`, `alloc::string::String`
- Wrap futures: `SendFutureWrapper(async move { ... })`
- Static allocation: `StaticCell<T>`
- Logging: `defmt::info!()` (behind `#[cfg(feature = "defmt")]`)
- Network access: `R: EmbassyNetwork` trait bound
- Unsafe `Send + Sync` impls for single-threaded safety

**SendFutureWrapper helper:**
```rust
struct SendFutureWrapper<F>(F);
unsafe impl<F> Send for SendFutureWrapper<F> {}

impl<F: Future> Future for SendFutureWrapper<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) 
        -> core::task::Poll<Self::Output> 
    {
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}
```

**See:** `examples/embassy-mqtt-connector-demo/` for Embassy patterns

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

**Embassy lifetime issues:**
```rust
// ❌ Stack allocation
let channel = Channel::new();

// ✅ Static allocation
static CH: StaticCell<Channel<...>> = StaticCell::new();
let ch = CH.init(Channel::new());
```

**Missing Send wrapper (Embassy):**
```rust
// ❌ Not Send
Box::pin(async move { ... })

// ✅ Send-wrapped
Box::pin(SendFutureWrapper(async move { ... }))
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

- [ ] Create crate with `tokio-runtime` and `embassy-runtime` features
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
