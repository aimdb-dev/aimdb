# Design: Dynamic MQTT Topics

**Status**: ✅ Implemented  
**Author**: GitHub Copilot  
**Date**: 2026-01-02 (Implemented: 2026-01-03)  
**Target**: Both `std` and `no_std` (with `alloc`) environments

## Problem Statement

MQTT topics were previously defined statically at configuration time:

```rust
builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
    reg.link_to("mqtt://sensors/temp/indoor")  // Static topic
       .with_serializer(|temp| Ok(temp.to_json_vec()))
       .finish();
});
```

This prevented use cases where the topic should be derived from the data being published:

```rust
// Desired: topic based on sensor_id in the data
// Temperature { sensor_id: "kitchen-001", celsius: 22.5 }
// → publishes to: "sensors/temp/kitchen-001"
```

## Goals

1. ✅ **Minimal Changes**: Add dynamic topic support with fewest possible code modifications
2. ✅ **Backward Compatible**: Existing static topic configuration continues to work unchanged
3. ✅ **Type Safe**: Topic provider has access to the typed value `T` (outbound)
4. ✅ **Trait-Based**: Enable reusable, testable implementations that can be shared across record types
5. ✅ **no_std Compatible**: Works in both `std` and `no_std + alloc` environments
6. ✅ **Inbound Support**: Allow runtime topic resolution for late-binding scenarios (smart contracts, discovery)

## Non-Goals

- Topic validation at configuration time

## Solution

### Overview

**Outbound (AimDB → External):** A `TopicProvider` trait is implemented by smart data contracts to dynamically determine MQTT topics based on the data being published. The trait is `no_std` compatible and uses type erasure for storage while preserving type safety at the implementation level.

**Inbound (External → AimDB):** A `TopicResolverFn` closure type resolves the subscription topic at connector startup. This supports late-binding scenarios where the topic isn't known at compile time but is determined from configuration, smart contracts, or service discovery.

### Outbound: TopicProvider

```rust
builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
    reg.link_to("mqtt://sensors/temp/default")  // Fallback topic
       .with_topic_provider(TemperatureTopicProvider)
       .with_serializer(|temp| Ok(temp.to_json_vec()))
       .finish();
});

// Implemented in a separate crate (e.g., smart-data-contracts)
struct TemperatureTopicProvider;

impl TopicProvider<Temperature> for TemperatureTopicProvider {
    fn topic(&self, value: &Temperature) -> Option<String> {
        Some(format!("sensors/temp/{}", value.sensor_id))
    }
}
```

### Inbound: TopicResolverFn

```rust
builder.configure::<SensorData>(SensorKey::MeshNode, |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link_from("mqtt://mesh/default/data")  // Fallback topic
           .with_topic_resolver(|| {
               // Read from smart contract, config service, discovery, etc.
               let node_id = smart_contract.get_producer_node_id()?;
               Some(format!("mesh/{}/data", node_id))
           })
           .with_deserializer(|bytes| parse_sensor_data(bytes))
           .finish();
});
```

**Note on Configuration Changes:** The `TopicResolverFn` is called once at connector startup. If configuration sources (smart contracts, service discovery, etc.) change at runtime, a server restart is required to pick up the new topic. This is the recommended approach for most deployments:

```
Git push (contract change) → CI/CD detects → Server restart → TopicResolverFn runs → Fresh topic resolved
```

This matches standard 12-factor app deployment patterns and ensures clean state on every deployment. For zero-downtime dynamic topic changes, see "Dynamic Topic Watch" in Future Extensions.

### Design Rationale

**Why a trait instead of a callback?**

A trait-based approach enables:
- Reusable implementations that can be shared across multiple record types
- Better testability and mockability
- Compile-time interface enforcement
- Consistent pattern with other AimDB extension points

**Why `Option<String>` return type?**

- `Some(topic)` → Use this dynamic topic
- `None` → Fall back to the static topic from `link_to()` URL

This allows conditional routing where most values use the default topic but specific conditions override it.

**Why type erasure with `TopicProviderWrapper`?**

The `ConnectorLink` struct stores links for different record types in a unified `Vec`. Type erasure via `Arc<dyn TopicProviderAny>` allows storing heterogeneous providers while the concrete implementation maintains type safety through `Any::downcast_ref()`. The wrapper pattern uses `PhantomData<fn(T) -> T>` to avoid Send/Sync variance issues.

