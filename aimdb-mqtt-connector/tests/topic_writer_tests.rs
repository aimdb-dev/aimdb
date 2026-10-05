//! Integration tests for MQTT TopicWriter and TopicResolver functionality
//!
//! These tests verify the dynamic topic routing features:
//! - **TopicWriter**: Outbound (AimDB → MQTT) dynamic topic selection per-value
//! - **TopicResolver**: Inbound (MQTT → AimDB) late-binding topic resolution at startup
//!
//! The tests use mock data and don't require a running MQTT broker.

#![cfg(feature = "std")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::{TopicBuf, TopicOverflow, TopicWriter};
use aimdb_core::{AimDbBuilder, Producer, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// ============================================================================
// Test Types
// ============================================================================

/// Temperature reading with sensor_id for dynamic topic routing
#[derive(Clone, Debug)]
struct Temperature {
    sensor_id: String,
    celsius: f32,
}

impl Temperature {
    fn new(sensor_id: &str, celsius: f32) -> Self {
        Self {
            sensor_id: sensor_id.into(),
            celsius,
        }
    }

    fn to_json_vec(&self) -> Vec<u8> {
        format!(
            r#"{{"sensor_id":"{}","celsius":{:.1}}}"#,
            self.sensor_id, self.celsius
        )
        .into_bytes()
    }
}

/// Command type for inbound messages
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct Command {
    action: String,
    target: String,
}

impl Command {
    fn from_json(data: &[u8]) -> Result<Self, String> {
        // Simple parser for test purposes
        let s = std::str::from_utf8(data).map_err(|e| e.to_string())?;
        if s.contains("read") {
            Ok(Command {
                action: "read".into(),
                target: "sensor".into(),
            })
        } else {
            Err("Unknown command".into())
        }
    }
}

// ============================================================================
// TopicWriter Implementations
// ============================================================================

/// Dynamic topic writer that routes based on sensor_id
///
/// This demonstrates the core use case: routing to different MQTT topics
/// based on data content (e.g., multi-tenant, device-specific topics).
struct SensorIdTopic;

impl TopicWriter<Temperature> for SensorIdTopic {
    fn write_topic(
        &self,
        value: &Temperature,
        out: &mut TopicBuf<'_>,
    ) -> Result<bool, TopicOverflow> {
        write!(out, "sensors/temp/{}", value.sensor_id)?;
        Ok(true)
    }
}

/// Topic writer that falls back to the link's default topic
///
/// When the writer returns `Ok(false)`, the connector uses the default topic
/// from the link_to() URL.
struct FallbackTopic;

impl TopicWriter<Temperature> for FallbackTopic {
    fn write_topic(
        &self,
        value: &Temperature,
        out: &mut TopicBuf<'_>,
    ) -> Result<bool, TopicOverflow> {
        if value.sensor_id == "use-default" {
            return Ok(false);
        }
        write!(out, "sensors/custom/{}", value.sensor_id)?;
        Ok(true)
    }
}

/// Topic writer that uses temperature thresholds
///
/// Demonstrates conditional routing based on data values.
struct ThresholdTopic {
    threshold: f32,
}

impl TopicWriter<Temperature> for ThresholdTopic {
    fn write_topic(
        &self,
        value: &Temperature,
        out: &mut TopicBuf<'_>,
    ) -> Result<bool, TopicOverflow> {
        if value.celsius > self.threshold {
            out.push_str("alerts/high-temp")?;
        } else if value.celsius < 0.0 {
            out.push_str("alerts/freezing")?;
        } else {
            return Ok(false); // Use default topic for normal temps
        }
        Ok(true)
    }
}

/// What `writer` writes for `value` into 64 bytes: `Some(topic)`, or `None`
/// for the link's default.
fn written(writer: &dyn TopicWriter<Temperature>, value: &Temperature) -> Option<String> {
    let mut storage = [0u8; 64];
    let mut out = TopicBuf::new(&mut storage);
    match writer.write_topic(value, &mut out) {
        Ok(true) => Some(out.as_str().to_string()),
        Ok(false) => None,
        Err(TopicOverflow) => panic!("topic overflowed 64 bytes"),
    }
}

// ============================================================================
// Unit Tests for TopicWriter
// ============================================================================

#[test]
fn test_sensor_id_topic_writer() {
    let writer = SensorIdTopic;

    let temp_indoor = Temperature::new("indoor-001", 22.0);
    assert_eq!(
        written(&writer, &temp_indoor),
        Some("sensors/temp/indoor-001".into())
    );

    let temp_outdoor = Temperature::new("outdoor-garden", 5.5);
    assert_eq!(
        written(&writer, &temp_outdoor),
        Some("sensors/temp/outdoor-garden".into())
    );
}

#[test]
fn test_fallback_topic_writer() {
    let writer = FallbackTopic;

    // Normal case: returns custom topic
    let temp = Temperature::new("kitchen", 21.0);
    assert_eq!(
        written(&writer, &temp),
        Some("sensors/custom/kitchen".into())
    );

    // Fallback case: returns None
    let temp_default = Temperature::new("use-default", 20.0);
    assert_eq!(written(&writer, &temp_default), None);
}

#[test]
fn test_threshold_topic_writer() {
    let writer = ThresholdTopic { threshold: 30.0 };

    // Normal temperature: fallback to default
    let normal = Temperature::new("room", 22.0);
    assert_eq!(written(&writer, &normal), None);

    // High temperature: route to alert topic
    let hot = Temperature::new("server-room", 35.0);
    assert_eq!(written(&writer, &hot), Some("alerts/high-temp".into()));

    // Freezing: route to freezing alert
    let cold = Temperature::new("outdoor", -5.0);
    assert_eq!(written(&writer, &cold), Some("alerts/freezing".into()));
}

// ============================================================================
// Unit Tests for TopicResolver
// ============================================================================

#[test]
fn test_topic_resolver_from_env() {
    // Simulate reading topic from environment variable
    std::env::set_var("TEST_MQTT_TOPIC", "env/dynamic/topic");

    let resolver = || std::env::var("TEST_MQTT_TOPIC").ok();

    assert_eq!(resolver(), Some("env/dynamic/topic".into()));

    // Cleanup
    std::env::remove_var("TEST_MQTT_TOPIC");
}

#[test]
fn test_topic_resolver_fallback() {
    // When env var is not set, resolver returns None (use default)
    std::env::remove_var("NONEXISTENT_VAR");

    let resolver = || std::env::var("NONEXISTENT_VAR").ok();

    assert_eq!(resolver(), None);
}

#[test]
fn test_topic_resolver_with_config() {
    use std::sync::Mutex;

    // Simulate a configuration service
    let config = Arc::new(Mutex::new(Some("config/dynamic/topic".to_string())));
    let config_clone = config.clone();

    let resolver = move || config_clone.lock().unwrap().clone();

    assert_eq!(resolver(), Some("config/dynamic/topic".into()));

    // Config changes
    *config.lock().unwrap() = Some("config/updated/topic".into());
    // Note: In real usage, resolver is called once at startup, so this wouldn't affect runtime
}

// ============================================================================
// Integration Test: TopicWriter with AimDbBuilder (No Connector)
// ============================================================================

/// Test that a record can be configured without connector (verifies API)
///
/// Note: These tests verify the configuration API compiles and works,
/// but don't test actual MQTT connectivity (that requires a broker).
#[tokio::test]
async fn test_topic_writer_registration_api() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let produced_count = Arc::new(AtomicU32::new(0));
    let produced_count_clone = produced_count.clone();

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register without link_to (no connector needed) - just source and tap
    builder.configure::<Temperature>("test.sensor.dynamic", |reg| {
        let counter = produced_count_clone.clone();
        reg.buffer(BufferCfg::SingleLatest).source(
            move |_ctx: RuntimeContext, producer: Producer<Temperature>| {
                let counter = counter.clone();
                async move {
                    let temp = Temperature::new("test-001", 22.5);
                    producer.produce(temp);
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            },
        );
    });

    // Build should succeed (no connector registered)
    let db = builder.build().await;
    assert!(db.is_ok());
}

/// Test TopicWriter with MqttConnector registration
///
/// This test verifies the full configuration API including link_to + with_topic_writer
/// works correctly. Runtime requires actual MQTT broker.
#[tokio::test]
async fn test_topic_writer_with_connector_registration() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new("mqtt://localhost:1883")
            .with_client_id("test-topic-writer"),
    );

    // Register with a dynamic topic writer
    builder.configure::<Temperature>("test.sensor.dynamic", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(
                |_ctx: RuntimeContext, producer: Producer<Temperature>| async move {
                    let temp = Temperature::new("kitchen", 22.5);
                    producer.produce(temp);
                },
            )
            .link_to("mqtt://sensors/temp/default") // Fallback topic
            .with_topic_writer(64, SensorIdTopic) // Dynamic routing!
            .with_serializer(|_ctx, temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    // Build will succeed even without broker (connector is registered)
    // It won't connect, but validates the configuration
    let result = builder.build().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_topic_resolver_with_connector_registration() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    std::env::set_var("TEST_MQTT_TOPIC", "commands/test/dynamic");

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new("mqtt://localhost:1883")
            .with_client_id("test-topic-resolver"),
    );

    // Register with dynamic topic resolver
    builder.configure::<Command>("test.command.dynamic", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("mqtt://commands/default") // Fallback topic
            .with_topic_resolver(|| {
                // Late-binding: resolve from environment at startup
                std::env::var("TEST_COMMAND_TOPIC").ok()
            })
            .with_deserializer(|_ctx, data: &[u8]| Command::from_json(data))
            .finish();
    });

    // Build succeeds with connector registered
    assert!(builder.build().await.is_ok());

    // Cleanup
    std::env::remove_var("TEST_MQTT_TOPIC");
}

