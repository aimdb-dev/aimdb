//! Integration tests for KNX TopicProvider and TopicResolver functionality
//!
//! These tests verify the dynamic group address routing features:
//! - **TopicProvider**: Outbound (AimDB → KNX) dynamic group address selection per-value
//! - **TopicResolver**: Inbound (KNX → AimDB) late-binding group address resolution at startup
//!
//! The tests use mock data and don't require a running KNX/IP gateway.

#![cfg(feature = "tokio-runtime")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::TopicProvider;
use aimdb_core::{AimDbBuilder, Producer, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// ============================================================================
// Test Types
// ============================================================================

/// Dimmer value with room identifier for dynamic group address routing
#[derive(Clone, Debug)]
struct DimmerValue {
    room_id: String,
    level: u8, // 0-255
}

impl DimmerValue {
    fn new(room_id: &str, level: u8) -> Self {
        Self {
            room_id: room_id.into(),
            level,
        }
    }

    fn to_knx_bytes(&self) -> Vec<u8> {
        vec![self.level]
    }
}

/// Switch state with zone identifier
#[derive(Clone, Debug)]
struct SwitchState {
    zone_id: String,
    is_on: bool,
}

impl SwitchState {
    fn new(zone_id: &str, is_on: bool) -> Self {
        Self {
            zone_id: zone_id.into(),
            is_on,
        }
    }

    #[allow(dead_code)]
    fn to_knx_bytes(&self) -> Vec<u8> {
        vec![if self.is_on { 1 } else { 0 }]
    }

    fn from_knx_bytes(data: &[u8]) -> Result<Self, String> {
        if data.is_empty() {
            return Err("Empty data".into());
        }
        Ok(SwitchState {
            zone_id: "unknown".into(),
            is_on: data[0] != 0,
        })
    }
}

/// Temperature setpoint for HVAC control
#[derive(Clone, Debug)]
struct TemperatureSetpoint {
    hvac_zone: u8, // Zone number 1-16
    setpoint: f32, // Temperature in Celsius
}

impl TemperatureSetpoint {
    fn new(hvac_zone: u8, setpoint: f32) -> Self {
        Self {
            hvac_zone,
            setpoint,
        }
    }

    fn to_knx_bytes(&self) -> Vec<u8> {
        // KNX DPT 9.001 encoding (simplified for test)
        let value = (self.setpoint * 100.0) as i16;
        vec![(value >> 8) as u8, value as u8]
    }
}

// ============================================================================
// TopicProvider Implementations (KNX Group Addresses)
// ============================================================================

/// Dynamic group address provider based on room ID
///
/// Routes dimmer commands to different KNX group addresses based on room.
/// In a real building, each room has its own dimmer actuator address.
struct RoomBasedGroupAddressProvider {
    /// Base group address (main/middle)
    base_main: u8,
    base_middle: u8,
}

impl RoomBasedGroupAddressProvider {
    fn new(base_main: u8, base_middle: u8) -> Self {
        Self {
            base_main,
            base_middle,
        }
    }

    fn room_to_sub(&self, room_id: &str) -> u8 {
        // Map room names to sub-addresses
        match room_id {
            "living" => 1,
            "bedroom" => 2,
            "kitchen" => 3,
            "bathroom" => 4,
            "office" => 5,
            _ => 0, // Default/unknown
        }
    }
}

impl TopicProvider<DimmerValue> for RoomBasedGroupAddressProvider {
    fn topic(&self, value: &DimmerValue) -> Option<String> {
        let sub = self.room_to_sub(&value.room_id);
        Some(format!(
            "knx://{}/{}/{}",
            self.base_main, self.base_middle, sub
        ))
    }
}

/// HVAC zone-based group address provider
///
/// Routes temperature setpoints to zone-specific group addresses.
struct HvacZoneProvider;

impl TopicProvider<TemperatureSetpoint> for HvacZoneProvider {
    fn topic(&self, value: &TemperatureSetpoint) -> Option<String> {
        // HVAC zones mapped to group addresses 5/0/1 through 5/0/16
        if value.hvac_zone >= 1 && value.hvac_zone <= 16 {
            Some(format!("knx://5/0/{}", value.hvac_zone))
        } else {
            None // Invalid zone, use fallback
        }
    }
}

/// Switch provider with emergency override
///
/// Demonstrates conditional routing: emergency signals go to broadcast address.
struct SwitchWithEmergencyProvider;

impl TopicProvider<SwitchState> for SwitchWithEmergencyProvider {
    fn topic(&self, value: &SwitchState) -> Option<String> {
        if value.zone_id == "emergency" {
            // Emergency signals go to broadcast group
            Some("knx://0/0/0".into())
        } else if value.zone_id.starts_with("zone-") {
            // Parse zone number
            let zone_num: u8 = value.zone_id[5..].parse().unwrap_or(0);
            Some(format!("knx://1/1/{}", zone_num))
        } else {
            None // Use default from link_to()
        }
    }
}

// ============================================================================
// Unit Tests for KNX TopicProvider
// ============================================================================

#[test]
fn test_room_based_group_address_provider() {
    let provider = RoomBasedGroupAddressProvider::new(1, 0);

    let living = DimmerValue::new("living", 128);
    assert_eq!(provider.topic(&living), Some("knx://1/0/1".into()));

    let bedroom = DimmerValue::new("bedroom", 255);
    assert_eq!(provider.topic(&bedroom), Some("knx://1/0/2".into()));

    let kitchen = DimmerValue::new("kitchen", 64);
    assert_eq!(provider.topic(&kitchen), Some("knx://1/0/3".into()));

    // Unknown room uses sub-address 0
    let unknown = DimmerValue::new("garage", 100);
    assert_eq!(provider.topic(&unknown), Some("knx://1/0/0".into()));
}

#[test]
fn test_hvac_zone_provider() {
    let provider = HvacZoneProvider;

    let zone1 = TemperatureSetpoint::new(1, 21.0);
    assert_eq!(provider.topic(&zone1), Some("knx://5/0/1".into()));

    let zone16 = TemperatureSetpoint::new(16, 22.5);
    assert_eq!(provider.topic(&zone16), Some("knx://5/0/16".into()));

    // Invalid zone returns None (fallback)
    let invalid = TemperatureSetpoint::new(0, 20.0);
    assert_eq!(provider.topic(&invalid), None);

    let too_high = TemperatureSetpoint::new(17, 20.0);
    assert_eq!(provider.topic(&too_high), None);
}

#[test]
fn test_switch_with_emergency_provider() {
    let provider = SwitchWithEmergencyProvider;

    // Emergency goes to broadcast
    let emergency = SwitchState::new("emergency", true);
    assert_eq!(provider.topic(&emergency), Some("knx://0/0/0".into()));

    // Normal zones
    let zone1 = SwitchState::new("zone-1", true);
    assert_eq!(provider.topic(&zone1), Some("knx://1/1/1".into()));

    let zone5 = SwitchState::new("zone-5", false);
    assert_eq!(provider.topic(&zone5), Some("knx://1/1/5".into()));

    // Unknown zone uses fallback
    let unknown = SwitchState::new("lobby", true);
    assert_eq!(provider.topic(&unknown), None);
}

// ============================================================================
// Unit Tests for KNX TopicResolver
// ============================================================================

#[test]
fn test_topic_resolver_from_config_file() {
    // Simulate reading group address from a config file or service
    std::env::set_var("KNX_SWITCH_GROUP", "1/2/3");

    let resolver = || {
        std::env::var("KNX_SWITCH_GROUP")
            .ok()
            .map(|addr| format!("knx://{}", addr))
    };

    assert_eq!(resolver(), Some("knx://1/2/3".into()));

    std::env::remove_var("KNX_SWITCH_GROUP");
}

#[test]
fn test_topic_resolver_with_discovery() {
    use std::sync::Mutex;

    // Simulate KNX device discovery
    let discovered_devices = Arc::new(Mutex::new(vec![
        ("dimmer-1", "1/0/1"),
        ("dimmer-2", "1/0/2"),
    ]));

    let devices_clone = discovered_devices.clone();
    let resolver = move || {
        devices_clone
            .lock()
            .unwrap()
            .iter()
            .find(|(name, _)| *name == "dimmer-1")
            .map(|(_, addr)| format!("knx://{}", addr))
    };

    assert_eq!(resolver(), Some("knx://1/0/1".into()));
}

// ============================================================================
// Integration Test: TopicProvider with AimDbBuilder
// ============================================================================

/// Test that TopicProvider can be configured without connector (verifies API)
#[tokio::test]
async fn test_knx_topic_provider_registration_api() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let produced_count = Arc::new(AtomicU32::new(0));
    let produced_count_clone = produced_count.clone();

    let mut builder = AimDbBuilder::new().runtime(runtime);

    // Register without link_to (no connector needed)
    builder.configure::<DimmerValue>("knx.dimmer.living", |reg| {
        let counter = produced_count_clone.clone();
        reg.buffer(BufferCfg::SingleLatest).source(
            move |_ctx: RuntimeContext<TokioAdapter>,
                  producer: Producer<DimmerValue, TokioAdapter>| {
                let counter = counter.clone();
                async move {
                    let dimmer = DimmerValue::new("living", 200);
                    producer.produce(dimmer).await.ok();
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            },
        );
    });

    // Build should succeed
    assert!(builder.build().await.is_ok());
}

/// Test TopicProvider with KnxConnector registration
///
/// This test verifies the full configuration API including link_to + with_topic_provider
/// works correctly at compile time. Runtime requires actual KNX gateway.
#[tokio::test]
async fn test_knx_topic_provider_with_connector_registration() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_knx_connector::KnxConnector::new("knx://192.168.1.10:3671"),
    );

    // Register dimmer with dynamic group address provider
    builder.configure::<DimmerValue>("knx.dimmer.living", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(
                |_ctx: RuntimeContext<TokioAdapter>,
                 producer: Producer<DimmerValue, TokioAdapter>| async move {
                    let dimmer = DimmerValue::new("living", 200);
                    producer.produce(dimmer).await.ok();
                },
            )
            .link_to("knx://1/0/0") // Fallback group address
            .with_topic_provider(RoomBasedGroupAddressProvider::new(1, 0))
            .with_serializer(|dimmer: &DimmerValue| Ok(dimmer.to_knx_bytes()))
            .finish();
    });

    // Build succeeds with connector registered
    assert!(builder.build().await.is_ok());
}

