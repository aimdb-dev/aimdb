# Bidirectional Connector Trait Enhancement

**Goal**: Extend the `Connector` trait to support **bidirectional** data flow (inbound from external systems → AimDB, not just outbound AimDB → external systems).

**Why**: Multiple connectors need inbound support:
- **KNX**: Listen to building automation bus → push to AimDB
- **MQTT**: Subscribe to broker topics → push to AimDB  
- **Kafka**: Consume from topics → push to AimDB
- **WebSocket**: Receive messages → push to AimDB

**Impact**: Foundational change that unblocks multiple connector implementations.

## Key Design Decisions (Finalized)

✅ **Stream Trait**: Use `futures-core::Stream` (minimal, standard, future-proof)  
✅ **Embassy Consistency**: Implement `ChannelStream` adapter (30 lines, unified API)  
✅ **Breaking Change**: Remove `.link()`, replace with `.link_to()` / `.link_from()`  
✅ **No Backward Compat**: Pre-1.0 project, clean API takes priority

---

## Current State

### Existing Trait (Outbound Only)

```rust
// aimdb-core/src/transport.rs
pub trait Connector: Send + Sync {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>>;
}
```

**Current Capabilities:**
- ✅ Outbound: AimDB buffer → `.link()` → external system
- ❌ Inbound: External system → ??? → AimDB buffer (missing)

### Current Builder API

```rust
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link("mqtt://sensors/temp")           // Outbound only, no direction indicator
           .with_serializer(|t| serialize(t))  // Only serialization supported
           .finish()
});
```