#[tokio::test]
async fn test_mixed_static_and_dynamic_topics() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new("mqtt://localhost:1883")
            .with_client_id("test-mixed-topics"),
    );

    // Static topic (traditional approach)
    builder.configure::<Temperature>("test.sensor.static", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(
                |_ctx: RuntimeContext, producer: Producer<Temperature>| async move {
                    producer.produce(Temperature::new("static", 20.0));
                },
            )
            .link_to("mqtt://sensors/temp/static-topic")
            .with_serializer(|_ctx, temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    // Dynamic topic (new approach)
    builder.configure::<Temperature>("test.sensor.dynamic", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(
                |_ctx: RuntimeContext, producer: Producer<Temperature>| async move {
                    producer.produce(Temperature::new("dynamic", 25.0));
                },
            )
            .link_to("mqtt://sensors/temp/fallback")
            .with_topic_fn(64, |temp, out| {
                write!(out, "sensors/temp/{}", temp.sensor_id)?;
                Ok(true)
            })
            .with_serializer(|_ctx, temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    // Both should coexist without issues
    assert!(builder.build().await.is_ok());
}

// ============================================================================
// Test TopicWriter as a typed trait object
// ============================================================================

#[test]
fn test_topic_writer_as_trait_object() {
    // Writers are stored as Arc<dyn TopicWriter<T>> and stay typed
    // end-to-end — a wrong-type call is unrepresentable.
    let writer: Arc<dyn TopicWriter<Temperature>> = Arc::new(SensorIdTopic);

    let temp = Temperature::new("kitchen", 22.0);
    assert_eq!(
        written(&*writer, &temp),
        Some("sensors/temp/kitchen".into())
    );
}

#[test]
fn test_a_topic_over_the_capacity_overflows() {
    let mut storage = [0u8; 8];
    let mut out = TopicBuf::new(&mut storage);
    let temp = Temperature::new("kitchen", 22.0);
    // "sensors/temp/kitchen" is 20 bytes.
    assert_eq!(
        SensorIdTopic.write_topic(&temp, &mut out),
        Err(TopicOverflow)
    );
}

// ============================================================================
// Test: Simulate Connector Topic Resolution Logic
// ============================================================================
//
// These tests simulate what `OutboundRoutes` does while it still holds the
// typed value: the written topic, or the route's default on `Ok(false)` or
// without a writer.

/// Simulates the fused reader's topic resolution for outbound messages
fn resolve_topic_like_connector(
    default_topic: &str,
    topic_writer: Option<&dyn TopicWriter<Temperature>>,
    value: &Temperature,
) -> String {
    topic_writer
        .and_then(|writer| written(writer, value))
        .unwrap_or_else(|| default_topic.to_string())
}

#[test]
fn test_connector_topic_resolution_with_dynamic_writer() {
    let writer = SensorIdTopic;
    let default_topic = "sensors/temp/default";

    // Test 1: The written topic is returned when the writer returns Ok(true)
    let temp_kitchen = Temperature::new("kitchen", 22.0);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &temp_kitchen);
    assert_eq!(resolved, "sensors/temp/kitchen");

    // Test 2: Different sensor_id → different topic
    let temp_bedroom = Temperature::new("bedroom", 19.5);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &temp_bedroom);
    assert_eq!(resolved, "sensors/temp/bedroom");

    // Test 3: Multi-tenant scenario
    let temp_tenant_a = Temperature::new("tenant-a/room-1", 21.0);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &temp_tenant_a);
    assert_eq!(resolved, "sensors/temp/tenant-a/room-1");
}