#[tokio::test]
async fn test_knx_topic_resolver_with_connector_registration() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    std::env::set_var("KNX_SWITCH_INPUT", "1/2/10");

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_knx_connector::KnxConnector::new("knx://192.168.1.10:3671"),
    );

    // Register switch with dynamic group address resolver
    builder.configure::<SwitchState>("knx.switch.zone1", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("knx://1/2/0") // Fallback group address
            .with_topic_resolver(|| {
                std::env::var("KNX_SWITCH_INPUT")
                    .ok()
                    .map(|addr| format!("knx://{}", addr))
            })
            .with_deserializer(|data: &[u8]| SwitchState::from_knx_bytes(data))
            .finish();
    });

    // Build should succeed
    assert!(builder.build().await.is_ok());

    std::env::remove_var("KNX_SWITCH_INPUT");
}

#[tokio::test]
async fn test_hvac_zone_routing() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_knx_connector::KnxConnector::new("knx://192.168.1.10:3671"),
    );

    // HVAC setpoint with zone-based routing
    builder.configure::<TemperatureSetpoint>("knx.hvac.setpoint", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(
                |_ctx: RuntimeContext<TokioAdapter>,
                 producer: Producer<TemperatureSetpoint, TokioAdapter>| async move {
                    // Different zones get routed to different group addresses
                    for zone in 1..=4 {
                        let setpoint = TemperatureSetpoint::new(zone, 21.0 + zone as f32 * 0.5);
                        producer.produce(setpoint).await.ok();
                    }
                },
            )
            .link_to("knx://5/0/0") // Fallback for invalid zones
            .with_topic_provider(HvacZoneProvider)
            .with_serializer(|sp: &TemperatureSetpoint| Ok(sp.to_knx_bytes()))
            .finish();
    });

    assert!(builder.build().await.is_ok());
}

