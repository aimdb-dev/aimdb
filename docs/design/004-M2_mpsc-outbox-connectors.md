# Design Document: MPSC Outbox for Connector Sinks

This design introduces **MPSC (Multi-Producer Single-Consumer) outbox channels** as a new primitive in AimDB for interfacing with external protocol connectors (MQTT, Kafka, DDS). Unlike the existing SPMC record buffers used for internal consumer dispatch, MPSC outboxes enable multiple producers (any record consumer or user code) to enqueue messages to a single external system worker, preventing connection duplication and enabling efficient resource pooling.

**Key Benefits:**
- **Multi-Producer Pattern**: Any part of the application can enqueue to connectors
- **Single Worker per Outbox**: One connection/session per external system
- **Type-Safe**: Payload types prevent mixing messages across different sinks
- **Decoupled from Records**: Records keep their SPMC buffers, outboxes are separate
- **Runtime Agnostic**: Works with Tokio (std) and Embassy (embedded)
- **Backpressure Ready**: Configurable overflow behavior for flow control

---

## Table of Contents

1. [Motivation](#motivation)
2. [Goals & Non-Goals](#goals--non-goals)
3. [Architecture Overview](#architecture-overview)
4. [Detailed Design](#detailed-design)
5. [API Specification](#api-specification)
6. [Runtime Integration](#runtime-integration)
7. [Error Handling & Backpressure](#error-handling--backpressure)
8. [Connector Implementations](#connector-implementations)
9. [Memory & Performance](#memory--performance)
10. [Testing Strategy](#testing-strategy)
11. [Migration Path](#migration-path)
12. [Future Work](#future-work)

---

## Motivation

### Problem Statement

AimDB's current architecture provides excellent patterns for **internal data flow** via SPMC buffers and the Emitter pattern, but lacks primitives for **outbound communication** to external systems like message brokers, time-series databases, or IoT platforms.

**Challenges with Current Architecture:**
1. **No Multi-Producer Pattern**: Records have single producers, but sinks need multiple sources
2. **Connection Management**: Creating multiple connections to the same broker is wasteful
3. **Worker Lifecycle**: No clear pattern for managing long-lived external client tasks
4. **Type Safety**: Need to prevent different message types from mixing in the same queue

**Example Problem:**
```rust
// Multiple consumers want to publish to MQTT, but each would need its own connection
impl RecordT for SensorData {
    fn register(reg: &mut RecordRegistrar<Self>, cfg: &Self::Config) {
        reg.consumer(|em, data| async move {
            // ❌ How do we share ONE MQTT client across multiple consumers?
            // ❌ Creating a client per consumer is wasteful
            mqtt_client.publish("sensor/temp", &data.temp).await?;
        });
    }
}
```

### Use Cases

| Use Case | Pattern | Why MPSC? |
|----------|---------|-----------|
| **MQTT Sink** | All sensors → one broker | Single TCP connection, multiple publish sources |
| **Kafka Producer** | Events from multiple records → topic | Batch efficiency, connection pooling |
| **DDS Publisher** | Real-time data → DDS domain | QoS enforcement, single publisher per topic |
| **Metrics Export** | Stats from everywhere → Prometheus | Aggregation point, rate limiting |
| **Logging Sink** | Logs from all records → Loki | Single HTTP client, batch uploads |

### Target Environments

- **MCU (Embassy)**: Constrained resources, single connection per protocol, static channels
- **Edge (Tokio)**: Moderate traffic, efficient connection pooling
- **Cloud (Tokio)**: High throughput, multiple workers per payload type

---

## Goals & Non-Goals

### Goals

✅ **G1**: MPSC channel abstraction for outbound messages to external systems  
✅ **G2**: Type-safe payload routing (one MPSC per message payload type)  
✅ **G3**: Single worker per outbox (one consumer task manages external connection)  
✅ **G4**: Multi-producer pattern (any record/consumer can enqueue)  
✅ **G5**: `SinkWorker` trait for pluggable connector implementations  
✅ **G6**: Configurable overflow behavior (block, drop, error)  
✅ **G7**: Runtime-agnostic design (Tokio `mpsc` + Embassy `Channel`)  
✅ **G8**: Worker health monitoring and restart capability  
✅ **G9**: Integrate seamlessly with existing `Emitter` API  
✅ **G10**: Support for connection pooling (multiple workers per payload type)  

### Non-Goals

❌ **NG1**: Inbound connectors (source/subscriber) - separate design  
❌ **NG2**: Guaranteed delivery or persistence - connectors handle retries  
❌ **NG3**: Dynamic outbox reconfiguration after initialization  
❌ **NG4**: Transaction semantics across multiple outboxes  
❌ **NG5**: Automatic failover or high availability - connector responsibility  
❌ **NG6**: Message transformation or routing logic - use records for that  

---

## Architecture Overview

### Existing SPMC Record Buffers (Internal)

```
┌──────────────┐
│   Producer   │
│   produce()  │
└──────┬───────┘
       │
       ▼
┌─────────────────┐
│  SPMC Buffer    │◄── broadcast/watch channel
│  (per record)   │
└────┬───┬───┬────┘
     │   │   │
     ▼   ▼   ▼
  ┌──┐ ┌──┐ ┌──┐
  │C1│ │C2│ │C3│  (consumers: parallel async tasks)
  └──┘ └──┘ └──┘

Use Case: Internal multi-consumer dispatch
Pattern: Single producer → Multiple consumers
```

### New MPSC Outbox (External Systems)

```
┌────────┐  ┌────────┐  ┌────────┐
│Record1 │  │Record2 │  │User    │
│Consumer│  │Consumer│  │Code    │
└───┬────┘  └───┬────┘  └───┬────┘
    │           │           │
    │ enqueue() │ enqueue() │ enqueue()
    │           │           │
    └───────┬───┴───────┬───┘
            │           │
            ▼           ▼
    ┌───────────────────────┐
    │   MPSC Outbox<T>      │◄── tokio::mpsc / embassy::Channel
    │   (per payload type)  │
    └───────────┬───────────┘
                │
                ▼
         ┌──────────────┐
         │ SinkWorker   │
         │ (single task)│
         └──────┬───────┘
                │
                ▼
         ┌──────────────┐
         │ External     │
         │ System       │
         │ (MQTT/Kafka) │
         └──────────────┘

Use Case: External protocol connectors
Pattern: Multiple producers → Single consumer
```

### Coexistence: Both Patterns in AimDB

```
┌─────────────────────────────────────────────────────────────┐
│                         AimDb                               │
│                                                             │
│  ┌────────────────────────┐   ┌─────────────────────────┐  │
│  │   TypedRecord<T>       │   │   Outbox Registry       │  │
│  │                        │   │                         │  │
│  │  ┌──────────────┐      │   │  HashMap<TypeId,        │  │
│  │  │ SPMC Buffer  │      │   │    Box<dyn AnySender>>  │  │
│  │  │ (internal)   │      │   │                         │  │
│  │  └──────────────┘      │   │  ┌──────────────────┐   │  │
│  │                        │   │  │ MPSC<MqttMsg>    │   │  │
│  │  Vec<Consumer>         │   │  │ → MqttWorker     │   │  │
│  │                        │   │  └──────────────────┘   │  │
│  └────────────────────────┘   │                         │  │
│                               │  ┌──────────────────┐   │  │
│                               │  │ MPSC<KafkaMsg>   │   │  │
│                               │  │ → KafkaWorker    │   │  │
│                               │  └──────────────────┘   │  │
│                               └─────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘

Key: Records remain unchanged, outboxes are a separate subsystem
```

---

## Detailed Design

### Core Components

#### 1. Outbox Registry

The `AimDbInner` struct gains a new field for storing outbox senders:

```rust
pub struct AimDbInner {
    /// Existing: Map from TypeId to type-erased records (SPMC buffers)
    #[cfg(feature = "std")]
    pub(crate) records: HashMap<TypeId, Box<dyn AnyRecord>>,
    
    #[cfg(not(feature = "std"))]
    pub(crate) records: BTreeMap<TypeId, Box<dyn AnyRecord>>,
    
    /// NEW: Map from payload TypeId to MPSC sender (for outboxes)
    #[cfg(feature = "std")]
    pub(crate) outboxes: Arc<Mutex<HashMap<TypeId, Box<dyn AnySender>>>>,
    
    #[cfg(not(feature = "std"))]
    pub(crate) outboxes: Arc<Mutex<BTreeMap<TypeId, Box<dyn AnySender>>>>,
}
```

**Design Rationale:**
- Separate from `records` - outboxes are not records (no producer/consumer functions)
- Keyed by payload `TypeId` - type safety at runtime
- `Arc<Mutex<_>>` for interior mutability - initialization happens after `AimDb` is built
- `Box<dyn AnySender>` for type erasure - store heterogeneous channel types

#### 2. Type-Erased Sender Trait

To store different `mpsc::Sender<T>` types in the same map:

```rust
/// Type-erased sender for MPSC outboxes
trait AnySender: Send + Sync {
    /// Downcast to concrete sender type
    fn as_any(&self) -> &dyn core::any::Any;
}

/// Concrete wrapper for typed senders
struct StoredSender<T: Send + 'static> {
    #[cfg(feature = "std")]
    sender: tokio::sync::mpsc::Sender<T>,
    
    #[cfg(not(feature = "std"))]
    sender: embassy_sync::channel::Sender<'static, T>,
}

impl<T: Send + 'static> AnySender for StoredSender<T> {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
```

**Design Rationale:**
- `AnySender` is object-safe (no associated types, no `Self`)
- `StoredSender<T>` holds runtime-specific sender type
- `as_any()` allows safe downcasting with type checking
- Feature flags handle Tokio vs Embassy channel types

#### 3. SinkWorker Trait

Defines the interface for connector implementations:

```rust
/// Worker that consumes messages from an MPSC outbox
///
/// Implementers spawn a task that drains the receiver and
/// communicates with an external system (MQTT, Kafka, etc.).
///
/// # Type Parameters
/// * `T` - The payload type this worker consumes
///
/// # Lifecycle
/// 1. `spawn()` is called once during `init_outbox()`
/// 2. Worker task runs until receiver is closed
/// 3. Worker is responsible for connection management and retries
pub trait SinkWorker<T>: Send + 'static {
    /// Spawn the worker task
    ///
    /// # Arguments
    /// * `rt` - Runtime adapter for spawning tasks
    /// * `rx` - Receiver end of the MPSC channel
    ///
    /// # Returns
    /// A handle for monitoring/controlling the worker
    fn spawn(
        self,
        rt: Arc<dyn RuntimeAdapter>,
        rx: OutboxReceiver<T>,
    ) -> WorkerHandle;
}

/// Handle for monitoring and controlling sink workers
pub struct WorkerHandle {
    task_id: usize,
    is_running: Arc<AtomicBool>,
    restart_fn: Option<Box<dyn Fn() + Send + Sync>>,
}

impl WorkerHandle {
    /// Check if worker is still running
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }
    
    /// Restart the worker (if restart function was provided)
    pub fn restart(&self) -> Result<(), DbError> {
        if let Some(restart) = &self.restart_fn {
            restart();
            Ok(())
        } else {
            Err(DbError::InvalidOperation(
                "Worker does not support restart".into()
            ))
        }
    }
}
```

**Design Rationale:**
- `spawn()` takes ownership - worker moves into task
- Returns `WorkerHandle` for monitoring - enables health checks
- Runtime adapter provided - worker can spawn subtasks
- Single method - simple to implement

#### 4. OutboxConfig

Configuration for outbox behavior:

```rust
/// Configuration for an MPSC outbox
#[derive(Clone, Debug)]
pub struct OutboxConfig {
    /// Channel capacity (buffer size)
    pub capacity: usize,
    
    /// Behavior when channel is full
    pub overflow: OverflowBehavior,
    
    /// Enable worker restart on panic
    pub auto_restart: bool,
    
    /// Maximum concurrent enqueue operations (0 = unlimited)
    pub max_concurrent_enqueue: usize,
}

/// Behavior when outbox channel is full
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowBehavior {
    /// Block until space is available (applies backpressure)
    Block,
    
    /// Return error immediately (fail fast)
    Error,
    
    /// Drop oldest message (ring buffer behavior)
    DropOldest,
    
    /// Drop newest message (reject incoming)
    DropNewest,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            capacity: 1024,
            overflow: OverflowBehavior::Block,
            auto_restart: false,
            max_concurrent_enqueue: 0,
        }
    }
}
```

**Design Rationale:**
- `capacity` - tunable buffer size for different environments
- `overflow` - explicit choice vs implicit blocking
- `auto_restart` - resilience for long-running workers
- `max_concurrent_enqueue` - rate limiting if needed

---

## API Specification

### Initialization API

```rust
impl AimDb {
    /// Initialize an MPSC outbox for payload type T
    ///
    /// This creates a channel, stores the sender in the outbox registry,
    /// and spawns the worker task to consume messages.
    ///
    /// # Type Parameters
    /// * `T` - The payload type (must be unique per outbox)
    /// * `W` - The worker implementation
    ///
    /// # Arguments
    /// * `config` - Outbox configuration
    /// * `worker` - Worker implementation to spawn
    ///
    /// # Returns
    /// A handle for monitoring the worker
    ///
    /// # Errors
    /// * `DbError::AlreadyExists` - Outbox for this type already exists
    ///
    /// # Example
    /// ```rust,ignore
    /// let handle = db.init_outbox::<MqttMsg, _>(
    ///     OutboxConfig::default(),
    ///     MqttWorker::new("mqtt://broker", "client-id"),
    /// )?;
    /// ```
    pub fn init_outbox<T, W>(
        &self,
        config: OutboxConfig,
        worker: W,
    ) -> Result<WorkerHandle, DbError>
    where
        T: Send + 'static,
        W: SinkWorker<T>,
    {
        let type_id = TypeId::of::<T>();
        
        // Check if outbox already exists
        {
            let outboxes = self.inner.outboxes.lock().unwrap();
            if outboxes.contains_key(&type_id) {
                return Err(DbError::AlreadyExists(
                    "Outbox already initialized for this type".into()
                ));
            }
        }
        
        // Create channel
        let (tx, rx) = self.runtime.create_mpsc_channel::<T>(config.capacity);
        
        // Store sender
        {
            let mut outboxes = self.inner.outboxes.lock().unwrap();
            outboxes.insert(
                type_id,
                Box::new(StoredSender { sender: tx })
            );
        }
        
        // Spawn worker
        let handle = worker.spawn(self.runtime.clone(), rx);
        
        Ok(handle)
    }
    
    /// Initialize multiple outboxes at once
    ///
    /// Convenience method for bulk initialization.
    ///
    /// # Example
    /// ```rust,ignore
    /// db.init_outboxes(&[
    ///     (OutboxConfig::default(), MqttWorker::new(...)),
    ///     (OutboxConfig::default(), KafkaWorker::new(...)),
    /// ])?;
    /// ```
    pub fn init_outboxes(
        &self,
        configs: &[(OutboxConfig, Box<dyn AnySinkWorker>)],
    ) -> Result<Vec<WorkerHandle>, DbError> {
        // Implementation
    }
}
```

### Enqueue API (Producer Side)

```rust
impl Emitter {
    /// Enqueue a message to an outbox
    ///
    /// This is the primary way to send messages to external systems.
    /// Multiple producers can call this concurrently.
    ///
    /// # Type Parameters
    /// * `T` - The payload type (must match initialized outbox)
    ///
    /// # Arguments
    /// * `item` - The message to enqueue
    ///
    /// # Returns
    /// * `Ok(())` - Message enqueued successfully
    /// * `Err(DbError)` - Outbox not initialized or channel error
    ///
    /// # Errors
    /// * `DbError::NotFound` - No outbox for this type
    /// * `DbError::ChannelClosed` - Worker has stopped
    /// * `DbError::ChannelFull` - Channel full (if overflow=Error)
    ///
    /// # Example
    /// ```rust,ignore
    /// async fn process_sensor(em: &Emitter, data: SensorData) {
    ///     // Transform data
    ///     let mqtt_msg = MqttMsg {
    ///         topic: format!("sensors/{}", data.id),
    ///         payload: serde_json::to_vec(&data)?,
    ///     };
    ///     
    ///     // Enqueue to MQTT outbox
    ///     em.enqueue(mqtt_msg).await?;
    /// }
    /// ```
    pub async fn enqueue<T: Send + 'static>(
        &self,
        item: T,
    ) -> DbResult<()> {
        let type_id = TypeId::of::<T>();
        
        // Get sender from registry
        let sender = {
            let outboxes = self.inner.outboxes.lock().unwrap();
            let tx_any = outboxes
                .get(&type_id)
                .ok_or_else(|| DbError::NotFound(
                    "Outbox not initialized for this type".into()
                ))?;
            
            // Downcast to concrete type
            tx_any
                .as_any()
                .downcast_ref::<StoredSender<T>>()
                .ok_or_else(|| DbError::TypeMismatch(
                    "Outbox type mismatch".into()
                ))?
                .sender
                .clone()
        };
        
        // Send (respects OutboxConfig.overflow behavior)
        sender
            .send(item)
            .await
            .map_err(|_| DbError::ChannelClosed(
                "Outbox receiver closed".into()
            ))
    }
    
    /// Try to enqueue without blocking
    ///
    /// Useful for non-critical messages that should be dropped if buffer is full.
    pub fn try_enqueue<T: Send + 'static>(
        &self,
        item: T,
    ) -> DbResult<()> {
        // Similar implementation but uses try_send()
    }
}

impl AimDb {
    /// Convenience method: enqueue via database (delegates to emitter)
    pub async fn enqueue<T: Send + 'static>(
        &self,
        item: T,
    ) -> DbResult<()> {
        self.emitter().enqueue(item).await
    }
}
```

### Worker Implementation Example

```rust
/// Example: MQTT sink worker
pub struct MqttWorker {
    broker_url: String,
    client_id: String,
    qos: QoS,
}

impl MqttWorker {
    pub fn new(broker_url: String, client_id: String) -> Self {
        Self {
            broker_url,
            client_id,
            qos: QoS::AtLeastOnce,
        }
    }
}

impl SinkWorker<MqttMsg> for MqttWorker {
    fn spawn(
        self,
        rt: Arc<dyn RuntimeAdapter>,
        mut rx: OutboxReceiver<MqttMsg>,
    ) -> WorkerHandle {
        let is_running = Arc::new(AtomicBool::new(true));
        let is_running_clone = is_running.clone();
        
        rt.spawn(Box::pin(async move {
            // Connect to MQTT broker
            let mut client = match MqttClient::connect(
                &self.broker_url,
                &self.client_id,
            ).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("MQTT connection failed: {}", e);
                    is_running_clone.store(false, Ordering::Relaxed);
                    return;
                }
            };
            
            // Drain receiver
            while let Some(msg) = rx.recv().await {
                if let Err(e) = client.publish(
                    &msg.topic,
                    &msg.payload,
                    self.qos,
                ).await {
                    eprintln!("MQTT publish failed: {}", e);
                    // Retry logic here
                }
            }
            
            is_running_clone.store(false, Ordering::Relaxed);
        }));
        
        WorkerHandle {
            task_id: 0, // Generate unique ID
            is_running,
            restart_fn: None, // Could add restart logic
        }
    }
}
```

---

## Runtime Integration

### Tokio Adapter (std)

```rust
// In aimdb-tokio-adapter/src/outbox.rs

use tokio::sync::mpsc;

pub type OutboxSender<T> = mpsc::Sender<T>;
pub type OutboxReceiver<T> = mpsc::Receiver<T>;

impl RuntimeAdapter for TokioAdapter {
    fn create_mpsc_channel<T: Send + 'static>(
        &self,
        capacity: usize,
    ) -> (OutboxSender<T>, OutboxReceiver<T>) {
        mpsc::channel(capacity)
    }
}
```

### Embassy Adapter (no_std)

```rust
// In aimdb-embassy-adapter/src/outbox.rs

use embassy_sync::channel::Channel;

// Static channel for embedded (capacity must be const)
pub struct OutboxChannel<T, const N: usize> {
    inner: Channel<CriticalSectionRawMutex, T, N>,
}

pub type OutboxSender<T> = embassy_sync::channel::Sender<'static, T>;
pub type OutboxReceiver<T> = embassy_sync::channel::Receiver<'static, T>;

impl RuntimeAdapter for EmbassyAdapter {
    fn create_mpsc_channel<T: Send + 'static>(
        &self,
        capacity: usize,
    ) -> (OutboxSender<T>, OutboxReceiver<T>) {
        // Embassy channels require static lifetime and const capacity
        // Implementation requires static allocation or heap (if alloc feature)
        
        // Option 1: Static pool of channels (const generics)
        // Option 2: Heap allocation with alloc feature
        // See detailed implementation in appendix
    }
}
```

**Embassy Challenge:** Channels need `'static` lifetime and `const` capacity.

**Solutions:**
1. **Static Pool**: Pre-allocate channels in `static` storage
2. **Heap Allocation**: Use `alloc` feature + `Box::leak`
3. **Builder API**: User provides channels during initialization

---

## Error Handling & Backpressure

### Error Types

```rust
/// Outbox-specific errors
#[derive(Debug)]
pub enum OutboxError {
    /// Outbox not initialized for this type
    NotInitialized(String),
    
    /// Channel receiver has been dropped
    ReceiverClosed,
    
    /// Channel is full (when overflow=Error)
    ChannelFull,
    
    /// Type mismatch during downcast
    TypeMismatch,
    
    /// Worker panic or crash
    WorkerDied,
}
```

### Backpressure Strategies

| Overflow Behavior | Use Case | Trade-offs |
|-------------------|----------|------------|
| **Block** | Critical messages (commands) | Applies backpressure, may slow producers |
| **Error** | Best-effort with fallback | Fast failure, requires error handling |
| **DropOldest** | High-frequency telemetry | Ring buffer, may lose history |
| **DropNewest** | Latest-value semantics | Protects old data, may reject fresh data |

**Example: Configurable Overflow**

```rust
// High-priority commands: block until sent
db.init_outbox::<CommandMsg, _>(
    OutboxConfig {
        capacity: 100,
        overflow: OverflowBehavior::Block,
        ..Default::default()
    },
    CommandWorker::new(),
)?;

// Best-effort telemetry: drop oldest if full
db.init_outbox::<TelemetryMsg, _>(
    OutboxConfig {
        capacity: 10000,
        overflow: OverflowBehavior::DropOldest,
        ..Default::default()
    },
    TelemetryWorker::new(),
)?;
```

---

## Connector Implementations

### MQTT Connector Architecture

```rust
// In aimdb-mqtt-connector crate

/// MQTT message payload
#[derive(Clone, Debug)]
pub struct MqttMsg {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
}

/// MQTT sink worker configuration
#[derive(Clone)]
pub struct MqttSinkConfig {
    pub broker_url: String,
    pub client_id: String,
    pub keep_alive: Duration,
    pub clean_session: bool,
    pub credentials: Option<(String, String)>,
}

/// MQTT sink worker
pub struct MqttSinkWorker {
    config: MqttSinkConfig,
}

impl SinkWorker<MqttMsg> for MqttSinkWorker {
    fn spawn(
        self,
        rt: Arc<dyn RuntimeAdapter>,
        mut rx: OutboxReceiver<MqttMsg>,
    ) -> WorkerHandle {
        // Implementation with rumqttc
    }
}

/// Builder for MQTT sink
pub struct MqttSinkBuilder {
    config: MqttSinkConfig,
    outbox_config: OutboxConfig,
}

impl MqttSinkBuilder {
    pub fn new(broker_url: impl Into<String>) -> Self {
        Self {
            config: MqttSinkConfig {
                broker_url: broker_url.into(),
                client_id: format!("aimdb-{}", uuid::Uuid::new_v4()),
                keep_alive: Duration::from_secs(60),
                clean_session: true,
                credentials: None,
            },
            outbox_config: OutboxConfig::default(),
        }
    }
    
    pub fn client_id(mut self, id: impl Into<String>) -> Self {
        self.config.client_id = id.into();
        self
    }
    
    pub fn credentials(mut self, user: String, pass: String) -> Self {
        self.config.credentials = Some((user, pass));
        self
    }
    
    pub fn buffer_size(mut self, size: usize) -> Self {
        self.outbox_config.capacity = size;
        self
    }
    
    pub fn build(self) -> (OutboxConfig, MqttSinkWorker) {
        (
            self.outbox_config,
            MqttSinkWorker { config: self.config },
        )
    }
}

// Usage
let (config, worker) = MqttSinkBuilder::new("mqtt://broker:1883")
    .client_id("my-app")
    .credentials("user".into(), "pass".into())
    .buffer_size(2048)
    .build();

db.init_outbox::<MqttMsg, _>(config, worker)?;
```

### Kafka Connector Architecture

```rust
// In aimdb-kafka-connector crate

/// Kafka message payload
#[derive(Clone, Debug)]
pub struct KafkaMsg {
    pub topic: String,
    pub key: Option<Vec<u8>>,
    pub payload: Vec<u8>,
    pub headers: HashMap<String, Vec<u8>>,
}

/// Kafka sink worker with batching
pub struct KafkaSinkWorker {
    brokers: Vec<String>,
    batch_size: usize,
    batch_timeout: Duration,
}

impl SinkWorker<KafkaMsg> for KafkaSinkWorker {
    fn spawn(
        self,
        rt: Arc<dyn RuntimeAdapter>,
        mut rx: OutboxReceiver<KafkaMsg>,
    ) -> WorkerHandle {
        // Implementation with rdkafka
        // Includes batching logic for efficiency
    }
}
```

### DDS Connector Architecture

```rust
// In aimdb-dds-connector crate

/// DDS message (typed via IDL)
#[derive(Clone, Debug)]
pub struct DdsMsg<T: Serialize> {
    pub data: T,
    pub qos: DdsQoS,
}

/// DDS sink worker (generic over message type)
pub struct DdsSinkWorker<T: Serialize + Send + 'static> {
    domain_id: u32,
    topic_name: String,
    _phantom: PhantomData<T>,
}

impl<T: Serialize + Send + 'static> SinkWorker<DdsMsg<T>> for DdsSinkWorker<T> {
    fn spawn(
        self,
        rt: Arc<dyn RuntimeAdapter>,
        mut rx: OutboxReceiver<DdsMsg<T>>,
    ) -> WorkerHandle {
        // Implementation with cyclonedds-rs or rust_dds
    }
}
```

---

## Memory & Performance

### Memory Footprint

**Per Outbox:**
- Channel buffer: `capacity * sizeof(T)` bytes
- Sender storage: ~48 bytes (Arc + vtable + wrapper)
- Worker task stack: ~4-8 KB (runtime dependent)
- Worker state: varies by connector (e.g., MQTT client ~10 KB)

**Total Overhead:**
```
outbox_memory = num_outboxes * (
    capacity * sizeof(payload) +
    sender_overhead +
    task_stack +
    worker_state
)
```

**Example: 3 outboxes with 1024 capacity**
- MQTT (128-byte msgs): 1024 * 128 = 128 KB
- Kafka (256-byte msgs): 1024 * 256 = 256 KB
- Metrics (64-byte msgs): 1024 * 64 = 64 KB
- Total: ~450 KB + worker overhead (~30 KB) = **~480 KB**

### Performance Characteristics

**Enqueue Latency:**
- Tokio `mpsc`: ~100-500ns (uncontended)
- Embassy `Channel`: ~200-800ns (depends on mutex impl)
- Type lookup: ~50ns (`HashMap` + `TypeId`)
- Downcast: ~20ns (pointer cast + type check)
- **Total: <1µs** typical case

**Throughput:**
- Tokio: >1M msgs/sec single producer, >500K msgs/sec multi-producer
- Embassy: ~50K-200K msgs/sec (depends on target, clock speed)

**Comparison to Direct Calls:**
- Direct function call: ~10-50ns
- MPSC enqueue: ~500ns
- **Overhead: ~10-50x**, but enables multi-producer + async dispatch

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_outbox_init_and_enqueue() {
        let db = setup_test_db().await;
        
        // Initialize outbox
        let handle = db.init_outbox::<TestMsg, _>(
            OutboxConfig::default(),
            TestWorker::new(),
        ).unwrap();
        
        assert!(handle.is_running());
        
        // Enqueue message
        let msg = TestMsg { data: "hello".into() };
        db.enqueue(msg.clone()).await.unwrap();
        
        // Verify worker received it
        // (use channel or shared state)
    }
    
    #[tokio::test]
    async fn test_multi_producer() {
        let db = Arc::new(setup_test_db().await);
        let counter = Arc::new(AtomicUsize::new(0));
        
        db.init_outbox::<TestMsg, _>(
            OutboxConfig::default(),
            CountingWorker::new(counter.clone()),
        ).unwrap();
        
        // Spawn 10 producers
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let db = db.clone();
                tokio::spawn(async move {
                    for j in 0..100 {
                        db.enqueue(TestMsg {
                            data: format!("p{}-m{}", i, j),
                        }).await.unwrap();
                    }
                })
            })
            .collect();
        
        for h in handles {
            h.await.unwrap();
        }
        
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // Verify all 1000 messages received
        assert_eq!(counter.load(Ordering::Relaxed), 1000);
    }
    
    #[tokio::test]
    async fn test_overflow_behavior() {
        // Test each OverflowBehavior variant
    }
    
    #[tokio::test]
    async fn test_worker_crash_detection() {
        // Verify handle.is_running() returns false after worker panic
    }
}
```

### Integration Tests

```rust
// tests/mqtt_connector.rs

#[tokio::test]
async fn test_mqtt_roundtrip() {
    // Start test MQTT broker
    let broker = start_test_broker().await;
    
    // Initialize AimDB with MQTT sink
    let db = AimDb::build_with(TokioAdapter::new(), |b| {
        b.register_record::<SensorData>(&cfg);
    }).unwrap();
    
    let (config, worker) = MqttSinkBuilder::new(&broker.url)
        .client_id("test-client")
        .build();
    
    db.init_outbox::<MqttMsg, _>(config, worker).unwrap();
    
    // Subscribe to topic
    let mut sub = broker.subscribe("sensors/#").await;
    
    // Produce sensor data (consumer will enqueue to MQTT)
    db.produce(SensorData { id: 1, temp: 23.5 }).await.unwrap();
    
    // Verify message received
    let msg = sub.recv().await.unwrap();
    assert_eq!(msg.topic, "sensors/1");
}
```

### Performance Benchmarks

```rust
// benches/outbox_throughput.rs

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_enqueue_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(setup_bench_db());
    
    c.bench_function("enqueue_single", |b| {
        b.to_async(&rt).iter(|| async {
            db.enqueue(TestMsg { data: vec![0u8; 128] }).await.unwrap();
        });
    });
}