#[test]
fn test_connector_topic_resolution_fallback_to_default() {
    let writer = FallbackTopic;
    let default_topic = "sensors/temp/default";

    // Test 1: Writer returns Ok(true) → use the written topic
    let temp_kitchen = Temperature::new("kitchen", 22.0);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &temp_kitchen);
    assert_eq!(resolved, "sensors/custom/kitchen");

    // Test 2: Writer returns Ok(false) → fallback to default
    let temp_use_default = Temperature::new("use-default", 20.0);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &temp_use_default);
    assert_eq!(resolved, "sensors/temp/default"); // Fallback!
}

#[test]
fn test_connector_topic_resolution_no_writer() {
    let default_topic = "sensors/temp/static";

    // No writer configured → always use default
    let temp = Temperature::new("kitchen", 22.0);
    let resolved = resolve_topic_like_connector(default_topic, None, &temp);
    assert_eq!(resolved, "sensors/temp/static");
}

#[test]
fn test_connector_topic_resolution_with_threshold_writer() {
    let writer = ThresholdTopic { threshold: 30.0 };
    let default_topic = "sensors/temp/normal";

    // Normal temperature → fallback (writer returns Ok(false))
    let normal_temp = Temperature::new("room", 22.0);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &normal_temp);
    assert_eq!(resolved, "sensors/temp/normal"); // Default

    // High temperature → alert topic
    let hot_temp = Temperature::new("server-room", 35.0);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &hot_temp);
    assert_eq!(resolved, "alerts/high-temp"); // Dynamic

    // Freezing → freezing alert
    let cold_temp = Temperature::new("outdoor", -5.0);
    let resolved = resolve_topic_like_connector(default_topic, Some(&writer), &cold_temp);
    assert_eq!(resolved, "alerts/freezing"); // Dynamic
}