// ============================================================================
// Test TopicProviderAny Type Erasure with KNX Types
// ============================================================================

#[test]
fn test_knx_topic_provider_type_erasure() {
    use aimdb_core::connector::{TopicProviderAny, TopicProviderWrapper};
    use std::any::Any;

    let provider = TopicProviderWrapper::new(RoomBasedGroupAddressProvider::new(1, 0));

    // Correct type
    let dimmer = DimmerValue::new("living", 128);
    let dimmer_any: &dyn Any = &dimmer;
    assert_eq!(provider.topic_any(dimmer_any), Some("knx://1/0/1".into()));

    // Wrong type: should return None
    let switch = SwitchState::new("zone-1", true);
    let switch_any: &dyn Any = &switch;
    assert_eq!(provider.topic_any(switch_any), None);
}

// ============================================================================
// Test: Simulate Connector Group Address Resolution Logic
// ============================================================================
//
// These tests simulate EXACTLY what the KNX connector does internally:
// ```rust
// let group_addr = topic_provider
//     .as_ref()
//     .and_then(|provider| provider.topic_any(&*value_any))
//     .unwrap_or_else(|| default_group_addr.clone());
// ```

/// Simulates the connector's group address resolution for outbound telegrams
fn resolve_group_address_like_connector(
    default_group_addr: &str,
    topic_provider: Option<&dyn aimdb_core::connector::TopicProviderAny>,
    value: &dyn std::any::Any,
) -> String {
    topic_provider
        .and_then(|provider| provider.topic_any(value))
        .unwrap_or_else(|| default_group_addr.to_string())
}

