#![no_std]
#![no_main]

//! MQTT Connector Demo for Embassy Runtime
//!
//! Demonstrates bidirectional MQTT integration with automatic publishing and subscription on embedded hardware.
//!
//! ## Hardware Requirements
//!
//! - STM32H563ZI Nucleo board (or similar with Ethernet)
//! - Ethernet connection to network with MQTT broker
//!
//! ## Running
//!
//! 1. Start an MQTT broker on your network:
//! ```bash
//! docker run -d -p 1883:1883 eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf
//! ```
//!
//! 2. Subscribe to sensor data:
//! ```bash
//! mosquitto_sub -h <broker-ip> -t 'sensors/#' -v
//! ```
//!
//! 3. Send commands to device:
//! ```bash
//! mosquitto_pub -h <broker-ip> -t 'commands/temperature' -m '{"action":"read","sensor_id":"sensor-001"}'
//! ```
//!
//! 4. Update MQTT_BROKER_IP constant below to match your broker
//!
//! 5. Flash to target:
//! ```bash
//! cargo run --example embassy-mqtt-connector-demo --features embassy-runtime,tracing
//! ```

extern crate alloc;

use aimdb_core::{AimDbBuilder, Consumer, Producer, RuntimeContext};
use aimdb_embassy_adapter::{
    EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExt, EmbassyRecordRegistrarExtCustom,
};
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue};
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::peripherals::ETH;
use embassy_stm32::rng::Rng;
use embassy_stm32::{Config, bind_interrupts, eth, peripherals, rng};
use embassy_time::{Duration, Timer};
use heapless::String as HeaplessString;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// Interrupt bindings for Ethernet and RNG
bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
});

type Device = Ethernet<'static, ETH, GenericPhy>;

/// Network task that runs the embassy-net stack
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
    runner.run().await
}

// ============================================================================
// SENSOR DATA TYPES
// ============================================================================
//

/// Temperature sensor reading (no_std compatible)
#[derive(Clone, Debug)]
struct Temperature {
    sensor_id: &'static str,
    celsius: f32,
    timestamp: u32,
}

impl Temperature {
    /// Serialize to JSON format using heapless string
    /// Returns a byte slice ready for MQTT publishing
    fn to_json(&self) -> HeaplessString<128> {
        use core::fmt::Write;
        let mut json = HeaplessString::<128>::new();
        // Manual JSON formatting for no_std
        // Note: Simple integer representation of celsius * 10 for no_std compatibility
        let celsius_int = (self.celsius * 10.0) as i32;
        let _ = core::write!(
            &mut json,
            r#"{{"sensor_id":"{}","celsius_x10":{},"timestamp":{}}}"#,
            self.sensor_id,
            celsius_int,
            self.timestamp
        );
        json
    }

    /// Convert heapless string to Vec for connector compatibility
    fn to_json_vec(&self) -> alloc::vec::Vec<u8> {
        self.to_json().as_bytes().to_vec()
    }

    /// Serialize to CSV format for no_std
    fn to_csv(&self) -> alloc::vec::Vec<u8> {
        use alloc::format;
        let csv = format!("{},{},{}", self.sensor_id, self.celsius, self.timestamp);
        csv.into_bytes()
    }
}

/// Command for controlling temperature sensor (inbound from MQTT)
#[derive(Clone, Debug)]
struct TemperatureCommand {
    action: HeaplessString<32>,
    sensor_id: HeaplessString<64>,
}

impl TemperatureCommand {
    /// Simple JSON parser for no_std
    /// Expected format: {"action":"read","sensor_id":"sensor-001"}
    fn from_json(data: &[u8]) -> Result<Self, alloc::string::String> {
        use alloc::string::ToString;

        let text = core::str::from_utf8(data).map_err(|_| "Invalid UTF-8".to_string())?;

        // Simple JSON parsing for {"action":"xxx","sensor_id":"yyy"}
        let mut action = HeaplessString::<32>::new();
        let mut sensor_id = HeaplessString::<64>::new();

        for pair in text.trim_matches(|c| c == '{' || c == '}').split(',') {
            let parts: alloc::vec::Vec<&str> = pair.split(':').collect();
            if parts.len() != 2 {
                continue;
            }

            let key = parts[0].trim().trim_matches('"');
            let value = parts[1].trim().trim_matches('"');

            match key {
                "action" => {
                    action
                        .push_str(value)
                        .map_err(|_| "Action too long".to_string())?;
                }
                "sensor_id" => {
                    sensor_id
                        .push_str(value)
                        .map_err(|_| "Sensor ID too long".to_string())?;
                }
                _ => {}
            }
        }

        if action.is_empty() || sensor_id.is_empty() {
            return Err("Missing required fields".to_string());
        }

        Ok(TemperatureCommand { action, sensor_id })
    }
}

