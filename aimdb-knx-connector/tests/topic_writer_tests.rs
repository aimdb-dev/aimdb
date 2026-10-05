//! Integration tests for KNX TopicWriter and TopicResolver functionality
//!
//! These tests verify the dynamic group address routing features:
//! - **TopicWriter**: Outbound (AimDB → KNX) dynamic group address selection per-value
//! - **TopicResolver**: Inbound (KNX → AimDB) late-binding group address resolution at startup
//!
//! A writer writes the bare group address (`1/0/1`), not the `knx://` URL. The
//! longest group address, `31/7/255`, is 8 bytes.
//!
//! The tests use mock data and don't require a running KNX/IP gateway.

#![cfg(feature = "std")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::connector::{TopicBuf, TopicOverflow, TopicWriter};
use aimdb_core::{AimDbBuilder, Producer, RuntimeContext};
use aimdb_knx_connector::KnxConnector;
use aimdb_tokio_adapter::net::{TokioDelay, TokioNet, TokioUdpBinder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::sync::Arc;

/// Topic capacity for every KNX writer here: the longest group address.
const GROUP_ADDRESS_CAPACITY: usize = 8;

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
    #[allow(dead_code)]
    is_on: bool,
}

impl SwitchState {
    fn new(zone_id: &str, is_on: bool) -> Self {
        Self {
            zone_id: zone_id.into(),
            is_on,
        }
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
// TopicWriter Implementations (KNX Group Addresses)
// ============================================================================

/// Dynamic group address writer based on room ID
///
/// Routes dimmer commands to different KNX group addresses based on room.
/// In a real building, each room has its own dimmer actuator address.
struct RoomBasedGroupAddress {
    /// Base group address (main/middle)
    base_main: u8,
    base_middle: u8,
}

impl RoomBasedGroupAddress {
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

impl TopicWriter<DimmerValue> for RoomBasedGroupAddress {
    fn write_topic(
        &self,
        value: &DimmerValue,
        out: &mut TopicBuf<'_>,
    ) -> Result<bool, TopicOverflow> {
        let sub = self.room_to_sub(&value.room_id);
        write!(out, "{}/{}/{}", self.base_main, self.base_middle, sub)?;
        Ok(true)
    }
}

/// HVAC zone-based group address writer
///
/// Routes temperature setpoints to zone-specific group addresses.
struct HvacZone;

impl TopicWriter<TemperatureSetpoint> for HvacZone {
    fn write_topic(
        &self,
        value: &TemperatureSetpoint,
        out: &mut TopicBuf<'_>,
    ) -> Result<bool, TopicOverflow> {
        // HVAC zones mapped to group addresses 5/0/1 through 5/0/16
        if !(1..=16).contains(&value.hvac_zone) {
            return Ok(false); // Invalid zone, use fallback
        }
        write!(out, "5/0/{}", value.hvac_zone)?;
        Ok(true)
    }
}

/// Switch writer with emergency override
///
/// Demonstrates conditional routing: emergency signals go to broadcast address.
struct SwitchWithEmergency;

impl TopicWriter<SwitchState> for SwitchWithEmergency {
    fn write_topic(
        &self,
        value: &SwitchState,
        out: &mut TopicBuf<'_>,
    ) -> Result<bool, TopicOverflow> {
        if value.zone_id == "emergency" {
            // Emergency signals go to broadcast group
            out.push_str("0/0/0")?;
        } else if let Some(zone) = value.zone_id.strip_prefix("zone-") {
            let zone_num: u8 = zone.parse().unwrap_or(0);
            write!(out, "1/1/{zone_num}")?;
        } else {
            return Ok(false); // Use default from link_to()
        }
        Ok(true)
    }
}

/// The group address `writer` selects for `value`, or `default` when it
/// returns `Ok(false)` — what the outbound route resolves.
fn resolve<T>(writer: &dyn TopicWriter<T>, value: &T, default: &str) -> String {
    let mut storage = [0u8; GROUP_ADDRESS_CAPACITY];
    let mut out = TopicBuf::new(&mut storage);
    match writer.write_topic(value, &mut out) {
        Ok(true) => out.as_str().to_string(),
        Ok(false) => default.to_string(),
        Err(TopicOverflow) => panic!("group address exceeds {GROUP_ADDRESS_CAPACITY} bytes"),
    }
}

// ============================================================================
// Unit Tests for KNX TopicWriter
// ============================================================================

#[test]
fn test_room_based_group_address() {
    let writer = RoomBasedGroupAddress::new(1, 0);
    let default = "1/0/0";

    for (room, expected) in [
        ("living", "1/0/1"),
        ("bedroom", "1/0/2"),
        ("kitchen", "1/0/3"),
        // Unknown room uses sub-address 0
        ("garage", "1/0/0"),
    ] {
        let dimmer = DimmerValue::new(room, 128);
        assert_eq!(resolve(&writer, &dimmer, default), expected, "{room}");
    }
}

#[test]
fn test_hvac_zone_routing_and_fallback() {
    let default = "5/0/0"; // Fallback for invalid zones

    for zone in 1..=16u8 {
        let setpoint = TemperatureSetpoint::new(zone, 21.0);
        assert_eq!(
            resolve(&HvacZone, &setpoint, default),
            format!("5/0/{zone}")
        );
    }

    // Invalid zones fall back to the link's default.
    for zone in [0, 17] {
        let setpoint = TemperatureSetpoint::new(zone, 20.0);
        assert_eq!(resolve(&HvacZone, &setpoint, default), default);
    }
}

#[test]
fn test_switch_with_emergency() {
    let default = "1/1/0";

    // Emergency goes to broadcast
    let emergency = SwitchState::new("emergency", true);
    assert_eq!(resolve(&SwitchWithEmergency, &emergency, default), "0/0/0");

    // Normal zones
    let zone1 = SwitchState::new("zone-1", true);
    assert_eq!(resolve(&SwitchWithEmergency, &zone1, default), "1/1/1");

    let zone5 = SwitchState::new("zone-5", false);
    assert_eq!(resolve(&SwitchWithEmergency, &zone5, default), "1/1/5");

    // Unknown zone uses fallback
    let lobby = SwitchState::new("lobby", true);
    assert_eq!(resolve(&SwitchWithEmergency, &lobby, default), default);
}

/// The longest group address fits the capacity; one byte less overflows.
#[test]
fn test_longest_group_address_fits_the_capacity() {
    let writer = RoomBasedGroupAddress::new(31, 7);
    let dimmer = DimmerValue::new("living", 1);

    let mut storage = [0u8; GROUP_ADDRESS_CAPACITY];
    let mut out = TopicBuf::new(&mut storage);
    assert_eq!(writer.write_topic(&dimmer, &mut out), Ok(true));
    assert_eq!(out.as_str(), "31/7/1");

    let writer = |out: &mut TopicBuf<'_>| write!(out, "31/7/255");
    let mut storage = [0u8; GROUP_ADDRESS_CAPACITY];
    assert!(writer(&mut TopicBuf::new(&mut storage)).is_ok());
    let mut storage = [0u8; GROUP_ADDRESS_CAPACITY - 1];
    assert!(writer(&mut TopicBuf::new(&mut storage)).is_err());
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

// ============================================================================
// Integration Tests: registration with KnxConnector
// ============================================================================

fn connector() -> KnxConnector<TokioUdpBinder, TokioDelay> {
    KnxConnector::new(
        TokioNet::udp(Ipv4Addr::UNSPECIFIED),
        TokioDelay,
        "knx://192.168.1.10:3671",
    )
}

/// `link_to` + `with_topic_writer` builds with the connector registered.
/// Running it would need a KNX gateway.
#[tokio::test]
async fn test_knx_topic_writer_with_connector_registration() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(connector());

    builder.configure::<DimmerValue>("knx.dimmer.living", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(
                |_ctx: RuntimeContext, producer: Producer<DimmerValue>| async move {
                    producer.produce(DimmerValue::new("living", 200));
                },
            )
            .link_to("knx://1/0/0") // Fallback group address
            .with_topic_writer(GROUP_ADDRESS_CAPACITY, RoomBasedGroupAddress::new(1, 0))
            .with_serializer(|_ctx, dimmer: &DimmerValue| Ok(dimmer.to_knx_bytes()))
            .finish();
    });
    builder.configure::<TemperatureSetpoint>("knx.hvac.setpoint", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_to("knx://5/0/0") // Fallback for invalid zones
            .with_topic_writer(GROUP_ADDRESS_CAPACITY, HvacZone)
            .with_serializer(|_ctx, sp: &TemperatureSetpoint| Ok(sp.to_knx_bytes()))
            .finish();
    });

    assert!(builder.build().await.is_ok());
}

/// The connector pulls through `OutboundRoutes`, which rejects
/// `with_topic_provider` links at build.
#[tokio::test]
async fn test_knx_rejects_a_topic_provider() {
    use aimdb_core::connector::TopicProvider;

    struct Fixed;
    impl TopicProvider<DimmerValue> for Fixed {
        fn topic(&self, _value: &DimmerValue) -> Option<String> {
            Some("1/0/1".into())
        }
    }

    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(connector());
    builder.configure::<DimmerValue>("knx.dimmer.living", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_to("knx://1/0/0")
            .with_topic_provider(Fixed)
            .with_serializer(|_ctx, dimmer: &DimmerValue| Ok(dimmer.to_knx_bytes()))
            .finish();
    });

    let Err(err) = builder.build().await else {
        panic!("a topic provider must be rejected");
    };
    assert!(
        format!("{err}").contains("with_topic_provider"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_knx_topic_resolver_with_connector_registration() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());

    std::env::set_var("KNX_SWITCH_INPUT", "1/2/10");

    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(connector());

    // Register switch with dynamic group address resolver
    builder.configure::<SwitchState>("knx.switch.zone1", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("knx://1/2/0") // Fallback group address
            .with_topic_resolver(|| {
                std::env::var("KNX_SWITCH_INPUT")
                    .ok()
                    .map(|addr| format!("knx://{}", addr))
            })
            .with_deserializer(|_ctx, data: &[u8]| SwitchState::from_knx_bytes(data))
            .finish();
    });

    assert!(builder.build().await.is_ok());

    std::env::remove_var("KNX_SWITCH_INPUT");
}
