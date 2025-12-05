//! KNX Connector Demo - Bidirectional Bus Monitor
//!
//! Demonstrates bidirectional KNX integration with AimDB:
//! - Inbound: Monitor KNX bus telegrams → process in AimDB
//! - Outbound: Control KNX devices from AimDB (toggle light on key press)
//! - Real-time logging of light switches and temperature sensors
//! - **Multi-source pattern**: Separate record types for each KNX address with shared logic
//!
//! ## Multi-Source Temperature Monitoring
//!
//! This example demonstrates the recommended pattern for monitoring multiple KNX
//! addresses of the same data type (e.g., temperature sensors in different rooms):
//!
//! 1. **Shared base type**: `TemperatureReading` holds the common data
//! 2. **Trait for sources**: `TemperatureSource` defines the contract
//! 3. **Newtype per source**: `LivingRoomTemp`, `BedroomTemp` wrap the base
//! 4. **Generic handler**: One `temperature_monitor<T>` works for all sources
//!
//! This approach provides:
//! - Type-safe queries: `db.get::<LivingRoomTemp>()` vs runtime key lookup
//! - Compile-time source validation
//! - Zero runtime overhead (newtypes are zero-cost)
//! - No complex indexed buffer machinery
//!
//! ## Running
//!
//! Prerequisites:
//! - KNX/IP gateway on your network (e.g., SCN-IP000.03, Gira X1, ABB IP Interface)
//! - KNX devices configured with group addresses
//!
//! Run the demo:
//! ```bash
//! cargo run --example tokio-knx-connector-demo --features tokio-runtime,tracing
//! ```
//!
//! ## Configuration
//!
//! Update the gateway URL and group addresses in main() to match your setup:
//! - Gateway: "knx://192.168.1.19:3671"
//! - Light switch (monitor): "knx://1/0/7"
//! - Light control (publish): "knx://1/0/6"
//! - Temperature sensors: "knx://9/1/0" (Living Room), "knx://9/1/1" (Bedroom)

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult, Producer, RuntimeContext};
use aimdb_knx_connector::dpt::{Dpt1, Dpt9, DptDecode, DptEncode};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LightState {
    group_address: String,
    is_on: bool,
    timestamp: u64,
}

// ============================================================================
// MULTI-SOURCE TEMPERATURE PATTERN
// ============================================================================
//
// This demonstrates the recommended approach for handling multiple KNX addresses
// of the same data type. Instead of a complex indexed buffer, we use:
//
// 1. A shared base struct (TemperatureReading)
// 2. A trait defining the source contract (TemperatureSource)
// 3. Zero-cost newtype wrappers per source (LivingRoomTemp, BedroomTemp)
// 4. A generic handler that works with any source
//
// Benefits:
// - Type-safe: db.get::<LivingRoomTemp>() vs runtime key lookup
// - Zero overhead: newtypes compile away
// - Compile-time validation: can't accidentally mix up sources
// - Simple: no indexed buffer complexity
// ============================================================================

/// Shared temperature reading data (the actual payload)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemperatureReading {
    pub celsius: f32,
    pub timestamp: u64,
}

/// Trait for temperature sources - enables generic handlers
///
/// Each temperature source implements this trait, providing:
/// - GROUP_ADDRESS: The KNX group address as a compile-time constant
/// - LOCATION: Human-readable location name for logging
/// - Access to the underlying TemperatureReading
pub trait TemperatureSource: Clone + Send + Sync + std::fmt::Debug + 'static {
    /// KNX group address for this sensor (e.g., "9/1/0")
    const GROUP_ADDRESS: &'static str;
    /// Human-readable location name (e.g., "Living Room")
    const LOCATION: &'static str;

    /// Create from a temperature reading
    fn from_reading(reading: TemperatureReading) -> Self;
    /// Get reference to the underlying reading
    fn reading(&self) -> &TemperatureReading;
}

