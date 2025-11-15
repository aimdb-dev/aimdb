# AimDB Connector Development Guide

**Version:** 0.1.0  
**Last Updated:** November 15, 2025  
**Status:** Complete

## Overview

This guide walks you through creating custom connectors for AimDB. Connectors enable bidirectional data synchronization between AimDB and external systems like message brokers (MQTT, Kafka), industrial protocols (KNX, DDS), databases, APIs, and more.

AimDB provides a **dual runtime architecture**:
- **Tokio (std)**: For servers, edge devices, and cloud deployments
- **Embassy (no_std)**: For embedded MCUs and resource-constrained environments

This guide shows you how to build connectors that work in **both environments** while maintaining **API parity** - the same user-facing API regardless of runtime.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Connector Lifecycle](#connector-lifecycle)
3. [Step-by-Step Tutorial](#step-by-step-tutorial)
4. [Tokio Implementation](#tokio-implementation)
5. [Embassy Implementation](#embassy-implementation)
6. [Testing and Validation](#testing-and-validation)
7. [Best Practices](#best-practices)
8. [Real-World Examples](#real-world-examples)

---

## Architecture Overview

### Core Concepts

**ConnectorBuilder Trait**
```rust
pub trait ConnectorBuilder<R>: Send + Sync
where
    R: aimdb_executor::Spawn + 'static,
{
    /// Build the connector using the database
    fn build<'a>(
        &'a self,
        db: &'a AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = DbResult<Arc<dyn Connector>>> + Send + 'a>>;

    /// The URL scheme this connector handles (e.g., "mqtt", "kafka", "knx")
    fn scheme(&self) -> &str;
}
```

**Connector Trait**
```rust
pub trait Connector: Send + Sync {
    /// Publish a message to a destination
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>>;
}
```

**Router System**
```rust
pub struct Router {
    // Routes map resource IDs (topics) to producers
    // Automatically dispatches incoming messages to the right records
}
```

### Data Flow

#### Outbound (AimDB → External System)
```
Record Update → Tap Consumer → Serializer → Connector.publish() → External System
```

#### Inbound (External System → AimDB)
```
External Message → Connector Event Handler → Router.route() → Producer → Record
```

---

## Connector Lifecycle

### 1. User Configuration
```rust
let db = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(MyConnectorBuilder::new("myprotocol://broker:1234"))
    .configure::<Temperature>(|reg| {
        reg.link_to("myprotocol://sensor/temp")    // Outbound
           .with_serializer(serialize_temp);
    })
    .configure::<Command>(|reg| {
        reg.link_from("myprotocol://cmd/device")   // Inbound
           .with_deserializer(deserialize_cmd)
           .with_buffer(BufferCfg::SingleLatest);
    })
    .build().await?;
```

### 2. Build Phase
1. Database collects all `.link_from()` declarations
2. Calls `builder.build(db)` for each connector scheme
3. Builder collects inbound routes via `db.collect_inbound_routes(scheme)`
4. Builder constructs router from routes
5. Builder creates and returns connector implementation

### 3. Runtime Phase
- **Outbound**: Records publish via `.link_to()` automatically
- **Inbound**: Connector receives messages, routes to producers
- **Reconnection**: Connector handles connection failures
- **Subscriptions**: Connector subscribes to topics from router

---

## Step-by-Step Tutorial

Let's build a simple **HTTP webhook connector** that demonstrates all key concepts.

### Step 1: Create Crate Structure

```bash
mkdir aimdb-http-connector
cd aimdb-http-connector
```

**Cargo.toml:**
```toml
[package]
name = "aimdb-http-connector"
version = "0.1.0"
edition = "2021"

[features]
default = []
std = ["aimdb-core/std", "tokio", "reqwest"]
tokio-runtime = ["std", "tokio", "reqwest"]
embassy-runtime = ["aimdb-core/alloc", "embassy-net", "reqwest-embassy"]

[dependencies]
aimdb-core = { path = "../aimdb-core", default-features = false }
aimdb-executor = { path = "../aimdb-executor", default-features = false }

# Tokio dependencies (std)
tokio = { workspace = true, optional = true }
reqwest = { version = "0.11", optional = true }

# Embassy dependencies (no_std)
embassy-net = { workspace = true, optional = true }
reqwest-embassy = { version = "0.1", optional = true }
```

### Step 2: Define Builder and Implementation

**src/lib.rs:**
```rust
use aimdb_core::ConnectorBuilder;
use alloc::string::String;
use alloc::sync::Arc;

/// HTTP webhook connector builder
pub struct HttpConnectorBuilder {
    base_url: String,
}

impl HttpConnectorBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            base_url: url.into(),
        }
    }
}

/// Internal connector implementation
pub struct HttpConnectorImpl {
    base_url: String,
    router: Arc<Router>,
    // Runtime-specific HTTP client
}
```

### Step 3: Implement ConnectorBuilder

```rust
impl<R> ConnectorBuilder<R> for HttpConnectorBuilder
where
    R: aimdb_executor::Spawn + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = DbResult<Arc<dyn Connector>>> + Send + 'a>> {
        Box::pin(async move {
            // Step 1: Collect inbound routes
            let routes = db.collect_inbound_routes("http");

            // Step 2: Build router
            let router = RouterBuilder::from_routes(routes).build();

            // Step 3: Create connector implementation
            let connector = HttpConnectorImpl::new(
                &self.base_url,
                router,
            ).await?;

            Ok(Arc::new(connector) as Arc<dyn Connector>)
        })
    }

    fn scheme(&self) -> &str {
        "http"
    }
}
```

### Step 4: Implement Connector Trait

```rust
impl Connector for HttpConnectorImpl {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        let url = format!("{}{}", self.base_url, destination);
        let payload_owned = payload.to_vec();

        Box::pin(async move {
            // Send HTTP POST request
            let response = self.client
                .post(&url)
                .body(payload_owned)
                .send()
                .await
                .map_err(|_| PublishError::ConnectionFailed)?;

            if response.status().is_success() {
                Ok(())
            } else {
                Err(PublishError::ConnectionFailed)
            }
        })
    }
}
```

---

## Tokio Implementation

### Full Example: Kafka Connector (Tokio)

**Key Patterns:**
- Use `rdkafka` for Kafka client
- Spawn consumer task with `tokio::spawn`
- Use `tokio::sync::mpsc` for internal channels
- Use standard library types (`std::sync::Arc`, `std::string::String`)

```rust
use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::ConnectorBuilder;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::ClientConfig;
use std::sync::Arc;

pub struct KafkaConnectorBuilder {
    bootstrap_servers: String,
}

impl KafkaConnectorBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            bootstrap_servers: url.into(),
        }
    }
}

impl<R> ConnectorBuilder<R> for KafkaConnectorBuilder
where
    R: aimdb_executor::Spawn + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = DbResult<Arc<dyn Connector>>> + Send + 'a>> {
        Box::pin(async move {
            // Collect routes
            let routes = db.collect_inbound_routes("kafka");
            let router = RouterBuilder::from_routes(routes).build();

            // Create Kafka producer
            let producer: FutureProducer = ClientConfig::new()
                .set("bootstrap.servers", &self.bootstrap_servers)
                .create()
                .map_err(|e| DbError::RuntimeError {
                    message: format!("Kafka producer error: {}", e).into(),
                })?;

            // Create consumer for inbound messages
            let consumer: StreamConsumer = ClientConfig::new()
                .set("bootstrap.servers", &self.bootstrap_servers)
                .set("group.id", "aimdb-consumer")
                .create()
                .map_err(|e| DbError::RuntimeError {
                    message: format!("Kafka consumer error: {}", e).into(),
                })?;

            // Subscribe to topics from router
            let topics: Vec<&str> = router
                .resource_ids()
                .iter()
                .map(|s| s.as_ref())
                .collect();
            
            consumer
                .subscribe(&topics)
                .map_err(|e| DbError::RuntimeError {
                    message: format!("Kafka subscribe error: {}", e).into(),
                })?;

            // Spawn consumer task
            let router_clone = Arc::new(router);
            let router_for_task = router_clone.clone();
            
            tokio::spawn(async move {
                use rdkafka::message::Message;
                use futures_util::StreamExt;

                let mut stream = consumer.stream();
                while let Some(message) = stream.next().await {
                    match message {
                        Ok(msg) => {
                            if let (Some(topic), Some(payload)) = 
                                (msg.topic(), msg.payload())
                            {
                                // Route to appropriate producer
                                if let Err(e) = router_for_task
                                    .route(topic, payload)
                                    .await
                                {
                                    eprintln!("Routing error: {:?}", e);
                                }
                            }
                        }
                        Err(e) => eprintln!("Kafka error: {:?}", e),
                    }
                }
            });

            Ok(Arc::new(KafkaConnectorImpl {
                producer: Arc::new(producer),
                router: router_clone,
            }) as Arc<dyn Connector>)
        })
    }

    fn scheme(&self) -> &str {
        "kafka"
    }
}

pub struct KafkaConnectorImpl {
    producer: Arc<FutureProducer>,
    router: Arc<Router>,
}

impl Connector for KafkaConnectorImpl {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        let topic = destination.to_string();
        let payload_owned = payload.to_vec();
        let producer = self.producer.clone();

        Box::pin(async move {
            let record = FutureRecord::to(&topic)
                .payload(&payload_owned);

            producer
                .send(record, std::time::Duration::from_secs(5))
                .await
                .map_err(|(e, _)| {
                    eprintln!("Kafka publish error: {:?}", e);
                    PublishError::ConnectionFailed
                })?;

            Ok(())
        })
    }
}
```

### Tokio Patterns Summary

✅ **Use standard library types**
```rust
use std::sync::Arc;
use std::string::String;
use std::vec::Vec;
```

✅ **Spawn tasks with tokio::spawn**
```rust
tokio::spawn(async move {
    // Background task
});
```

✅ **Use async libraries**
```rust
tokio::time::sleep(Duration::from_secs(1)).await;
```

✅ **Error handling with thiserror**
```rust
use thiserror::Error;
```

---

## Embassy Implementation

### Full Example: KNX Connector (Embassy)

**Key Patterns:**
- Use `no_std` compatible libraries
- Use `StaticCell` for static allocation
- Use `embassy_sync::channel` for communication
- Wrap futures with `SendFutureWrapper` for `Send` trait
- Access network via `EmbassyNetwork` trait

```rust
extern crate alloc;

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::ConnectorBuilder;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Sender};
use static_cell::StaticCell;

const CHANNEL_SIZE: usize = 32;

/// KNX action (outbound)
#[derive(Clone)]
pub enum KnxAction {
    Write { address: String, value: Vec<u8> },
}

/// KNX event (inbound)
#[derive(Clone)]
pub enum KnxEvent {
    GroupValueReceived { address: String, value: Vec<u8> },
}

pub struct KnxConnectorBuilder {
    gateway_url: String,
}

impl KnxConnectorBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            gateway_url: url.into(),
        }
    }
}

impl<R> ConnectorBuilder<R> for KnxConnectorBuilder
where
    R: aimdb_executor::Spawn + aimdb_embassy_adapter::EmbassyNetwork + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = DbResult<Arc<dyn Connector>>> + Send + 'a>> {
        // Wrap in SendFutureWrapper for Embassy compatibility
        Box::pin(SendFutureWrapper(async move {
            // Collect routes
            let routes = db.collect_inbound_routes("knx");
            let router = RouterBuilder::from_routes(routes).build();

            // Parse gateway URL
            let connector_url = ConnectorUrl::parse(&self.gateway_url)
                .map_err(|_| DbError::RuntimeError { _message: () })?;

            // Create static channels
            static ACTION_CHANNEL: StaticCell<Channel<NoopRawMutex, KnxAction, CHANNEL_SIZE>> =
                StaticCell::new();
            static EVENT_CHANNEL: StaticCell<Channel<NoopRawMutex, KnxEvent, CHANNEL_SIZE>> =
                StaticCell::new();

            let action_channel = ACTION_CHANNEL.init(Channel::new());
            let event_channel = EVENT_CHANNEL.init(Channel::new());

            let action_sender = action_channel.sender();
            let action_receiver = action_channel.receiver();
            let event_sender = event_channel.sender();
            let event_receiver = event_channel.receiver();

            let router_arc = Arc::new(router);
            let router_for_task = router_arc.clone();

            // Get network stack from runtime
            let network = db.runtime().network_stack();

            // Spawn KNX protocol task
            let gateway_addr = connector_url.host.clone();
            let _ = db.runtime().spawn(SendFutureWrapper(async move {
                knx_protocol_task(
                    network,
                    gateway_addr,
                    action_receiver,
                    event_sender,
                ).await
            }));

            // Spawn event router task
            let _ = db.runtime().spawn(SendFutureWrapper(async move {
                loop {
                    if let Ok(event) = event_receiver.receive().await {
                        match event {
                            KnxEvent::GroupValueReceived { address, value } => {
                                if let Err(_e) = router_for_task
                                    .route(&address, &value)
                                    .await
                                {
                                    #[cfg(feature = "defmt")]
                                    defmt::error!("KNX routing error");
                                }
                            }
                        }
                    }
                }
            }));

            Ok(Arc::new(KnxConnectorImpl {
                router: router_arc,
                action_sender,
            }) as Arc<dyn Connector>)
        }))
    }

    fn scheme(&self) -> &str {
        "knx"
    }
}

pub struct KnxConnectorImpl {
    router: Arc<Router>,
    action_sender: Sender<'static, NoopRawMutex, KnxAction, CHANNEL_SIZE>,
}

// SAFETY: Embassy is single-threaded
unsafe impl Send for KnxConnectorImpl {}
unsafe impl Sync for KnxConnectorImpl {}

impl Connector for KnxConnectorImpl {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        let address = destination.to_string();
        let value = payload.to_vec();
        let action_sender = self.action_sender.clone();

        Box::pin(SendFutureWrapper(async move {
            let action = KnxAction::Write { address, value };
            action_sender.send(action).await;
            Ok(())
        }))
    }
}

/// KNX protocol task (runs in background)
async fn knx_protocol_task(
    network: &'static Stack<'static>,
    gateway_addr: String,
    action_receiver: Receiver<'static, NoopRawMutex, KnxAction, CHANNEL_SIZE>,
    event_sender: Sender<'static, NoopRawMutex, KnxEvent, CHANNEL_SIZE>,
) {
    // Implementation of KNX/IP protocol
    // - Connect to KNX gateway
    // - Send write commands from action_receiver
    // - Receive group telegrams and send to event_sender
}

// SendFutureWrapper for Embassy compatibility
struct SendFutureWrapper<F>(F);
unsafe impl<F> Send for SendFutureWrapper<F> {}

impl<F: Future> Future for SendFutureWrapper<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> core::task::Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}
```

### Embassy Patterns Summary

✅ **Use alloc types**
```rust
extern crate alloc;
use alloc::{string::String, vec::Vec, sync::Arc};
```

✅ **Use StaticCell for static allocation**
```rust
static CHANNEL: StaticCell<Channel<_, _, SIZE>> = StaticCell::new();
let channel = CHANNEL.init(Channel::new());
```

✅ **Wrap futures with SendFutureWrapper**
```rust
Box::pin(SendFutureWrapper(async move { /* ... */ }))
```

✅ **Access network via EmbassyNetwork trait**
```rust
where R: aimdb_executor::Spawn + aimdb_embassy_adapter::EmbassyNetwork
```

✅ **Use defmt for logging**
```rust
#[cfg(feature = "defmt")]
defmt::info!("Message: {}", value);
```

✅ **Implement unsafe Send + Sync**
```rust
unsafe impl Send for MyConnector {}
unsafe impl Sync for MyConnector {}
```

---

## Testing and Validation

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connector_build() {
        let builder = MyConnectorBuilder::new("myprotocol://localhost:1234");
        assert_eq!(builder.scheme(), "myprotocol");
    }

    #[tokio::test]
    async fn test_publish() {
        let connector = /* create connector */;
        let config = ConnectorConfig::default();
        
        let result = connector.publish("/test", &config, b"hello").await;
        assert!(result.is_ok());
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_bidirectional_flow() {
    // Create database with connector
    let db = AimDbBuilder::new()
        .runtime(TokioAdapter::new()?)
        .with_connector(MyConnectorBuilder::new("myprotocol://localhost"))
        .configure::<Temperature>(|reg| {
            reg.link_to("myprotocol://sensor/temp")
               .with_serializer(|t| serde_json::to_vec(t).unwrap());
        })
        .configure::<Command>(|reg| {
            reg.link_from("myprotocol://cmd")
               .with_deserializer(|b| serde_json::from_slice(b).unwrap())
               .with_buffer(BufferCfg::SingleLatest);
        })
        .build().await?;

    // Test outbound
    db.set(Temperature { value: 23.5 }).await?;
    
    // Test inbound
    let cmd_consumer = db.consumer::<Command>().unwrap();
    let cmd = cmd_consumer.recv().await?;
    assert_eq!(cmd.action, "read");
}
```

### Cross-Compilation Validation

Add to Makefile:
```makefile
test-myconnector:
	@echo "Testing MyConnector cross-compilation..."
	cargo check --package aimdb-myconnector-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime"
```

---

## Best Practices

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

### 2. **Handle Reconnection**

```rust
loop {
    match connect().await {
        Ok(connection) => {
            if let Err(e) = handle_connection(connection).await {
                eprintln!("Connection lost: {:?}", e);
            }
        }
        Err(e) => {
            eprintln!("Connection failed: {:?}", e);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}
```

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

### 6. **Logging Strategy**

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

---

## Real-World Examples

### 1. DDS Connector (Data Distribution Service)

```rust
pub struct DdsConnectorBuilder {
    domain_id: u32,
    participant_qos: ParticipantQos,
}

impl DdsConnectorBuilder {
    pub fn new(url: &str) -> Self {
        // Parse "dds://domain/42"
        let domain_id = parse_domain_from_url(url);
        Self {
            domain_id,
            participant_qos: Default::default(),
        }
    }
}

// Key features:
// - Topic-based pub/sub
// - QoS policies (reliability, durability, history)
// - Type discovery
// - Multiple transports (UDP, shared memory)
```

### 2. Modbus Connector (Industrial Automation)

```rust
pub struct ModbusConnectorBuilder {
    slave_address: u8,
    transport: ModbusTransport,
}

// Key features:
// - Register mapping (coils, discrete inputs, holding registers)
// - RTU and TCP variants
// - Polling vs event-driven
// - Error recovery
```

### 3. WebSocket Connector

```rust
pub struct WebSocketConnectorBuilder {
    url: String,
    reconnect_policy: ReconnectPolicy,
}

// Key features:
// - Bidirectional streams
// - JSON or binary frames
// - Automatic reconnection
// - Heartbeat/ping-pong
```

### 4. gRPC Connector

```rust
pub struct GrpcConnectorBuilder {
    endpoint: String,
    service_descriptor: ServiceDescriptor,
}

// Key features:
// - Streaming RPC support
// - Protocol buffers
// - TLS/authentication
// - Load balancing
```

---

## Checklist for New Connectors

- [ ] Create crate with `tokio-runtime` and `embassy-runtime` features
- [ ] Implement `ConnectorBuilder<R>` trait
- [ ] Implement `Connector` trait
- [ ] Parse URL and extract connection parameters
- [ ] Collect inbound routes via `db.collect_inbound_routes(scheme)`
- [ ] Build `Router` from collected routes
- [ ] Spawn background task for connection management
- [ ] Spawn event router task for inbound messages
- [ ] Subscribe to all topics from router
- [ ] Implement reconnection logic
- [ ] Add comprehensive error handling
- [ ] Write unit tests
- [ ] Write integration tests
- [ ] Test cross-compilation for embedded targets
- [ ] Add logging (tracing for Tokio, defmt for Embassy)
- [ ] Document usage examples
- [ ] Add to CI/CD pipeline

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