**no_std Compatibility**

The implementation uses `alloc::string::String` and `alloc::sync::Arc` which are available in `no_std + alloc` environments. This matches the existing pattern used by `SerializerFn` and other type-erased callbacks in the codebase.

## Implementation

### Changes Required

#### 1. Add `TopicProvider` Trait and Type-Erased Wrapper

**File**: `aimdb-core/src/connector.rs`

```rust
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;

/// Trait for dynamic topic providers (outbound only)
///
/// Implement this trait to dynamically determine MQTT topics based on
/// the data being published. This enables reusable routing logic that
/// can be shared across multiple record types.
///
/// # Type Safety
///
/// The trait is generic over `T`, providing compile-time type safety
/// at the implementation site. Type erasure occurs only at storage time.
///
/// # no_std Compatibility
///
/// Works in both `std` and `no_std + alloc` environments.
///
/// # Example
///
/// ```rust,ignore
/// struct SensorTopicProvider;
///
/// impl TopicProvider<Temperature> for SensorTopicProvider {
///     fn topic(&self, value: &Temperature) -> Option<String> {
///         Some(format!("sensors/temp/{}", value.sensor_id))
///     }
/// }
/// ```
pub trait TopicProvider<T>: Send + Sync {
    /// Determine the topic for a given value
    ///
    /// Returns `Some(topic)` to use a dynamic topic, or `None` to fall back
    /// to the static topic from the `link_to()` URL.
    fn topic(&self, value: &T) -> Option<String>;
}

/// Type-erased topic provider trait (internal)
///
/// Allows storing providers for different types in a unified collection.
/// The concrete type is recovered via `Any::downcast_ref()` at runtime.
pub trait TopicProviderAny: Send + Sync {
    /// Get the topic for a type-erased value
    ///
    /// Returns `None` if the value type doesn't match or if the provider
    /// returns `None` for this value.
    fn topic_any(&self, value: &dyn Any) -> Option<String>;
}

/// Type alias for stored topic provider (no_std compatible)
pub type TopicProviderFn = Arc<dyn TopicProviderAny>;