**Gaps:**
- No inbound data flow support
- No deserializer API
- Unclear direction (`.link()` doesn't indicate outbound)
- No mechanism to spawn inbound producer tasks

---

## Proposed Design

### 1. Enhanced Connector Trait

```rust
// aimdb-core/src/transport.rs
use futures_core::Stream;
use core::pin::Pin;
use core::task::{Context, Poll};

pub trait Connector: Send + Sync {
    /// Publish data to external system (outbound: AimDB → External)
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>>;

    /// Subscribe to data from external system (inbound: External → AimDB)
    /// 
    /// Returns a stream of raw bytes from the external system.
    /// Each item represents a message/event that should be deserialized and
    /// published to an AimDB record.
    ///
    /// **Default implementation**: Returns empty stream (no inbound support).
    /// Connectors that don't support inbound can use the default.
    fn subscribe(
        &self,
        source: &str,
        config: &ConnectorConfig,
    ) -> Pin<Box<dyn Stream<Item = Result<Vec<u8>, PublishError>> + Send + '_>> {
        // Empty stream implementation (no dependencies on futures-util)
        struct EmptyStream;
        impl Stream for EmptyStream {
            type Item = Result<Vec<u8>, PublishError>;
            fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                Poll::Ready(None)  // Stream immediately ends
            }
        }
        Box::pin(EmptyStream)
    }
}
```

**Design Rationale:**

| Aspect | Choice | Reason |
|--------|--------|--------|
| **Stream Trait** | `futures-core::Stream` | Standard Rust async trait, minimal dependency (~20KB), `no_std` compatible |
| **Default Impl** | Empty stream | Backward compatible, connectors can opt-in to inbound |
| **Error Type** | `Result<Vec<u8>, PublishError>` | Reuse existing error type, stream can report connection failures |
| **Raw Bytes** | `Vec<u8>` | Protocol-agnostic, deserialization happens in AimDB layer |

**Dependency Addition:**
```toml
# aimdb-core/Cargo.toml
[dependencies]
futures-core = { version = "0.3", default-features = false }
```

**Future-Proofness Analysis:**

| Protocol | Tokio Implementation | Embassy Implementation |
|----------|---------------------|------------------------|
| **MQTT** | `rumqttc::EventLoop` → `Stream` wrapper | `mountain-mqtt` channel → `ChannelStream` |
| **KNX** | UDP socket loop → `Stream` | UDP socket loop → `ChannelStream` |
| **Kafka** | `rdkafka` consumer → `Stream` | N/A (std only) |
| **DDS** | Discovery reader → `Stream` | N/A (std only) |

All implementations are straightforward and follow standard Rust async patterns.

---

### 2. Builder API: Explicit Direction

**New API:**
```rust
// aimdb-core/src/typed_api.rs

impl<'a, T, R> RecordRegistrar<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Link FROM external system (inbound: External → AimDB)
    /// 
    /// Subscribes to an external data source and produces values into this record's buffer.
    ///
    /// # Example
    /// ```rust,ignore
    /// builder.configure::<LightState>(|reg| {
    ///     reg.buffer(BufferCfg::SingleLatest)
    ///        .link_from("mqtt://broker/lights/+/state")
    ///            .with_deserializer(|bytes| parse_light_state(bytes))
    ///            .finish()
    /// });
    /// ```
    pub fn link_from(&'a mut self, url: &str) -> InboundConnectorBuilder<'a, T, R> {
        InboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            deserializer: None,
        }
    }

    /// Link TO external system (outbound: AimDB → External)
    /// 
    /// Subscribes to buffer updates and publishes them to an external system.
    ///
    /// # Example
    /// ```rust,ignore
    /// builder.configure::<Temperature>(|reg| {
    ///     reg.buffer(BufferCfg::SingleLatest)
    ///        .link_to("mqtt://broker/sensors/temp")
    ///            .with_serializer(|t| serde_json::to_vec(t).unwrap())
    ///            .finish()
    /// });
    /// ```
    pub fn link_to(&'a mut self, url: &str) -> OutboundConnectorBuilder<'a, T, R> {
        OutboundConnectorBuilder {
            registrar: self,
            url: url.to_string(),
            config: Vec::new(),
            serializer: None,
        }
    }
}
```

**Breaking Change:**
- ❌ **Removed**: `.link()` method
- ✅ **Added**: `.link_from()` for inbound
- ✅ **Added**: `.link_to()` for outbound

**Migration:**
```rust
// Before (v0.1.x) - REMOVED
.link("mqtt://topic").with_serializer(...)

// After (v0.2.0) - Clear direction
.link_to("mqtt://topic").with_serializer(...)
.link_from("mqtt://topic").with_deserializer(...)
```

---

### 3. Embassy Runtime Consistency: `ChannelStream` Adapter

**Challenge**: Embassy's `mountain-mqtt` uses channels, not `Stream`:
```rust
// mountain-mqtt-embassy pattern
let receiver: Receiver<MqttEvent<MyEvent>> = ...;
loop {
    let event = receiver.receive().await;  // Not a Stream!
}
```

**Solution**: Implement `Stream` trait for Embassy channels:

```rust
// aimdb-embassy-adapter/src/stream.rs
use embassy_sync::channel::Receiver;
use futures_core::Stream;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Adapter that implements Stream for Embassy channel receivers
pub struct ChannelStream<'a, M, T, const N: usize> {
    receiver: Receiver<'a, M, T, N>,
}

impl<'a, M, T, const N: usize> ChannelStream<'a, M, T, N> {
    pub fn new(receiver: Receiver<'a, M, T, N>) -> Self {
        Self { receiver }
    }
}