/// Simulates a temperature sensor generating readings
async fn temperature_producer(
    ctx: RuntimeContext<EmbassyAdapter>,
    temperature: Producer<Temperature, EmbassyAdapter>,
) {
    let log = ctx.log();

    log.info("📊 Starting temperature producer service...\n");

    for i in 0..10 {
        let temp = Temperature {
            sensor_id: "sensor-001",
            celsius: 20.0 + (i as f32 * 2.0),
            timestamp: i,
        };

        if let Err(e) = temperature.produce(temp).await {
            log.error(&alloc::format!("❌ Failed to produce temperature: {:?}", e));
        }

        Timer::after(Duration::from_secs(2)).await;
    }

    log.info("\n✅ Published 10 temperature readings");
}

/// Consumer that logs temperature readings
async fn temperature_consumer(
    ctx: RuntimeContext<EmbassyAdapter>,
    consumer: Consumer<Temperature, EmbassyAdapter>,
) {
    let log = ctx.log();

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to temperature buffer");
        return;
    };

    while let Ok(temp) = reader.recv().await {
        log.info(&alloc::format!(
            "Temperature produced: {:.1}°C from {}",
            temp.celsius,
            temp.sensor_id
        ));
    }
}

/// Consumer that processes commands received from MQTT
async fn command_consumer(
    ctx: RuntimeContext<EmbassyAdapter>,
    consumer: Consumer<TemperatureCommand, EmbassyAdapter>,
) {
    let log = ctx.log();

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to command buffer");
        return;
    };

    log.info("📡 Listening for commands from MQTT...");

    while let Ok(cmd) = reader.recv().await {
        log.info(&alloc::format!(
            "📨 Command received: action='{}', sensor_id='{}'",
            cmd.action.as_str(),
            cmd.sensor_id.as_str()
        ));

        // Process command based on action
        match cmd.action.as_str() {
            "read" => log.info(&alloc::format!(
                "  → Would read from sensor {}",
                cmd.sensor_id.as_str()
            )),
            "reset" => log.info(&alloc::format!(
                "  → Would reset sensor {}",
                cmd.sensor_id.as_str()
            )),
            _ => log.warn(&alloc::format!(
                "  ⚠️  Unknown action: {}",
                cmd.action.as_str()
            )),
        }
    }
}

//
// ============================================================================
// MQTT CONFIGURATION
// ============================================================================
//

/// MQTT broker IP address (modify for your network)
const MQTT_BROKER_IP: &str = "192.168.1.3";