fn bench_multi_producer_throughput(c: &mut Criterion) {
    // Benchmark with 1, 2, 4, 8 concurrent producers
}

criterion_group!(benches, bench_enqueue_latency, bench_multi_producer_throughput);
criterion_main!(benches);
```

---

## Migration Path

### Phase 1: Core Implementation (Week 1-2)

- [ ] Add `outboxes` field to `AimDbInner`
- [ ] Implement `AnySender` trait and `StoredSender<T>`
- [ ] Add `SinkWorker` trait and `WorkerHandle`
- [ ] Implement `init_outbox()` in `AimDb`
- [ ] Add `enqueue()` to `Emitter` and `AimDb`
- [ ] Add `OutboxConfig` and `OverflowBehavior`
- [ ] Unit tests for core functionality

### Phase 2: Runtime Adapters (Week 2-3)

- [ ] Tokio `mpsc` integration in `aimdb-tokio-adapter`
- [ ] Embassy `Channel` integration in `aimdb-embassy-adapter`
- [ ] Runtime trait extension for `create_mpsc_channel()`
- [ ] Cross-runtime tests (Tokio + Embassy)

### Phase 3: Connector Crates (Week 3-5)

- [ ] **aimdb-mqtt-connector**: MQTT sink with `rumqttc`
- [ ] **aimdb-kafka-connector**: Kafka producer with `rdkafka`
- [ ] **aimdb-dds-connector**: DDS publisher (future)
- [ ] Example applications for each connector
- [ ] Integration tests with real brokers (Docker Compose)

### Phase 4: Documentation & Examples (Week 5-6)

- [ ] API documentation with examples
- [ ] Connector usage guides
- [ ] Performance tuning guide
- [ ] Migration guide from direct connections
- [ ] Example: Multi-protocol gateway (MQTT + Kafka)

---

## Future Work

### Enhancements

1. **Connection Pooling**
   ```rust
   // Multiple workers per payload type (keyed by connection params)
   db.init_outbox_pool::<MqttMsg, _>(
       pool_size: 4,
       factory: || MqttWorker::new(...),
   )?;
   ```

2. **Load Balancing**
   ```rust
   pub enum LoadBalanceStrategy {
       RoundRobin,
       LeastLoaded,
       Random,
   }
   ```

3. **Retry Policies**
   ```rust
   pub struct RetryConfig {
       max_retries: usize,
       backoff: BackoffStrategy,
       retry_on: Vec<ErrorKind>,
   }
   ```

4. **Message Transformation**
   ```rust
   db.init_outbox_with_transform::<SensorData, MqttMsg, _>(
       config,
       worker,
       |data| MqttMsg {
           topic: format!("sensors/{}", data.id),
           payload: serde_json::to_vec(&data).unwrap(),
       },
   )?;
   ```

5. **Dead Letter Queue**
   ```rust
   pub struct DeadLetterConfig {
       max_failures: usize,
       dlq_outbox: TypeId,
   }
   ```

6. **Metrics Integration**
   ```rust
   // Automatic metrics per outbox
   metrics::counter!("outbox.enqueue", "type" => "MqttMsg").increment(1);
   metrics::histogram!("outbox.latency", "type" => "MqttMsg").record(latency_us);
   ```

### Inbound Connectors (Source/Subscribe)

Separate design needed for:
- MQTT subscribers → AimDB records
- Kafka consumers → AimDB records
- DDS subscribers → AimDB records

**Pattern:** MPMC (Multi-Producer Multiple-Consumer) with source workers

---

## Appendices

### Appendix A: Embassy Static Channel Implementation

```rust
// Option 1: Static pool with const generics