impl<'a, M, T, const N: usize> Stream for ChannelStream<'a, M, T, N>
where
    M: embassy_sync::blocking_mutex::raw::RawMutex,
    T: Clone,
{
    type Item = T;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        // Poll the channel's receive future
        match Pin::new(&mut self.receiver.receive()).poll(cx) {
            Poll::Ready(value) => Poll::Ready(Some(value)),
            Poll::Pending => Poll::Pending,
        }
        // Note: Embassy channels never close, so we never return None
    }
}
```

**Usage in Embassy MQTT Connector:**
```rust
impl Connector for MqttConnector {
    fn subscribe(&self, _source: &str, _config: &ConnectorConfig) 
        -> Pin<Box<dyn Stream<Item = Result<Vec<u8>, PublishError>> + Send + '_>> 
    {
        let receiver = self.event_receiver.clone();
        
        // Wrap channel in Stream adapter and filter events
        let stream = ChannelStream::new(receiver)
            .filter_map(|event| match event {
                MqttEvent::ApplicationEvent { payload, .. } => Some(Ok(payload)),
                MqttEvent::Disconnected { error } => Some(Err(error.into())),
                _ => None,  // Ignore Connected, etc.
            });
        
        Box::pin(stream)
    }
}
```

**Benefits:**
- ✅ **Unified API**: Same `Connector` trait for Tokio and Embassy
- ✅ **Minimal Code**: ~30 lines, reusable across all Embassy connectors
- ✅ **Zero Runtime Cost**: Direct poll forwarding, no buffering
- ✅ **Maintains Philosophy**: "Write once, run everywhere"

---

### 4. Inbound Connector Builder

```rust
/// Builder for inbound connector links (External → AimDB)
pub struct InboundConnectorBuilder<
    'a,
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
> {
    registrar: &'a mut RecordRegistrar<'a, T, R>,
    url: String,
    config: Vec<(String, String)>,
    deserializer: Option<TypedDeserializerFn<T>>,
}

/// Type alias for deserializer function
type TypedDeserializerFn<T> = Arc<dyn Fn(&[u8]) -> Result<T, String> + Send + Sync>;