/// Blanket implementation for type erasure
impl<T, P> TopicProviderAny for P
where
    T: 'static,
    P: TopicProvider<T> + Send + Sync,
{
    fn topic_any(&self, value: &dyn Any) -> Option<String> {
        value.downcast_ref::<T>().and_then(|v| self.topic(v))
    }
}
```

#### 2. Extend `ConnectorLink` with Topic Provider

**File**: `aimdb-core/src/connector.rs`

Add field to `ConnectorLink`:

```rust
pub struct ConnectorLink {
    pub url: ConnectorUrl,
    pub config: Vec<(String, String)>,
    pub serializer: Option<SerializerFn>,
    #[cfg(feature = "alloc")]
    pub consumer_factory: Option<ConsumerFactoryFn>,
    /// Optional dynamic topic provider (NEW)
    pub topic_provider: Option<TopicProviderFn>,
}
```

Update `ConnectorLink::new()` and `Debug` impl to include the new field.

#### 3. Add `with_topic_provider()` to `OutboundConnectorBuilder`

**File**: `aimdb-core/src/typed_api.rs`

```rust
impl<'a, T, R> OutboundConnectorBuilder<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Sets a dynamic topic provider
    ///
    /// The provider receives the value being published and returns
    /// the MQTT topic to publish to. Return `None` to use the default
    /// static topic from the URL.
    ///
    /// # Type Safety
    ///
    /// The provider is type-checked at compile time against `T`.
    /// Type erasure occurs internally for storage.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// struct SensorTopicProvider;
    /// 
    /// impl TopicProvider<Temperature> for SensorTopicProvider {
    ///     fn topic(&self, value: &Temperature) -> Option<String> {
    ///         Some(format!("sensors/temp/{}", value.sensor_id))
    ///     }
    /// }
    ///
    /// reg.link_to("mqtt://sensors/default")
    ///    .with_topic_provider(SensorTopicProvider)
    ///    .with_serializer(...)
    ///    .finish();
    /// ```
    pub fn with_topic_provider<P>(mut self, provider: P) -> Self
    where
        P: crate::connector::TopicProvider<T> + 'static,
    {
        // Type-erase the provider via the blanket TopicProviderAny impl
        self.topic_provider = Some(Arc::new(provider));
        self
    }
}
```

#### 4. Update `OutboundRoute` Type Alias and `collect_outbound_routes()`

**File**: `aimdb-core/src/builder.rs`

Extend the route tuple to include the topic provider:

```rust
/// Type alias for outbound route tuples returned by `collect_outbound_routes`
///
/// Each tuple contains:
/// - `String` - Topic/key from the URL path (default topic)
/// - `Box<dyn ConsumerTrait>` - Consumer for this record
/// - `SerializerFn` - User-provided serializer
/// - `Vec<(String, String)>` - Configuration options
/// - `Option<TopicProviderFn>` - Optional dynamic topic provider (NEW)
#[cfg(feature = "alloc")]
type OutboundRoute = (
    String,
    Box<dyn crate::connector::ConsumerTrait>,
    crate::connector::SerializerFn,
    Vec<(String, String)>,
    Option<crate::connector::TopicProviderFn>,
);
```

Update `collect_outbound_routes()` to include the provider in returned tuples.

#### 5. Use Topic Provider in MQTT Connector (Tokio)

**File**: `aimdb-mqtt-connector/src/tokio_client.rs`

Modify `spawn_outbound_publishers()`:

```rust
fn spawn_outbound_publishers<R>(
    &self,
    db: &aimdb_core::builder::AimDb<R>,
    routes: Vec<(
        String,
        Box<dyn aimdb_core::connector::ConsumerTrait>,
        aimdb_core::connector::SerializerFn,
        Vec<(String, String)>,
        Option<aimdb_core::connector::TopicProviderFn>,  // NEW
    )>,
) -> aimdb_core::DbResult<()>
where
    R: aimdb_executor::Spawn + 'static,
{
    for (default_topic, consumer, serializer, config, topic_provider) in routes {
        // ... existing setup code ...

        runtime.spawn(async move {
            let mut reader = consumer.subscribe_any().await?;

            while let Ok(value_any) = reader.recv_any().await {
                // Determine topic: dynamic or default
                let topic = topic_provider
                    .as_ref()
                    .and_then(|provider| provider.topic_any(&*value_any))
                    .unwrap_or_else(|| default_topic.clone());

                // Serialize and publish
                let bytes = serializer(&*value_any)?;
                client.publish(&topic, qos, retain, bytes).await?;
            }
        })?;
    }
    Ok(())
}
```

#### 6. Use Topic Provider in MQTT Connector (Embassy)

**File**: `aimdb-mqtt-connector/src/embassy_client.rs`

Apply the same pattern as the Tokio client, using `topic_provider.topic_any()` to determine the topic.

#### 7. Update KNX Connectors (Both Runtimes)

**Files**: 
- `aimdb-knx-connector/src/tokio_client.rs`
- `aimdb-knx-connector/src/embassy_client.rs`

The KNX connectors also define local `OutboundRoute` type aliases and implement `spawn_outbound_publishers()`. Apply the same pattern to support dynamic group address routing:

```rust
// In spawn_outbound_publishers()
for (default_destination, consumer, serializer, config, topic_provider) in routes {
    // ... existing setup code ...

    while let Ok(value_any) = reader.recv_any().await {
        // Determine destination: dynamic or default
        let destination = topic_provider
            .as_ref()
            .and_then(|provider| provider.topic_any(&*value_any))
            .unwrap_or_else(|| default_destination.clone());

        // Parse group address and send telegram
        let group_addr = GroupAddress::from_str(&destination)?;
        // ...
    }
}
```

#### 8. Export `OutboundRoute` from `aimdb-core` (Recommended)

**File**: `aimdb-core/src/lib.rs`

Currently, each connector defines its own local `OutboundRoute` type alias. To ensure consistency and reduce duplication, export the type from `aimdb-core`:

```rust
// In aimdb-core/src/lib.rs
#[cfg(feature = "alloc")]
pub use builder::OutboundRoute;
```

Then connectors can import it:

```rust
// In connector crates
use aimdb_core::OutboundRoute;
```

This ensures all connectors stay in sync when the tuple structure changes.

---

### Inbound Implementation

#### 9. Add `TopicResolverFn` Type Alias

**File**: `aimdb-core/src/connector.rs`

```rust
/// Topic resolver function for inbound connections (late-binding)
///
/// Called once at connector startup to resolve the subscription topic.
/// Returns `Some(topic)` to use a dynamic topic, or `None` to fall back
/// to the static topic from the `link_from()` URL.
///
/// # Use Cases
///
/// - Topics determined from smart contracts at runtime
/// - Service discovery integration
/// - Environment-specific topic configuration
/// - Topics read from configuration files or databases
///
/// # no_std Compatibility
///
/// Works in both `std` and `no_std + alloc` environments.
pub type TopicResolverFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;
```

#### 10. Extend `InboundConnectorLink` with Topic Resolver

**File**: `aimdb-core/src/connector.rs`

Add field to `InboundConnectorLink`:

```rust
pub struct InboundConnectorLink {
    pub url: ConnectorUrl,
    pub config: Vec<(String, String)>,
    pub deserializer: Option<DeserializerFn>,
    /// Optional dynamic topic resolver (NEW)
    pub topic_resolver: Option<TopicResolverFn>,
}
```

#### 11. Add `with_topic_resolver()` to `InboundConnectorBuilder`

**File**: `aimdb-core/src/typed_api.rs`

```rust
impl<'a, T, R> InboundConnectorBuilder<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::Spawn + 'static,
{
    /// Sets a dynamic topic resolver for late-binding scenarios
    ///
    /// The resolver is called once at connector startup to determine
    /// the subscription topic. Return `None` to use the default
    /// static topic from the URL.
    ///
    /// # Use Cases
    ///
    /// - Topics determined from smart contracts at runtime
    /// - Service discovery integration
    /// - Environment-specific topic configuration
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// reg.link_from("mqtt://mesh/default/data")  // Fallback topic
    ///    .with_topic_resolver(|| {
    ///        // Read from smart contract, config service, etc.
    ///        let node_id = smart_contract.get_producer_node_id()?;
    ///        Some(format!("mesh/{}/data", node_id))
    ///    })
    ///    .with_deserializer(|bytes| parse_sensor_data(bytes))
    ///    .finish();
    /// ```
    pub fn with_topic_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.topic_resolver = Some(Arc::new(resolver));
        self
    }
}
```

#### 12. Update `InboundRoute` Type Alias and `collect_inbound_routes()`

**File**: `aimdb-core/src/builder.rs`

Extend the route tuple to include the topic resolver:

```rust
/// Type alias for inbound route tuples returned by `collect_inbound_routes`
///
/// Each tuple contains:
/// - `String` - Topic/key from the URL path (default topic)
/// - `ProducerTrait` - Producer for this record
/// - `DeserializerFn` - User-provided deserializer
/// - `Vec<(String, String)>` - Configuration options
/// - `Option<TopicResolverFn>` - Optional dynamic topic resolver (NEW)
#[cfg(feature = "alloc")]
type InboundRoute = (
    String,
    Box<dyn crate::connector::ProducerTrait>,
    crate::connector::DeserializerFn,
    Vec<(String, String)>,
    Option<crate::connector::TopicResolverFn>,
);
```

#### 13. Use Topic Resolver in MQTT Connector (Tokio)

**File**: `aimdb-mqtt-connector/src/tokio_client.rs`

Modify `spawn_inbound_subscribers()`:

```rust
fn spawn_inbound_subscribers<R>(
    &self,
    db: &aimdb_core::builder::AimDb<R>,
    routes: Vec<InboundRoute>,
) -> aimdb_core::DbResult<()>
where
    R: aimdb_executor::Spawn + 'static,
{
    for (default_topic, producer, deserializer, config, topic_resolver) in routes {
        // Determine topic: resolve dynamically or use default
        let topic = topic_resolver
            .as_ref()
            .and_then(|resolver| resolver())
            .unwrap_or_else(|| default_topic.clone());

        // Subscribe to the resolved topic
        client.subscribe(&topic, qos).await?;

        // ... existing message handling loop ...
    }
    Ok(())
}
```

#### 14. Apply Same Pattern to Embassy and KNX Connectors

Apply the inbound topic resolver pattern to:
- `aimdb-mqtt-connector/src/embassy_client.rs`
- `aimdb-knx-connector/src/tokio_client.rs`
- `aimdb-knx-connector/src/embassy_client.rs`

## File Change Summary

### Outbound (TopicProvider)

| File | Change Type | Lines (est.) |
|------|-------------|--------------|
| `aimdb-core/src/connector.rs` | Add trait + type alias + struct field | ~50 |
| `aimdb-core/src/typed_api.rs` | Add builder method + field (OutboundConnectorBuilder) | ~25 |
| `aimdb-core/src/builder.rs` | Extend OutboundRoute type + return + export | ~15 |
| `aimdb-core/src/lib.rs` | Re-export OutboundRoute | ~3 |
| `aimdb-mqtt-connector/src/tokio_client.rs` | Use topic provider, import shared type | ~15 |
| `aimdb-mqtt-connector/src/embassy_client.rs` | Use topic provider, remove local type alias | ~15 |
| `aimdb-knx-connector/src/tokio_client.rs` | Use topic provider, remove local type alias | ~15 |
| `aimdb-knx-connector/src/embassy_client.rs` | Use topic provider, remove local type alias | ~15 |
| **Outbound Total** | | **~153 lines** |

### Inbound (TopicResolverFn)

| File | Change Type | Lines (est.) |
|------|-------------|--------------|
| `aimdb-core/src/connector.rs` | Add TopicResolverFn type alias + struct field | ~15 |
| `aimdb-core/src/typed_api.rs` | Add builder method + field (InboundConnectorBuilder) | ~20 |
| `aimdb-core/src/builder.rs` | Extend InboundRoute type + return | ~10 |
| `aimdb-mqtt-connector/src/tokio_client.rs` | Resolve topic at startup | ~10 |
| `aimdb-mqtt-connector/src/embassy_client.rs` | Resolve topic at startup | ~10 |
| `aimdb-knx-connector/src/tokio_client.rs` | Resolve destination at startup | ~10 |
| `aimdb-knx-connector/src/embassy_client.rs` | Resolve destination at startup | ~10 |
| **Inbound Total** | | **~85 lines** |

**Combined Total: ~238 lines**

## Usage Examples

### Basic Dynamic Topic (Trait Implementation)

```rust
use aimdb_core::connector::TopicProvider;
use alloc::string::String;