#[test]
fn test_connector_group_address_resolution_room_based() {
    use aimdb_core::connector::TopicProviderWrapper;

    let provider = TopicProviderWrapper::new(RoomBasedGroupAddressProvider::new(1, 0));
    let default_addr = "knx://1/0/0";

    // Test 1: Living room dimmer
    let dimmer_living = DimmerValue::new("living", 128);
    let resolved =
        resolve_group_address_like_connector(default_addr, Some(&provider), &dimmer_living);
    assert_eq!(resolved, "knx://1/0/1");

    // Test 2: Bedroom dimmer
    let dimmer_bedroom = DimmerValue::new("bedroom", 255);
    let resolved =
        resolve_group_address_like_connector(default_addr, Some(&provider), &dimmer_bedroom);
    assert_eq!(resolved, "knx://1/0/2");

    // Test 3: Kitchen dimmer
    let dimmer_kitchen = DimmerValue::new("kitchen", 64);
    let resolved =
        resolve_group_address_like_connector(default_addr, Some(&provider), &dimmer_kitchen);
    assert_eq!(resolved, "knx://1/0/3");

    // Test 4: Unknown room → sub-address 0
    let dimmer_unknown = DimmerValue::new("garage", 100);
    let resolved =
        resolve_group_address_like_connector(default_addr, Some(&provider), &dimmer_unknown);
    assert_eq!(resolved, "knx://1/0/0");
}

