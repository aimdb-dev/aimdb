#![no_std]
#![no_main]

//! MQTT Connector Demo for Embassy Runtime
//!
//! Demonstrates bidirectional MQTT integration with multiple sensors:
//! - Multiple temperature sensors publishing to different topics
//! - Multiple command consumers receiving from different topics
//!
//! This demo uses `mqtt-connector-demo-common` for shared types and monitors,
//! demonstrating AimDB's "write once, run anywhere" capability.
//!
//! ## Hardware Requirements
//!
//! - STM32H563ZI Nucleo board (or similar with Ethernet)
//! - Ethernet connection to network with MQTT broker
//!
//! ## Task Pool Requirements
//!
//! This demo spawns multiple concurrent tasks:
//! - 3 temperature producers (indoor, outdoor, server_room)
//! - 3 temperature loggers (tap consumers)
//! - 2 command consumers
//! - 2 MQTT connector tasks (manager + event router)
//! - 3 outbound publisher tasks
//!
//! Total: 13 tasks - requires `embassy-task-pool-16` feature in aimdb-embassy-adapter.
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
//! mosquitto_pub -h <broker-ip> -t 'commands/temp/indoor' -m '{"action":"read","sensor_id":"indoor-001"}'
//! ```
//!
//! 4. Update MQTT_BROKER_IP constant below to match your broker
//!
//! 5. Flash to target:
//! ```bash
//! cargo run --example embassy-mqtt-connector-demo --features embassy-runtime,tracing
//! ```

extern crate alloc;

use aimdb_core::{AimDbBuilder, Producer, RuntimeContext};
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
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;

// Import shared types and monitors from the common crate
use mqtt_connector_demo_common::{
    Temperature, TemperatureCommand, command_consumer, temperature_logger,
};

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// Interrupt bindings for Ethernet and RNG
bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
});

type Device =
    Ethernet<'static, ETH, GenericPhy<embassy_stm32::eth::Sma<'static, peripherals::ETH_SMA>>>;

/// Network task that runs the embassy-net stack
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
    runner.run().await
}

// ============================================================================
// TEMPERATURE PRODUCERS (platform-specific due to embassy-time)
// ============================================================================