// Define the provider (can be in a separate crate)
struct SensorTopicProvider;

impl TopicProvider<Temperature> for SensorTopicProvider {
    fn topic(&self, value: &Temperature) -> Option<String> {
        Some(format!("sensors/temp/{}", value.sensor_id))
    }
}

// Use in record configuration
builder.configure::<Temperature>("sensors.temperature", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .source(temperature_producer)
       .link_to("mqtt://sensors/temp/default")
           .with_topic_provider(SensorTopicProvider)
           .with_serializer(|temp| Ok(temp.to_json_vec()))
           .finish();
});
```

### Conditional Topic

```rust
struct AlertTopicProvider;

impl TopicProvider<Temperature> for AlertTopicProvider {
    fn topic(&self, temp: &Temperature) -> Option<String> {
        if temp.celsius > 30.0 {
            Some("alerts/temperature/high".into())
        } else {
            Some(format!("sensors/temp/{}", temp.sensor_id))
        }
    }
}
```

### Fallback to Default

```rust
struct OptionalTopicProvider;

impl TopicProvider<Temperature> for OptionalTopicProvider {
    fn topic(&self, temp: &Temperature) -> Option<String> {
        // Return None to use the default topic from link_to()
        if temp.sensor_id.is_empty() {
            None
        } else {
            Some(format!("sensors/temp/{}", temp.sensor_id))
        }
    }
}
```

### Multi-Level Topics

```rust
struct BuildingTopicProvider;