use embassy_sync::channel::Channel;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

static MQTT_CHANNEL: Channel<CriticalSectionRawMutex, MqttMsg, 1024> =
    Channel::new();

static KAFKA_CHANNEL: Channel<CriticalSectionRawMutex, KafkaMsg, 2048> =
    Channel::new();

impl EmbassyAdapter {
    pub fn create_mqtt_outbox(&self) -> (
        embassy_sync::channel::Sender<'static, MqttMsg>,
        embassy_sync::channel::Receiver<'static, MqttMsg>,
    ) {
        MQTT_CHANNEL.split()
    }
}

// Option 2: Heap allocation with Box::leak

impl EmbassyAdapter {
    pub fn create_mpsc_channel<T: Send + 'static>(
        &self,
        capacity: usize,
    ) -> (OutboxSender<T>, OutboxReceiver<T>) {
        // This requires 'alloc' feature
        let channel = Box::leak(Box::new(
            Channel::<CriticalSectionRawMutex, T, 1024>::new()
        ));
        channel.split()
    }
}
```

### Appendix B: Type-Safe Outbox Example

```rust
// Define payload types

#[derive(Clone, Debug)]
pub struct MqttMsg {
    pub topic: String,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct KafkaMsg {
    pub topic: String,
    pub key: Option<Vec<u8>>,
    pub payload: Vec<u8>,
}

// Initialize outboxes

let db = AimDb::build_with(rt, |b| {
    b.register_record::<SensorData>(&cfg);
}).unwrap();

// MQTT outbox
db.init_outbox::<MqttMsg, _>(
    OutboxConfig::default(),
    MqttWorker::new("mqtt://broker", "client"),
)?;

// Kafka outbox
db.init_outbox::<KafkaMsg, _>(
    OutboxConfig::default(),
    KafkaWorker::new(vec!["kafka:9092"]),
)?;

// Usage in record consumer

impl RecordT for SensorData {
    fn register(reg: &mut RecordRegistrar<Self>, cfg: &Self::Config) {
        reg.consumer(|em, data| async move {
            // Send to MQTT
            em.enqueue(MqttMsg {
                topic: format!("sensors/{}", data.id),
                payload: serde_json::to_vec(&data).unwrap(),
            }).await.unwrap();
            
            // Send to Kafka
            em.enqueue(KafkaMsg {
                topic: "sensor-events".into(),
                key: Some(data.id.to_string().into_bytes()),
                payload: serde_json::to_vec(&data).unwrap(),
            }).await.unwrap();
        });
    }
}
```

### Appendix C: Error Handling Patterns

```rust
// Pattern 1: Fail fast
match em.enqueue(msg).await {
    Ok(()) => {},
    Err(DbError::NotFound(_)) => {
        // Outbox not initialized - critical error
        return Err(...);
    }
    Err(DbError::ChannelClosed(_)) => {
        // Worker died - restart or escalate
        worker_handle.restart()?;
    }
    Err(DbError::ChannelFull(_)) => {
        // Buffer full - drop or retry
        eprintln!("Dropping message due to full buffer");
    }
    Err(e) => return Err(e),
}

// Pattern 2: Best effort
let _ = em.try_enqueue(msg);  // Fire and forget

// Pattern 3: Retry with backoff
let mut retries = 0;
loop {
    match em.enqueue(msg.clone()).await {
        Ok(()) => break,
        Err(e) if retries < 3 => {
            retries += 1;
            tokio::time::sleep(Duration::from_millis(100 * retries)).await;
        }
        Err(e) => return Err(e),
    }
}
```

### Appendix D: Comparison with Alternatives

| Approach | Pros | Cons | AimDB Fit |
|----------|------|------|-----------|
| **Direct connections in consumers** | Simple, low latency | Connection duplication, hard to manage | ❌ Poor |
| **Shared client via Arc<Mutex>** | Single connection | Lock contention, not async-friendly | ❌ Poor |
| **MPSC outbox (this design)** | Multi-producer, single connection, type-safe | Extra indirection (~500ns) | ✅ Excellent |
| **Actor model (separate actor per sink)** | Full isolation, restart capability | More complex, message overhead | ⚠️ Overkill |
| **Broadcast + filter** | One channel for all | Type erasure, filtering overhead | ❌ No type safety |

---

## Conclusion

The MPSC outbox pattern provides a **clean, type-safe, and efficient** mechanism for AimDB to interface with external protocol connectors. By separating outboxes from the existing SPMC record buffers, we maintain the integrity of the internal producer-consumer architecture while enabling multi-producer access to external systems.

**Key Advantages:**
- ✅ Prevents connection duplication (single worker per outbox)
- ✅ Type-safe routing via `TypeId`
- ✅ Runtime agnostic (Tokio + Embassy)
- ✅ Configurable backpressure and overflow handling
- ✅ Extensible via `SinkWorker` trait
- ✅ Minimal performance overhead (<1µs per enqueue)

**Next Steps:**
1. Implement core MPSC outbox in `aimdb-core`
2. Add runtime adapter support (Tokio, Embassy)
3. Create connector crates (MQTT, Kafka)
4. Write comprehensive tests and documentation

This design positions AimDB as a powerful hub for real-time data flow between embedded devices, edge systems, and cloud infrastructure.