/// Indoor temperature sensor producer
async fn indoor_temp_producer(
    ctx: RuntimeContext<EmbassyAdapter>,
    temperature: Producer<Temperature, EmbassyAdapter>,
) {
    let log = ctx.log();
    log.info("🏠 Starting INDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("indoor-001", 22.0 + (i as f32 * 0.5)); // Indoor temps: 22-24°C

        log.info(&alloc::format!(
            "🏠 Indoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        if let Err(e) = temperature.produce(temp).await {
            log.error(&alloc::format!("❌ Failed to produce indoor temp: {:?}", e));
        }

        Timer::after(Duration::from_secs(2)).await;
    }

    log.info("✅ Indoor producer finished");
}

/// Outdoor temperature sensor producer
async fn outdoor_temp_producer(
    ctx: RuntimeContext<EmbassyAdapter>,
    temperature: Producer<Temperature, EmbassyAdapter>,
) {
    let log = ctx.log();
    log.info("🌳 Starting OUTDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("outdoor-001", 5.0 + (i as f32 * 1.0)); // Outdoor temps: 5-9°C (cold!)

        log.info(&alloc::format!(
            "🌳 Outdoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        if let Err(e) = temperature.produce(temp).await {
            log.error(&alloc::format!(
                "❌ Failed to produce outdoor temp: {:?}",
                e
            ));
        }

        Timer::after(Duration::from_secs(2)).await;
    }

    log.info("✅ Outdoor producer finished");
}

/// Server room temperature sensor producer
async fn server_room_temp_producer(
    ctx: RuntimeContext<EmbassyAdapter>,
    temperature: Producer<Temperature, EmbassyAdapter>,
) {
    let log = ctx.log();
    log.info("🖥️  Starting SERVER ROOM temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("server-room-001", 18.0 + (i as f32 * 0.2)); // Server room: 18-19°C (cooled)

        log.info(&alloc::format!(
            "🖥️  Server room sensor producing: {:.1}°C",
            temp.celsius
        ));

        if let Err(e) = temperature.produce(temp).await {
            log.error(&alloc::format!(
                "❌ Failed to produce server room temp: {:?}",
                e
            ));
        }

        Timer::after(Duration::from_secs(2)).await;
    }

    log.info("✅ Server room producer finished");
}

// ============================================================================
// MQTT CONFIGURATION
// ============================================================================

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
        p.PA7,  // ETH_CRS_DV
        p.PC4,  // ETH_RXD0
        p.PC5,  // ETH_RXD1
        p.PG13, // ETH_TXD0
        p.PB15, // ETH_TXD1
        p.PG11, // ETH_TX_EN
        mac_addr,
        p.ETH_SMA, // SMA peripheral
        p.PA2,     // ETH_MDIO
        p.PC1,     // ETH_MDC
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

    // Build MQTT broker URL
    use alloc::format;
    let broker_url = format!("mqtt://{}:{}", MQTT_BROKER_IP, MQTT_BROKER_PORT);

    let mut builder = AimDbBuilder::new()
        .runtime(runtime.clone())
        .with_connector(MqttConnectorBuilder::new(&broker_url).with_client_id("embassy-demo-001"));

    // ========================================================================
    // TEMPERATURE SENSORS (outbound: AimDB → MQTT)
    // Using shared temperature_logger monitor from common crate
    // ========================================================================

    builder.configure::<Temperature>("sensor.temp.indoor", |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
            .source(indoor_temp_producer)
            .tap(temperature_logger)
            .link_to("mqtt://sensors/temp/indoor")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    builder.configure::<Temperature>("sensor.temp.outdoor", |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
            .source(outdoor_temp_producer)
            .tap(temperature_logger)
            .link_to("mqtt://sensors/temp/outdoor")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    builder.configure::<Temperature>("sensor.temp.server_room", |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
            .source(server_room_temp_producer)
            .tap(temperature_logger)
            .link_to("mqtt://sensors/temp/server_room")
            .with_serializer(|temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    // ========================================================================
    // COMMAND CONSUMERS (inbound: MQTT → AimDB)
    // Using shared command_consumer monitor from common crate
    // ========================================================================

    builder.configure::<TemperatureCommand>("command.temp.indoor", |reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SpmcRing)
            .tap(command_consumer)
            .link_from("mqtt://commands/temp/indoor")
            .with_deserializer(|data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    builder.configure::<TemperatureCommand>("command.temp.outdoor", |reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SpmcRing)
            .tap(command_consumer)
            .link_from("mqtt://commands/temp/outdoor")
            .with_deserializer(|data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    info!("✅ Database configured with multi-sensor MQTT:");
    info!("   OUTBOUND: sensors/temp/indoor, outdoor, server_room");
    info!("   INBOUND:  commands/temp/indoor, outdoor");
    info!("   Broker:   {}:{}", MQTT_BROKER_IP, MQTT_BROKER_PORT);
    info!("");
    info!(
        "Subscribe: mosquitto_sub -h {} -t 'sensors/#' -v",
        MQTT_BROKER_IP
    );
    info!(
        "Command:   mosquitto_pub -h {} -t 'commands/temp/indoor' \\",
        MQTT_BROKER_IP
    );
    info!("             -m '{{\"action\":\"read\",\"sensor_id\":\"test\"}}'");
    info!("");

    static DB_CELL: StaticCell<aimdb_core::AimDb<EmbassyAdapter>> = StaticCell::new();
    let _db = DB_CELL.init(builder.build().await.expect("Failed to build database"));

    info!("✅ Database running with background services");

    // Main loop - blink LED to show system is alive
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(100)).await;
        led.set_low();
        Timer::after(Duration::from_millis(900)).await;
    }
}