impl TopicProvider<SensorData> for BuildingTopicProvider {
    fn topic(&self, data: &SensorData) -> Option<String> {
        Some(format!(
            "buildings/{}/floors/{}/rooms/{}/sensors/{}",
            data.building_id,
            data.floor,
            data.room,
            data.sensor_type
        ))
    }
}
```

### Stateful Provider with Runtime Configuration

For providers that need runtime configuration:

```rust
use alloc::sync::Arc;
use spin::RwLock;  // no_std compatible (requires "rwlock" feature in spin)

/// Configuration that can be updated at runtime
pub struct TopicConfig {
    pub prefix: String,
    pub environment: String,
}

/// Stateful provider with shared configuration
pub struct ConfigurableTopicProvider {
    config: Arc<RwLock<TopicConfig>>,
}

impl ConfigurableTopicProvider {
    pub fn new(config: Arc<RwLock<TopicConfig>>) -> Self {
        Self { config }
    }
}

impl TopicProvider<Temperature> for ConfigurableTopicProvider {
    fn topic(&self, temp: &Temperature) -> Option<String> {
        let cfg = self.config.read();
        Some(format!(
            "{}/{}/temp/{}",
            cfg.environment,
            cfg.prefix,
            temp.sensor_id
        ))
    }
}

// Usage:
let topic_config = Arc::new(RwLock::new(TopicConfig {
    prefix: "sensors".into(),
    environment: "prod".into(),
}));