// ----------------------------------------------------------------------------
// Per-Source Newtypes (zero-cost wrappers)
// ----------------------------------------------------------------------------

/// Living Room temperature sensor (KNX 9/1/0)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LivingRoomTemp(pub TemperatureReading);

impl TemperatureSource for LivingRoomTemp {
    const GROUP_ADDRESS: &'static str = "9/1/0";
    const LOCATION: &'static str = "Living Room";

    fn from_reading(reading: TemperatureReading) -> Self {
        Self(reading)
    }
    fn reading(&self) -> &TemperatureReading {
        &self.0
    }
}

/// Bedroom temperature sensor (KNX 9/1/1)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BedroomTemp(pub TemperatureReading);

impl TemperatureSource for BedroomTemp {
    const GROUP_ADDRESS: &'static str = "9/1/1";
    const LOCATION: &'static str = "Bedroom";

    fn from_reading(reading: TemperatureReading) -> Self {
        Self(reading)
    }
    fn reading(&self) -> &TemperatureReading {
        &self.0
    }
}

// ============================================================================
// END MULTI-SOURCE PATTERN
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LightControl {
    group_address: String,
    is_on: bool,
    timestamp: u64,
}

/// Input handler that toggles light on key press
async fn input_handler(
    ctx: RuntimeContext<TokioAdapter>,
    producer: Producer<LightControl, TokioAdapter>,
) {
    let log = ctx.log();
    log.info("\n⌨️  Input handler started. Press ENTER to toggle light on 1/0/6");
    log.info("   (This sends GroupValueWrite to the KNX bus)\n");

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    let mut light_on = false;

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                // Toggle light state
                light_on = !light_on;

                let state = LightControl {
                    group_address: "1/0/6".to_string(),
                    is_on: light_on,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                };

                match producer.produce(state).await {
                    Ok(_) => {
                        log.info(&format!(
                            "✅ Published to KNX: 1/0/6 = {} (sent to bus)",
                            if light_on { "ON ✨" } else { "OFF" }
                        ));
                    }
                    Err(e) => {
                        log.error(&format!("❌ Failed to publish: {:?}", e));
                    }
                }
            }
            Err(e) => {
                log.error(&format!("Error reading input: {}", e));
                break;
            }
        }
    }
}

/// Consumer that logs incoming KNX light telegrams
async fn light_monitor(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: aimdb_core::Consumer<LightState, TokioAdapter>,
) {
    let log = ctx.log();

    log.info("👀 Light monitor started - watching KNX bus...\n");

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to light buffer");
        return;
    };

    while let Ok(state) = reader.recv().await {
        log.info(&format!(
            "🔵 KNX telegram received: {} = {}",
            state.group_address,
            if state.is_on { "ON ✨" } else { "OFF" }
        ));
    }
}

/// Generic temperature monitor - works with ANY TemperatureSource implementation
///
/// This single function handles all temperature sensors by using the trait's
/// associated constants for location-specific logging. The compiler generates
/// specialized versions for each concrete type (monomorphization).
async fn temperature_monitor<T: TemperatureSource>(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: aimdb_core::Consumer<T, TokioAdapter>,
) {
    let log = ctx.log();

    log.info(&format!(
        "🌡️  {} monitor started ({})\n",
        T::LOCATION,
        T::GROUP_ADDRESS
    ));

    let Ok(mut reader) = consumer.subscribe() else {
        log.error(&format!(
            "Failed to subscribe to {} temperature buffer",
            T::LOCATION
        ));
        return;
    };

    while let Ok(temp) = reader.recv().await {
        let reading = temp.reading();
        log.info(&format!("🌡️  {}: {:.1}°C", T::LOCATION, reading.celsius));
    }
}