#[test]
fn test_connector_group_address_resolution_hvac_zones() {
    use aimdb_core::connector::TopicProviderWrapper;

    let provider = TopicProviderWrapper::new(HvacZoneProvider);
    let default_addr = "knx://5/0/0"; // Fallback for invalid zones

    // Test valid zones 1-16
    for zone in 1..=16u8 {
        let setpoint = TemperatureSetpoint::new(zone, 21.0);
        let resolved =
            resolve_group_address_like_connector(default_addr, Some(&provider), &setpoint);
        assert_eq!(resolved, format!("knx://5/0/{}", zone));
    }

    // Test invalid zone 0 → fallback
    let invalid_zone_0 = TemperatureSetpoint::new(0, 21.0);
    let resolved =
        resolve_group_address_like_connector(default_addr, Some(&provider), &invalid_zone_0);
    assert_eq!(resolved, "knx://5/0/0"); // Fallback

    // Test invalid zone 17 → fallback
    let invalid_zone_17 = TemperatureSetpoint::new(17, 21.0);
    let resolved =
        resolve_group_address_like_connector(default_addr, Some(&provider), &invalid_zone_17);
    assert_eq!(resolved, "knx://5/0/0"); // Fallback
}

#[test]
fn test_connector_group_address_resolution_emergency_switch() {
    use aimdb_core::connector::TopicProviderWrapper;

    let provider = TopicProviderWrapper::new(SwitchWithEmergencyProvider);
    let default_addr = "knx://1/1/0";

    // Test 1: Emergency switch → broadcast address
    let emergency = SwitchState::new("emergency", true);
    let resolved = resolve_group_address_like_connector(default_addr, Some(&provider), &emergency);
    assert_eq!(resolved, "knx://0/0/0"); // Broadcast!

    // Test 2: Normal zone switches
    let zone_1 = SwitchState::new("zone-1", true);
    let resolved = resolve_group_address_like_connector(default_addr, Some(&provider), &zone_1);
    assert_eq!(resolved, "knx://1/1/1");

    let zone_5 = SwitchState::new("zone-5", false);
    let resolved = resolve_group_address_like_connector(default_addr, Some(&provider), &zone_5);
    assert_eq!(resolved, "knx://1/1/5");

    // Test 3: Unknown zone → fallback
    let lobby = SwitchState::new("lobby", true);
    let resolved = resolve_group_address_like_connector(default_addr, Some(&provider), &lobby);
    assert_eq!(resolved, "knx://1/1/0"); // Fallback
}

#[test]
fn test_connector_group_address_no_provider() {
    let default_addr = "knx://1/0/99";

    // No provider → always use default
    let dimmer = DimmerValue::new("living", 128);
    let resolved = resolve_group_address_like_connector(default_addr, None, &dimmer);
    assert_eq!(resolved, "knx://1/0/99");
}

/// Test inbound group address resolution (TopicResolver)
#[test]
fn test_inbound_group_address_resolver_simulation() {
    fn resolve_inbound_group_addr(
        default_addr: &str,
        resolver: Option<&dyn Fn() -> Option<String>>,
    ) -> String {
        resolver
            .and_then(|r| r())
            .unwrap_or_else(|| default_addr.to_string())
    }

    // Test 1: Resolver returns dynamic address
    let resolver_dynamic = || Some("knx://1/2/10".to_string());
    let addr = resolve_inbound_group_addr("knx://1/2/0", Some(&resolver_dynamic));
    assert_eq!(addr, "knx://1/2/10");

    // Test 2: Resolver returns None → fallback
    let resolver_fallback = || None;
    let addr = resolve_inbound_group_addr("knx://1/2/0", Some(&resolver_fallback));
    assert_eq!(addr, "knx://1/2/0");

    // Test 3: No resolver → default
    let addr = resolve_inbound_group_addr("knx://1/2/0", None);
    assert_eq!(addr, "knx://1/2/0");

    // Test 4: Resolver from environment (simulating config file)
    std::env::set_var("KNX_INBOUND_ADDR", "1/3/5");
    let resolver_env = || {
        std::env::var("KNX_INBOUND_ADDR")
            .ok()
            .map(|a| format!("knx://{}", a))
    };
    let addr = resolve_inbound_group_addr("knx://1/0/0", Some(&resolver_env));
    assert_eq!(addr, "knx://1/3/5");
    std::env::remove_var("KNX_INBOUND_ADDR");
}