let provider = ConfigurableTopicProvider::new(topic_config.clone());

builder.configure::<Temperature>("sensors.temp", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .source(temperature_producer)
       .link_to("mqtt://default/topic")
           .with_topic_provider(provider)
           .with_serializer(|temp| Ok(temp.to_json_vec()))
           .finish();
});

// Later: change prefix at runtime
topic_config.write().prefix = "devices".into();
// All future publishes now use "prod/devices/temp/{sensor_id}"
```

### Inbound: Smart Contract Topic Resolution

For late-binding scenarios where the subscription topic is determined at runtime:

```rust
// Smart contract client (injected dependency)
let contract_client = Arc::new(SmartContractClient::new(contract_address));

builder.configure::<SensorData>(SensorKey::MeshNode, |reg| {
    let client = contract_client.clone();
    
    reg.buffer(BufferCfg::SingleLatest)
       .sink(sensor_data_handler)
       .link_from("mqtt://mesh/default/data")  // Fallback topic
           .with_topic_resolver(move || {
               // Called once at connector startup
               let node_id = client.get_producer_node_id().ok()?;
               Some(format!("mesh/{}/data", node_id))
           })
           .with_deserializer(|bytes| serde_json::from_slice(bytes))
           .finish();
});
```

### Inbound: Service Discovery Integration

```rust
let discovery = Arc::new(ServiceDiscovery::new(consul_url));

builder.configure::<DeviceStatus>(DeviceKey::Remote, |reg| {
    let disc = discovery.clone();
    
    reg.buffer(BufferCfg::SingleLatest)
       .sink(status_handler)
       .link_from("mqtt://devices/unknown/status")
           .with_topic_resolver(move || {
               // Resolve from service discovery
               let device = disc.find_service("device-controller").ok()?;
               Some(format!("devices/{}/status", device.id))
           })
           .with_deserializer(|bytes| DeviceStatus::from_bytes(bytes))
           .finish();
});
```

### Inbound: Environment-Based Configuration

```rust
let env_config = Arc::new(EnvConfig::load());

builder.configure::<Metrics>(MetricKey::System, |reg| {
    let config = env_config.clone();
    
    reg.buffer(BufferCfg::Ring { capacity: 100 })
       .sink(metrics_collector)
       .link_from("mqtt://metrics/default")
           .with_topic_resolver(move || {
               // Read from environment config
               config.get("METRICS_TOPIC").map(|t| t.to_string())
           })
           .with_deserializer(|bytes| Metrics::decode(bytes))
           .finish();
});
```

## Testing Strategy

### Outbound Tests
1. **Unit Tests**: TopicProvider trait implementation and type erasure
2. **Integration Tests**: End-to-end publish with dynamic topics (both Tokio and Embassy)
3. **Backward Compatibility**: Verify existing static topic code unchanged
4. **no_std Tests**: Compile for embedded targets (thumbv7em-none-eabihf)

```rust
#[test]
fn test_topic_provider_type_erasure() {
    struct TestProvider;
    impl TopicProvider<Temperature> for TestProvider {
        fn topic(&self, t: &Temperature) -> Option<String> {
            Some(format!("test/{}", t.sensor_id))
        }
    }
    
    let provider: Arc<dyn TopicProviderAny> = Arc::new(TestProvider);
    let temp = Temperature { sensor_id: "abc".into(), celsius: 22.0 };
    
    assert_eq!(
        provider.topic_any(&temp),
        Some("test/abc".into())
    );
}

#[test]
fn test_topic_provider_type_mismatch() {
    struct TestProvider;
    impl TopicProvider<Temperature> for TestProvider {
        fn topic(&self, _: &Temperature) -> Option<String> {
            Some("topic".into())
        }
    }
    
    let provider: Arc<dyn TopicProviderAny> = Arc::new(TestProvider);
    let wrong_type = "not a temperature";
    
    // Type mismatch returns None (falls back to default topic)
    assert_eq!(provider.topic_any(&wrong_type), None);
}

#[tokio::test]
async fn test_dynamic_topic_publish() {
    // Setup with topic provider
    // Publish Temperature { sensor_id: "test-001", ... }
    // Verify MQTT message arrives at "sensors/temp/test-001"
}