#[tokio::main]
async fn main() -> DbResult<()> {
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt::init();

    let runtime = Arc::new(TokioAdapter::new()?);

    println!("🔧 Creating database with KNX bus monitor...");
    println!("⚠️  NOTE: Update gateway URL and group addresses to match your setup!\n");

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_knx_connector::KnxConnector::new("knx://192.168.1.19:3671"),
    );

    // Configure LightState record (inbound monitoring only)
    builder.configure::<LightState>("lights.state", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(light_monitor)
            // Subscribe from KNX group address 1/0/7 (inbound)
            .link_from("knx://1/0/7")
            .with_deserializer(|data: &[u8]| {
                // Use DPT 1.001 (Switch) to decode boolean value
                let is_on = Dpt1::Switch.decode(data).unwrap_or(false);

                Ok(LightState {
                    group_address: "1/0/7".to_string(),
                    is_on,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                })
            })
            .finish();
    });

    // ========================================================================
    // MULTI-SOURCE TEMPERATURE CONFIGURATION
    // ========================================================================
    //
    // Each temperature source is registered as a separate record type.
    // The generic temperature_monitor<T> handler works with all of them.
    // This gives us type-safe queries and compile-time source validation.
    // ========================================================================

    // Living Room temperature sensor (9/1/0)
    builder.configure::<LivingRoomTemp>("temp.livingroom", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(temperature_monitor::<LivingRoomTemp>)
            .link_from(&format!("knx://{}", LivingRoomTemp::GROUP_ADDRESS))
            .with_deserializer(|data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(LivingRoomTemp::from_reading(TemperatureReading {
                    celsius,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                }))
            })
            .finish();
    });

    // Bedroom temperature sensor (9/1/1)
    builder.configure::<BedroomTemp>("temp.bedroom", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .tap(temperature_monitor::<BedroomTemp>)
            .link_from(&format!("knx://{}", BedroomTemp::GROUP_ADDRESS))
            .with_deserializer(|data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(BedroomTemp::from_reading(TemperatureReading {
                    celsius,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                }))
            })
            .finish();
    });

    // ========================================================================
    // END MULTI-SOURCE TEMPERATURE CONFIGURATION
    // ========================================================================

    // Configure LightControl record (outbound - control KNX device)
    builder.configure::<LightControl>("lights.control", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(input_handler)
            // Publish to KNX group address 1/0/6 (outbound)
            .link_to("knx://1/0/6")
            .with_serializer(|state: &LightControl| {
                // Use DPT 1.001 (Switch) to encode boolean value
                let mut buf = [0u8; 1];
                let len = Dpt1::Switch.encode(state.is_on, &mut buf).unwrap_or(0);
                Ok(buf[..len].to_vec())
            })
            .finish();
    });

    println!("✅ Database configured with bidirectional KNX integration:");
    println!("   INBOUND (KNX → AimDB):");
    println!("     - knx://1/0/7 (light monitoring, DPT 1.001)");
    println!("     - knx://9/1/0 (Living Room temperature, DPT 9.001)");
    println!("     - knx://9/1/1 (Bedroom temperature, DPT 9.001)");
    println!("   OUTBOUND (AimDB → KNX):");
    println!("     - knx://1/0/6 (light control, DPT 1.001)");
    println!("   Gateway: 192.168.1.19:3671");
    println!();
    println!("💡 Multi-source pattern demo:");
    println!("   - LivingRoomTemp and BedroomTemp are separate record types");
    println!("   - Both use the same generic temperature_monitor<T> handler");
    println!("   - Type-safe queries: db.get::<LivingRoomTemp>()");
    println!();
    println!("💡 The demo will:");
    println!("   1. Connect to the KNX/IP gateway");
    println!("   2. Monitor KNX bus for telegrams on configured addresses");
    println!("   3. Control light on 1/0/6 when you press ENTER");
    println!("   4. Log all KNX activity in real-time");
    println!("\n   Trigger events by:");
    println!("   - Pressing ENTER to toggle light (1/0/6)");
    println!("   - Pressing physical KNX switches");
    println!("   - Sending telegrams via ETS");
    println!("\n   Press Ctrl+C to stop.\n");

    builder.run().await
}