/// MQTT broker port
const MQTT_BROKER_PORT: u16 = 1883;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize heap for the allocator
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 32768; // 32KB heap
        static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe {
            let heap_ptr = core::ptr::addr_of_mut!(HEAP);
            ALLOCATOR.init((*heap_ptr).as_ptr() as usize, HEAP_SIZE)
        }
    }

    info!("🚀 Starting Embassy MQTT Connector Demo");

    // Configure MCU clocks for STM32H563ZI (from official embassy example)
    let mut config = Config::default();
    {
        use embassy_stm32::rcc::*;
        use embassy_stm32::time::Hertz;

        config.rcc.hsi = None;
        config.rcc.hsi48 = Some(Default::default()); // needed for RNG
        config.rcc.hse = Some(Hse {
            freq: Hertz(8_000_000),
            mode: HseMode::BypassDigital,
        });
        config.rcc.pll1 = Some(Pll {
            source: PllSource::HSE,
            prediv: PllPreDiv::DIV2,
            mul: PllMul::MUL125,
            divp: Some(PllDiv::DIV2),
            divq: Some(PllDiv::DIV2),
            divr: None,
        });
        config.rcc.ahb_pre = AHBPrescaler::DIV1;
        config.rcc.apb1_pre = APBPrescaler::DIV1;
        config.rcc.apb2_pre = APBPrescaler::DIV1;
        config.rcc.apb3_pre = APBPrescaler::DIV1;
        config.rcc.sys = Sysclk::PLL1_P;
        config.rcc.voltage_scale = VoltageScale::Scale0;
    }
    let p = embassy_stm32::init(config);

    info!("✅ MCU initialized");

    // Setup LED for visual feedback
    let mut led = Output::new(p.PB0, Level::Low, Speed::Low);

    // Generate random seed for network stack
    let mut rng = Rng::new(p.RNG, Irqs);
    let mut seed = [0; 8];
    rng.fill_bytes(&mut seed);
    let seed = u64::from_le_bytes(seed);

    info!("🔧 Initializing Ethernet...");

    // MAC address for this device
    let mac_addr = [0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF];

    // Create Ethernet device
    static PACKETS: StaticCell<PacketQueue<4, 4>> = StaticCell::new();

    let device = Ethernet::new(
        PACKETS.init(PacketQueue::<4, 4>::new()),
        p.ETH,
        Irqs,
        p.PA1,  // ETH_REF_CLK
        p.PA2,  // ETH_MDIO
        p.PC1,  // ETH_MDC
        p.PA7,  // ETH_CRS_DV
        p.PC4,  // ETH_RXD0
        p.PC5,  // ETH_RXD1
        p.PG13, // ETH_TXD0
        p.PB15, // ETH_TXD1 (corrected from PB13)
        p.PG11, // ETH_TX_EN
        GenericPhy::new_auto(),
        mac_addr,
    );

    // Network configuration (using DHCP)
    let config = embassy_net::Config::dhcpv4(Default::default());
    // Alternative: Static IP configuration
    // let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
    //     address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 1, 50), 24),
    //     dns_servers: Vec::new(),
    //     gateway: Some(Ipv4Address::new(192, 168, 1, 1)),
    // });

    // Initialize network stack
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    static STACK_CELL: StaticCell<embassy_net::Stack<'static>> = StaticCell::new();

    let (stack_obj, runner) =
        embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

    let stack: &'static _ = STACK_CELL.init(stack_obj);

    // Spawn network task
    spawner.spawn(unwrap!(net_task(runner)));

    info!("⏳ Waiting for network configuration (DHCP)...");

    // Wait for DHCP to complete and network to be ready
    stack.wait_config_up().await;

    info!("✅ Network ready!");
    if let Some(config) = stack.config_v4() {
        info!("   IP address: {}", config.address);
    }

    // Blink LED to show network is up
    for _ in 0..3 {
        led.set_high();
        Timer::after(Duration::from_millis(100)).await;
        led.set_low();
        Timer::after(Duration::from_millis(100)).await;
    }

    info!("🔌 Initializing MQTT client...");

    // Create AimDB database with Embassy adapter
    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new_with_network(spawner, stack));

    info!("🔧 Creating database with bidirectional MQTT connector...");

    // Build MQTT broker URL
    use alloc::format;
    let broker_url = format!("mqtt://{}:{}", MQTT_BROKER_IP, MQTT_BROKER_PORT);

    let mut builder = AimDbBuilder::new()
        .runtime(runtime.clone())
        .with_connector(
            MqttConnectorBuilder::new(&broker_url)
                .with_client_id("embassy-demo-001")
        );

    // Configure Temperature record with custom buffer sizing (outbound: AimDB → MQTT)
    //
    // For SPMC (Single Producer, Multiple Consumer) ring buffer:
    // - CAP=32: Ring buffer holds 32 temperature readings
    // - CONSUMERS=4: Maximum 4 concurrent consumers (used as SUBS for SPMC ring)
    //
    // Note: The CONSUMERS parameter is used differently based on buffer type:
    // - SPMC Ring: CONSUMERS becomes SUBS (independent ring buffer positions)
    // - SingleLatest: CONSUMERS becomes WATCH_N (watchers of latest value)
    // - Mailbox: CONSUMERS is ignored (single-slot)
    builder.configure::<Temperature>(|reg| {
        reg.buffer_sized::<32, 4>(EmbassyBufferType::SpmcRing)
            .source(temperature_producer)
            .tap(temperature_consumer)
            // Publish to MQTT as JSON
            .link_to("mqtt://sensors/temperature")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish()
            // Publish to MQTT as CSV with QoS 1 and retain
            .link_to("mqtt://sensors/raw")
            .with_config("qos", "1")
            .with_config("retain", "true")
            .with_serializer(|temp: &Temperature| Ok(temp.to_csv()))
            .finish();
    });

    // Configure TemperatureCommand record (inbound: MQTT → AimDB)
    builder.configure::<TemperatureCommand>(|reg| {
        reg.buffer_sized::<10, 2>(EmbassyBufferType::SpmcRing)
            .tap(command_consumer)
            // Subscribe from MQTT commands topic
            .link_from("mqtt://commands/temperature")
            .with_deserializer(|data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    info!("✅ Database configured with bidirectional MQTT:");
    info!("   OUTBOUND (AimDB → MQTT):");
    info!("     - mqtt://sensors/temperature (JSON format)");
    info!("     - mqtt://sensors/raw (CSV format, QoS=1, retain=true)");
    info!("   INBOUND (MQTT → AimDB):");
    info!("     - mqtt://commands/temperature (JSON commands)");
    info!("   Broker: {}:{}", MQTT_BROKER_IP, MQTT_BROKER_PORT);
    info!("");
    info!("💡 Try publishing a command:");
    info!(
        "   mosquitto_pub -h {} -t 'commands/temperature' \\",
        MQTT_BROKER_IP
    );
    info!("     -m '{{\"action\":\"read\",\"sensor_id\":\"sensor-001\"}}'");
    info!("");
    info!("   Press Reset button to restart.\n");

    static DB_CELL: StaticCell<aimdb_core::AimDb<EmbassyAdapter>> = StaticCell::new();
    let _db = DB_CELL.init(builder.build().await.expect("Failed to build database"));

    info!("✅ Database running with background services");

    // Main loop - blink LED to show system is alive
    // All services (producer, consumer, MQTT) run in the background
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(100)).await;
        led.set_low();
        Timer::after(Duration::from_millis(900)).await;
    }
}