#[tokio::test]
async fn test_static_topic_unchanged() {
    // Setup WITHOUT topic provider (existing code)
    // Verify behavior unchanged
}
```

### Inbound Tests

```rust
#[test]
fn test_topic_resolver_returns_some() {
    let resolver: TopicResolverFn = Arc::new(|| {
        Some("resolved/topic".into())
    });
    
    assert_eq!(resolver(), Some("resolved/topic".into()));
}

#[test]
fn test_topic_resolver_returns_none() {
    let resolver: TopicResolverFn = Arc::new(|| None);
    
    // Returns None, should fall back to default topic
    assert_eq!(resolver(), None);
}

#[test]
fn test_topic_resolver_with_captured_state() {
    let config = Arc::new(Mutex::new(Some("dynamic/topic".into())));
    let config_clone = config.clone();
    
    let resolver: TopicResolverFn = Arc::new(move || {
        config_clone.lock().unwrap().clone()
    });
    
    assert_eq!(resolver(), Some("dynamic/topic".into()));
    
    // Clear config
    *config.lock().unwrap() = None;
    assert_eq!(resolver(), None);
}

#[tokio::test]
async fn test_inbound_dynamic_subscription() {
    // Setup with topic resolver
    // Verify subscription to resolved topic
    // Publish to resolved topic
    // Verify data received
}

#[tokio::test]
async fn test_inbound_fallback_to_default() {
    // Setup with resolver that returns None
    // Verify subscription to default topic from link_from()
}
```

## Future Extensions

1. **Topic Pattern Routing**: Route messages based on topic patterns to different record types (e.g., wildcards)
2. **Topic Validation**: Optional compile-time or runtime topic validation via a `ValidatingTopicProvider` wrapper
3. **Metrics**: Track topics dynamically generated vs static via trait method hooks
4. **Derive Macro**: `#[derive(TopicProvider)]` for simple field-based routing
5. **Dynamic Topic Watch (Inbound)**: For zero-downtime topic changes without server restart

### Dynamic Topic Watch Pattern

For scenarios requiring runtime topic changes without restart (e.g., smart contract updates with zero downtime):

```rust
use tokio::sync::watch;  // or embassy_sync::watch for embedded

// 1. Setup: Create a watch channel with initial topic
let initial_topic = smart_contract.get_producer_topic();
let (topic_tx, topic_rx) = watch::channel(initial_topic);

// 2. Configure connector with watch receiver
builder.configure::<SensorData>(SensorKey::MeshNode, |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link_from("mqtt://mesh/default/data")
           .with_topic_watch(topic_rx.clone())  // Instead of with_topic_resolver
           .with_deserializer(|bytes| parse_sensor_data(bytes))
           .finish();
});

// 3. In your event loop - when smart contract changes:
smart_contract.on_config_changed(|event| {
    let new_topic = format!("mesh/{}/data", event.new_node_id);
    topic_tx.send(new_topic).ok();  // Connector auto-resubscribes
});
```

**Why `watch` channel:**
- Latest-value semantics (only current topic matters)
- Multiple receivers (same topic source for multiple connectors)
- Async-native (connector awaits on changes)
- No polling (zero overhead when unchanged)
- Works with Embassy (`embassy_sync::watch::Watch`)

**Estimated complexity:** ~50-80 lines per connector, plus API surface increase.

**Recommendation:** Start with restart-on-deploy; add this pattern when zero-downtime is a hard requirement.

## Alternatives Considered

### 1. Callback-Based (`Fn` closure)

```rust
.with_topic_provider(|temp: &Temperature| {
    Some(format!("sensors/{}", temp.sensor_id))
})
```

**Pros**: Concise, inline definition  
**Cons**: 
- Cannot be reused across different record configurations
- Less testable
- Harder to share implementations across record types

### 2. Template Strings

```rust
.link_to("mqtt://sensors/{sensor_id}/temp")
```

**Pros**: Declarative, familiar pattern  
**Cons**: Requires reflection, serde integration, less flexible, limited to field extraction

### 3. Separate `link_to_dynamic()` Method

```rust
.link_to_dynamic(|temp| format!("mqtt://sensors/{}/temp", temp.sensor_id))
```

**Pros**: Clear separation  
**Cons**: Duplicates builder logic, more code to maintain

## Decision