/// Test that verifies the inbound topic resolver is called correctly
#[test]
fn test_inbound_topic_resolver_simulation() {
    // Simulate what inbound_router does for TopicResolver
    fn resolve_inbound_topic(
        default_topic: &str,
        resolver: Option<&dyn Fn() -> Option<String>>,
    ) -> String {
        resolver
            .and_then(|r| r())
            .unwrap_or_else(|| default_topic.to_string())
    }

    // Test 1: Resolver returns dynamic topic
    let resolver_dynamic = || Some("commands/dynamic/kitchen".to_string());
    let topic = resolve_inbound_topic("commands/default", Some(&resolver_dynamic));
    assert_eq!(topic, "commands/dynamic/kitchen");

    // Test 2: Resolver returns None → fallback
    let resolver_fallback = || None;
    let topic = resolve_inbound_topic("commands/default", Some(&resolver_fallback));
    assert_eq!(topic, "commands/default");

    // Test 3: No resolver → default
    let topic = resolve_inbound_topic("commands/static", None);
    assert_eq!(topic, "commands/static");

    // Test 4: Resolver from environment variable
    std::env::set_var("TEST_INBOUND_TOPIC", "commands/env/sensor");
    let resolver_env = || std::env::var("TEST_INBOUND_TOPIC").ok();
    let topic = resolve_inbound_topic("commands/default", Some(&resolver_env));
    assert_eq!(topic, "commands/env/sensor");
    std::env::remove_var("TEST_INBOUND_TOPIC");
}
