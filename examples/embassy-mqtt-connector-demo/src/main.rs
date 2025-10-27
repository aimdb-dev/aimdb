#![no_std]
#![no_main]

//! MQTT Connector Demo for Embassy Runtime
//!
//! Demonstrates MQTT integration with automatic publishing to multiple topics on embedded hardware.
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
//! 2. Subscribe to topics:
//! ```bash
//! mosquitto_sub -h <broker-ip> -t 'sensors/#' -v
//! ```
//!
//! 3. Update MQTT_BROKER_IP constant below to match your broker
//!
//! 4. Flash to target:
//! ```bash
//! cargo run --example embassy-mqtt-connector-demo --features embassy-runtime,tracing
//! ```

extern crate alloc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, Consumer, Producer, RuntimeContext};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyRecordRegistrarExt};
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

use aimdb_mqtt_connector::embassy_client::MqttConnector;

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

/// MQTT background task that maintains connection and processes publish requests
#[embassy_executor::task]
async fn mqtt_task(task: aimdb_mqtt_connector::embassy_client::MqttBackgroundTask) {
    task.run().await;
}

//
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

//
// ============================================================================
// MQTT CONFIGURATION
// ============================================================================
//

/// MQTT broker IP address (modify for your network)
const MQTT_BROKER_IP: &str = "192.168.1.3";

/// MQTT broker port
const MQTT_BROKER_PORT: u16 = 1883;

/// MQTT client ID for this device
const MQTT_CLIENT_ID: &str = "embassy-sensor-node";

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
    let (stack, runner) =
        embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

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

    // Create MQTT connector and background task
    let mqtt_result =
        MqttConnector::create(stack, MQTT_BROKER_IP, MQTT_BROKER_PORT, MQTT_CLIENT_ID).await;

    let mqtt = match mqtt_result {
        Ok(m) => m,
        Err(_e) => {
            error!("Failed to create MQTT connector - check broker IP and network");
            core::panic!("MQTT initialization failed");
        }
    };

    info!("✅ MQTT connector created");

    // Spawn the MQTT background task
    // This task maintains the connection and processes publish requests
    spawner.spawn(unwrap!(mqtt_task(mqtt.task)));

    info!("✅ MQTT background task spawned");

    // Make connector available for MQTT consumer
    static MQTT_CONNECTOR: StaticCell<aimdb_mqtt_connector::embassy_client::MqttConnector> =
        StaticCell::new();
    let mqtt_connector = MQTT_CONNECTOR.init(mqtt.connector);

    info!("🎉 MQTT connector ready");

    // Create AimDB database with Embassy adapter
    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new_with_spawner(spawner));

    let mqtt_connector = alloc::sync::Arc::new(mqtt_connector.clone());

    info!("🔧 Creating database with MQTT connector...");

    let mut builder = AimDbBuilder::new()
        .runtime(runtime.clone())
        .with_connector("mqtt", mqtt_connector.clone());

    builder.configure::<Temperature>(|reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .source(temperature_producer)
            .tap(temperature_consumer)
            // Publish to MQTT as JSON
            .link("mqtt://sensors/temperature")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish()
            // Publish to MQTT as CSV with QoS 1 and retain
            .link("mqtt://sensors/raw")
            .with_qos(1)
            .with_retain(true)
            .with_serializer(|temp: &Temperature| Ok(temp.to_csv()))
            .finish();
    });

    info!("✅ Database configured with MQTT connectors");
    info!("   - mqtt://sensors/temperature (JSON format)");
    info!("   - mqtt://sensors/raw (CSV format, QoS=1, retain=true)");
    info!("   Broker: {}:{}", MQTT_BROKER_IP, MQTT_BROKER_PORT);
    info!("   Press Reset button to restart.\n");

    static DB_CELL: StaticCell<aimdb_core::AimDb<EmbassyAdapter>> = StaticCell::new();
    let _db = DB_CELL.init(builder.build().expect("Failed to build database"));

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