**Chosen**: Trait-based `TopicProvider` because:
- Enables reusable implementations shared across record types
- Better testability and mockability
- no_std compatible with `alloc`
- Type-safe with compile-time checking
- Consistent with Rust ecosystem patterns
- Supports both stateless and stateful implementations
- Works on both Tokio and Embassy runtimes

## Implementation Checklist

### Core (`aimdb-core`) - Outbound
- [ ] Add `TopicProvider<T>` trait to `connector.rs`
- [ ] Add `TopicProviderAny` trait for type erasure to `connector.rs`
- [ ] Add blanket impl `TopicProviderAny for TopicProvider<T>` to `connector.rs`
- [ ] Add `TopicProviderFn` type alias to `connector.rs`
- [ ] Add `topic_provider` field to `ConnectorLink`
- [ ] Update `ConnectorLink::new()` to initialize `topic_provider: None`
- [ ] Update `ConnectorLink` `Debug` impl to include new field
- [ ] Add `topic_provider` field to `OutboundConnectorBuilder`
- [ ] Implement `with_topic_provider()` method on `OutboundConnectorBuilder`
- [ ] Update `link_to()` constructor to initialize `topic_provider: None`
- [ ] Update deprecated `link()` constructor to initialize `topic_provider: None`
- [ ] Wire through `finish()` to store provider in `ConnectorLink`
- [ ] Update `OutboundRoute` type alias in `builder.rs`
- [ ] Update `collect_outbound_routes()` to include provider in tuple
- [ ] Export `OutboundRoute` from `lib.rs` (recommended)

### Core (`aimdb-core`) - Inbound
- [ ] Add `TopicResolverFn` type alias to `connector.rs`
- [ ] Add `topic_resolver` field to `InboundConnectorLink` (if exists, or `ConnectorLink`)
- [ ] Add `topic_resolver` field to `InboundConnectorBuilder`
- [ ] Implement `with_topic_resolver()` method on `InboundConnectorBuilder`
- [ ] Update `link_from()` constructor to initialize `topic_resolver: None`
- [ ] Wire through `finish()` to store resolver in connector link
- [ ] Update `InboundRoute` type alias in `builder.rs`
- [ ] Update `collect_inbound_routes()` to include resolver in tuple

### MQTT Connector (`aimdb-mqtt-connector`) - Outbound
- [ ] Update `spawn_outbound_publishers()` in `tokio_client.rs`
- [ ] Remove local `OutboundRoute` type alias, import from `aimdb_core`
- [ ] Update `spawn_outbound_publishers()` in `embassy_client.rs`
- [ ] Remove local `OutboundRoute` type alias in `embassy_client.rs`

### MQTT Connector (`aimdb-mqtt-connector`) - Inbound
- [ ] Update `spawn_inbound_subscribers()` in `tokio_client.rs` to resolve topic
- [ ] Update `spawn_inbound_subscribers()` in `embassy_client.rs` to resolve topic

### KNX Connector (`aimdb-knx-connector`) - Outbound
- [ ] Update `spawn_outbound_publishers()` in `tokio_client.rs`
- [ ] Remove local `OutboundRoute` type alias, import from `aimdb_core`
- [ ] Update `spawn_outbound_publishers()` in `embassy_client.rs`
- [ ] Remove local `OutboundRoute` type alias in `embassy_client.rs`

### KNX Connector (`aimdb-knx-connector`) - Inbound
- [ ] Update `spawn_inbound_subscribers()` in `tokio_client.rs` to resolve destination
- [ ] Update `spawn_inbound_subscribers()` in `embassy_client.rs` to resolve destination

### Testing & Validation
- [ ] Add unit tests for TopicProvider trait and type erasure
- [ ] Add unit tests for TopicResolverFn
- [ ] Add integration test (MQTT Tokio) - outbound
- [ ] Add integration test (MQTT Tokio) - inbound
- [ ] Add integration test (MQTT Embassy) - outbound
- [ ] Add integration test (MQTT Embassy) - inbound
- [ ] Add integration test (KNX Tokio) - optional
- [ ] Verify no_std compilation: `cargo check --target thumbv7em-none-eabihf --no-default-features --features alloc`
- [ ] Run full test suite: `make check`

### Documentation
- [ ] Update `aimdb-core/CHANGELOG.md`
- [ ] Update `aimdb-mqtt-connector/CHANGELOG.md`
- [ ] Update `aimdb-knx-connector/CHANGELOG.md`
- [ ] Add doc comments with examples