impl<'a, T, R> InboundConnectorBuilder<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Sets a deserialization callback
    pub fn with_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(&[u8]) -> Result<T, String> + Send + Sync + 'static,
    {
        self.deserializer = Some(Arc::new(f));
        self
    }

    /// Adds a configuration option
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_string(), value.to_string()));
        self
    }

    /// Finalizes the inbound connector registration
    pub fn finish(self) -> &'a mut RecordRegistrar<'a, T, R> {
        let url = ConnectorUrl::parse(&self.url)
            .unwrap_or_else(|_| panic!("Invalid connector URL: {}", self.url));

        let scheme = url.scheme().to_string();

        if let (Some(deserializer), Some(connector)) =
            (self.deserializer, self.registrar.connectors.get(&scheme))
        {
            // Store inbound connector info for spawning during build()
            let inbound_link = InboundConnectorLink {
                url,
                config: self.config,
                deserializer_erased: Arc::new(move |bytes| {
                    deserializer(bytes).map(|val| Box::new(val) as Box<dyn Any + Send>)
                }),
            };

            self.registrar.rec.add_inbound_connector(inbound_link);
        } else {
            panic!("Inbound connector requires deserializer and registered connector for scheme: {}", scheme);
        }

        self.registrar
    }
}
```

---

---

### 5. Task Spawning for Inbound Connectors

**Location**: `TokioAdapter::spawn_connector_tasks()` / `EmbassyAdapter::spawn_connector_tasks()`

```rust
// For each record with inbound connectors
for inbound_link in record.inbound_connectors() {
    let connector = connectors.get(inbound_link.url.scheme())
        .expect("Connector validated during build");
    
    // Get stream from connector
    let mut stream = connector.subscribe(
        inbound_link.url.path(),
        &inbound_link.to_connector_config(),
    );

    // Get producer handle to push values into AimDB
    let producer = db.producer::<T>();
    let deserializer = inbound_link.deserializer.clone();

    // Spawn background task that bridges stream → producer
    runtime.spawn(async move {
        use futures::StreamExt;  // For .next()
        
        while let Some(result) = stream.next().await {
            match result {
                Ok(bytes) => {
                    match deserializer(&bytes) {
                        Ok(value) => {
                            if let Err(e) = producer.produce(value).await {
                                tracing::error!("Inbound connector produce failed: {:?}", e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Inbound connector deserialize failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Inbound connector stream error: {:?}", e);
                    // Connector should handle reconnection, we just log
                }
            }
        }
        
        tracing::info!("Inbound connector stream ended");
    });
}
```

**New API Required**:
```rust
impl<R: Spawn> AimDb<R> {
    /// Create a producer handle for spawning inbound connector tasks
    pub fn producer<T: Send + 'static + Debug + Clone>(&self) -> Producer<T, R> {
        Producer::new(Arc::clone(&self.inner))
    }
}
```

---

## Implementation Plan

### Phase 1: Core Infrastructure (Week 1)

**1.1 Trait Enhancement**
- [ ] Add `futures-core` dependency to `aimdb-core`
- [ ] Add `subscribe()` method to `Connector` trait with empty stream default
- [ ] Add empty stream struct to avoid `futures-util` dependency
- [ ] Unit tests for default implementation

**1.2 Type System**
- [ ] Add `InboundConnectorLink` struct to `aimdb-core/src/connector.rs`
- [ ] Add `TypedDeserializerFn<T>` and type erasure support
- [ ] Add `inbound_connectors` field to `TypedRecord`
- [ ] Add `add_inbound_connector()` method

**1.3 Embassy Stream Adapter**
- [ ] Create `aimdb-embassy-adapter/src/stream.rs`
- [ ] Implement `ChannelStream<'a, M, T, N>` struct
- [ ] Implement `Stream` trait for `ChannelStream`
- [ ] Unit tests for `ChannelStream`
- [ ] Documentation for connector authors

### Phase 2: Builder API (Week 1-2)

**2.1 API Changes**
- [ ] Remove `.link()` method from `RecordRegistrar`
- [ ] Implement `.link_from()` → `InboundConnectorBuilder`
- [ ] Implement `.link_to()` → `OutboundConnectorBuilder` (rename from link)
- [ ] Implement `InboundConnectorBuilder::with_deserializer()`
- [ ] Implement `InboundConnectorBuilder::finish()`

**2.2 Validation**
- [ ] Validate buffer exists for `link_from()` records
- [ ] Validate deserializer provided for `link_from()`
- [ ] Validate connector registered for scheme
- [ ] Error messages for common mistakes

**2.3 Testing**
- [ ] Unit tests for builder API
- [ ] Test type erasure and downcasting
- [ ] Test configuration parsing

### Phase 3: Task Spawning (Week 2)

**3.1 Producer API**
- [ ] Add `AimDb::producer<T>()` method
- [ ] Test producer creation and usage

**3.2 Tokio Spawning**
- [ ] Extend `TokioAdapter::spawn_connector_tasks()` for inbound
- [ ] Spawn tasks that poll streams and call producer
- [ ] Error handling and logging
- [ ] Lifecycle management (shutdown on drop)

**3.3 Embassy Spawning**
- [ ] Extend `EmbassyAdapter::spawn_connector_tasks()` for inbound
- [ ] Use `ChannelStream` for Embassy connectors
- [ ] Test with Embassy executor

**3.4 Integration Tests**
- [ ] Mock connector with bidirectional support
- [ ] Stream lifecycle tests (spawn, run, cancel)
- [ ] Error handling tests (deserialize failure, stream error)
- [ ] Concurrent inbound/outbound on same record

### Phase 4: MQTT Validation (Week 2-3)

**4.1 Tokio MQTT**
- [ ] Implement `subscribe()` in `MqttConnector` using `rumqttc::EventLoop`
- [ ] Stream wrapper for EventLoop notifications
- [ ] Filter and map to raw bytes

**4.2 Embassy MQTT**
- [ ] Implement `subscribe()` using `ChannelStream` adapter
- [ ] Wrap `mountain-mqtt` event channel
- [ ] Filter MqttEvent types

**4.3 Examples**
- [ ] MQTT publish example (existing, update to use `.link_to()`)
- [ ] MQTT subscribe example (new, uses `.link_from()`)
- [ ] Bidirectional example (MQTT → process → MQTT)

**4.4 End-to-End Testing**
- [ ] Test against real MQTT broker
- [ ] Validate both Tokio and Embassy implementations
- [ ] Performance testing

### Phase 5: Documentation & Migration (Week 3)

**5.1 API Documentation**
- [ ] Document `Connector::subscribe()` with examples
- [ ] Document `ChannelStream` adapter for connector authors
- [ ] Document `link_from()` / `link_to()` with examples
- [ ] Architecture diagrams (inbound vs outbound flow)

**5.2 Migration Guide**
- [ ] Document breaking changes
- [ ] Provide before/after examples
- [ ] Update all existing examples to use `.link_to()`
- [ ] Update README and quickstart guide

**5.3 Connector Author Guide**
- [ ] How to implement `subscribe()` for new protocols
- [ ] How to use `ChannelStream` in Embassy
- [ ] Best practices for error handling
- [ ] Performance considerations

---

## Testing Strategy

### Unit Tests
- ✅ `Connector` trait default implementation (empty stream)
- ✅ `ChannelStream` adapter poll semantics
- ✅ Builder API (`.link_from()`, `.link_to()`)
- ✅ Type erasure (deserializer downcast from `Box<dyn Any>`)
- ✅ Configuration parsing and validation
- ✅ Producer handle creation

### Integration Tests
- ✅ Mock connector with bidirectional support
- ✅ Stream lifecycle (spawn task, receive values, task ends)
- ✅ Error handling (deserialize failure, stream error, connector error)
- ✅ Concurrent inbound/outbound on same record
- ✅ Multiple consumers on single inbound stream
- ✅ Backpressure handling

### End-to-End Tests (MQTT)
- ✅ Tokio: MQTT subscribe → deserialize → produce to buffer
- ✅ Embassy: MQTT subscribe via `ChannelStream` → produce to buffer
- ✅ Bidirectional: MQTT subscribe → process → MQTT publish
- ✅ Multi-topic: Subscribe to wildcard topics, filter by prefix
- ✅ Error recovery: Broker disconnect, automatic reconnect

### Performance Tests
- ✅ Stream overhead (raw channel vs `ChannelStream`)
- ✅ Deserializer performance (JSON, binary)
- ✅ Throughput: Messages/second for inbound connectors
- ✅ Latency: External event → AimDB buffer (target: <10ms)

---

## Migration Path

### Breaking Changes (v0.2.0)
- ❌ **Removed**: `.link()` method (replaced by `.link_to()`)
- ✅ **Added**: `.link_from()` for inbound data flow
- ✅ **Added**: `.link_to()` for outbound data flow (explicit naming)

### Migration Required
All existing code using `.link()` must be updated:

```rust
// Before (v0.1.x)
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link("mqtt://sensors/temp")  // ❌ Removed
           .with_serializer(|t| serialize(t))
           .finish()
});

// After (v0.2.0)
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link_to("mqtt://sensors/temp")  // ✅ Explicit direction
           .with_serializer(|t| serialize(t))
           .finish()
});
```

**Rationale**: Pre-1.0 project, no backward compatibility guarantees. Clear API is more important than migration burden.

---

## Benefits

✅ **Unified API**: Same `Connector` trait works for Tokio and Embassy  
✅ **Type Safety**: Compile-time type checking for deserializers  
✅ **Backward Compatible Trait**: Default `subscribe()` impl maintains existing connectors  
✅ **Protocol Agnostic**: Works for MQTT, KNX, Kafka, WebSocket, DDS, etc.  
✅ **Lifecycle Management**: Connectors handle reconnection, AimDB spawns and manages tasks  
✅ **Self-Documenting API**: `.link_from()` / `.link_to()` makes direction explicit  
✅ **Minimal Dependencies**: `futures-core` is trait-only (~20KB)  
✅ **no_std Compatible**: Works in embedded environments with `alloc`

---

## Open Questions & Decisions

### 1. Stream Error Handling

**Question**: When a stream returns `Err(PublishError)`, should AimDB automatically retry or leave it to the connector?

**Recommendation**: Connector handles reconnection, AimDB only logs errors.

**Rationale**:
- Connectors know their protocol's reconnection semantics
- MQTT has built-in reconnect logic (rumqttc, mountain-mqtt)
- KNX may need different backoff strategies than MQTT
- AimDB would need protocol-specific retry configuration (complex)

**Implementation**: Stream errors are logged via `tracing::error!`, task continues polling.

---

### 2. Backpressure & Flow Control

**Question**: What happens if deserializer is slow and stream fills up?

**Recommendation**: Use bounded channels at connector level, document capacity tuning.

**Rationale**:
- Each connector can choose appropriate buffer size
- MQTT QoS 0: Drop old messages (ring buffer)
- MQTT QoS 1/2: Bounded channel with backpressure to broker
- KNX: Buffer last N telegrams, drop oldest

**Implementation**: Connectors document their buffering strategy, users can configure via connector construction.

---

### 3. Multiple Inbound Subscriptions

**Question**: Can multiple records `link_from()` the same connector source?

**Recommendation**: Yes, connector broadcasts to all subscribers.

**Examples**:
```rust
// Both records receive the same MQTT messages
builder.configure::<RawMessage>(|reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
       .link_from("mqtt://broker/sensors/#")
           .with_deserializer(|b| Ok(RawMessage { bytes: b.to_vec() }))
           .finish()
});

builder.configure::<ParsedMessage>(|reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link_from("mqtt://broker/sensors/#")
           .with_deserializer(|b| parse_json(b))
           .finish()
});
```

**Implementation**: Each `link_from()` calls `connector.subscribe()` independently, connector returns cloned/broadcast stream.

---

### 4. Graceful Shutdown

**Question**: How to stop inbound connector tasks when database is dropped?

**Recommendation**: Use `tokio::task::JoinHandle` / Embassy task cancellation.

**Implementation**:
```rust
// Store join handles in AimDb
struct AimDbInner<R> {
    // ... existing fields
    inbound_tasks: Vec<JoinHandle<()>>,  // Tokio
}

impl<R> Drop for AimDbInner<R> {
    fn drop(&mut self) {
        for handle in &self.inbound_tasks {
            handle.abort();  // Cancel task
        }
    }
}
```

Embassy: Tasks are automatically cancelled when executor shuts down.  

---

## Related Issues

**Unblocked by this design**:
- KNX Connector Implementation (see [aimdb-knx-connector-design.md](./aimdb-knx-connector-design.md))
- MQTT Subscribe Support (currently only publish)
- Kafka Consumer Integration (planned)
- WebSocket Bidirectional Bridge (planned)

**Related Design Docs**:
- [KNX Connector Design](./aimdb-knx-connector-design.md) - Section 8 discusses bidirectional needs
- [API Refactor: source/tap/link](./004-M2-api-refactor-source-tap-link.md) - Original API design

---

## Summary

This design enhances the `Connector` trait with bidirectional support using a pragmatic, proven approach:

1. **`futures-core::Stream`**: Standard, minimal, no_std compatible
2. **`ChannelStream` Adapter**: Bridges Embassy channels to Stream (30 lines)
3. **Explicit Direction**: `.link_from()` and `.link_to()` replace ambiguous `.link()`
4. **Breaking Change Accepted**: Pre-1.0, clean API is priority

**Timeline**: 2-3 weeks with clear milestones and incremental validation.

**Risk**: Low. All components use standard Rust patterns and are individually testable.

**Next Step**: Begin Phase 1 implementation.
