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
//!
//! ## TLS (`mqtts://`)
//!
//! Build with `--features tls` to connect to a TLS broker instead: the URL
//! becomes `mqtts://` (hostname, resolved via DNS), the CONNECT authenticates
//! with `MQTT_CREDENTIALS`, and certificate time comes from SNTP
//! automatically. Before building:
//!
//! 1. Set `MQTT_BROKER_HOST` (prefer a DNS name; an IPv4 literal needs the
//!    certificate to pin that IP in its CN — the `dev/mosquitto` bench CA
//!    does) and `MQTT_CREDENTIALS` below.
//! 2. Drop the broker's root CA in DER form at the crate root; for the dev
//!    bench:
//! ```bash
//! openssl x509 -in ../../dev/mosquitto/config/certs/ca.crt -outform der -out ca.der
//! ```
//! 3. Build and flash from this directory (its `.cargo/config.toml` selects
//!    the thumbv8m target and the probe-rs runner):
//! ```bash
//! cargo run --release --features tls
//! ```

extern crate alloc;

use aimdb_core::remote::SecurityPolicy;
use aimdb_core::{AimDbBuilder, Producer, RecordKey, RuntimeContext};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExtCustom};
use aimdb_serial_connector::embassy_transport::SerialServer;
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue};
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::peripherals::ETH;
use embassy_stm32::rng::Rng;
use embassy_stm32::usart::{BufferedUart, Config as UartConfig};
use embassy_stm32::{Config, bind_interrupts, eth, peripherals, rng, usart};
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;
#[cfg(feature = "tls")]
use aimdb_mqtt_connector::embassy_client::TlsOptions;

// Import shared types, monitors, and compile-time safe keys from the common crate
use mqtt_connector_demo_common::{
    CommandKey, SensorKey, Temperature, TemperatureCommand, command_consumer, temperature_logger,
};

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// Interrupt bindings for Ethernet and RNG
bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
    USART3 => usart::BufferedInterruptHandler<peripherals::USART3>;
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
async fn indoor_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    log.info("🏠 Starting INDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("indoor-001", 22.0 + (i as f32 * 0.5)); // Indoor temps: 22-24°C

        log.info(&alloc::format!(
            "🏠 Indoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        Timer::after(Duration::from_secs(2)).await;
    }

    log.info("✅ Indoor producer finished");
}

/// Outdoor temperature sensor producer
async fn outdoor_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    log.info("🌳 Starting OUTDOOR temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("outdoor-001", 5.0 + (i as f32 * 1.0)); // Outdoor temps: 5-9°C (cold!)

        log.info(&alloc::format!(
            "🌳 Outdoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        Timer::after(Duration::from_secs(2)).await;
    }

    log.info("✅ Outdoor producer finished");
}

/// Server room temperature sensor producer
async fn server_room_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    log.info("🖥️  Starting SERVER ROOM temperature producer...\n");

    for i in 0..5 {
        let temp = Temperature::new("server-room-001", 18.0 + (i as f32 * 0.2)); // Server room: 18-19°C (cooled)

        log.info(&alloc::format!(
            "🖥️  Server room sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        Timer::after(Duration::from_secs(2)).await;
    }

    log.info("✅ Server room producer finished");
}

// ============================================================================
// MQTT CONFIGURATION
// ============================================================================

/// MQTT broker IP address (modify for your network)
#[cfg(not(feature = "tls"))]
const MQTT_BROKER_IP: &str = "192.168.1.10";

/// MQTT broker port
#[cfg(not(feature = "tls"))]
const MQTT_BROKER_PORT: u16 = 1883;

/// TLS broker host (modify for your broker). Prefer a DNS name; an IPv4
/// literal verifies only when the certificate pins that IP in its CN, as
/// the dev bench CA does (`dev/mosquitto`).
#[cfg(feature = "tls")]
const MQTT_BROKER_HOST: &str = "192.168.1.10";

/// TLS MQTT broker port
#[cfg(feature = "tls")]
const MQTT_BROKER_TLS_PORT: u16 = 8883;

/// Credentials for MQTT CONNECT (`None` connects unauthenticated).
#[cfg(feature = "tls")]
const MQTT_CREDENTIALS: Option<(&str, &str)> = None; // Some(("user", "password"))

/// The broker's root CA, DER-encoded (see the TLS section in the module doc).
#[cfg(feature = "tls")]
static MQTT_CA_DER: &[u8] = include_bytes!("../ca.der");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize heap for the allocator
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 98304; // 96KB heap (MQTT + serial AimX server JSON)
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
            source: PllSource::Hse,
            prediv: PllPreDiv::Div2,
            mul: PllMul::Mul125,
            divp: Some(PllDiv::Div2),
            divq: Some(PllDiv::Div2),
            divr: None,
        });
        config.rcc.ahb_pre = AHBPrescaler::Div1;
        config.rcc.apb1_pre = APBPrescaler::Div1;
        config.rcc.apb2_pre = APBPrescaler::Div1;
        config.rcc.apb3_pre = APBPrescaler::Div1;
        config.rcc.sys = Sysclk::Pll1P;
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

    // The one TRNG instance seeds the net stack (above) and then feeds the
    // TLS handshake, so it parks in a static for the connector's `'static`
    // bound.
    #[cfg(feature = "tls")]
    let rng = {
        static TLS_RNG: StaticCell<Rng<'static, peripherals::RNG>> = StaticCell::new();
        TLS_RNG.init(rng)
    };

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

    // Initialize network stack (TLS builds carry two extra sockets: DNS + SNTP)
    #[cfg(not(feature = "tls"))]
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    #[cfg(feature = "tls")]
    static RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
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
    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new());

    // Build MQTT broker URL (the scheme selects the transport)
    use alloc::format;
    #[cfg(not(feature = "tls"))]
    let broker_url = format!("mqtt://{}:{}", MQTT_BROKER_IP, MQTT_BROKER_PORT);
    #[cfg(feature = "tls")]
    let broker_url = format!("mqtts://{}:{}", MQTT_BROKER_HOST, MQTT_BROKER_TLS_PORT);

    // ── AimX-over-serial: serve this db over USART3 (ST-LINK VCP, PD8=TX/PD9=RX) ──
    // A *second* connector alongside MQTT. With no extra cabling on a Nucleo-H563ZI
    // it appears on the host as /dev/ttyACM0; read the live records with:
    //   aimdb --features transport-serial \
    //         --connect serial:///dev/ttyACM0?baud=115200 record list
    // (sensor records are SpmcRing → use `record drain`/`watch`; `get` has no
    // canonical latest). defmt logs ride RTT (SWD), separate from this data UART.
    static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let mut uart_config = UartConfig::default();
    uart_config.baudrate = 115_200;
    let uart = BufferedUart::new(
        p.USART3,
        p.PD9, // RX
        p.PD8, // TX
        TX_BUF.init([0; 256]),
        RX_BUF.init([0; 256]),
        Irqs,
        uart_config,
    )
    .unwrap();
    let (serial_tx, serial_rx) = uart.split();

    // Read-only: each record has a single writer (a sensor source, or MQTT for the
    // command records), so remote `record.set` is refused — peers can
    // list/drain/subscribe, not write.
    let mqtt = MqttConnectorBuilder::new(&broker_url, stack).with_client_id("embassy-demo-001");

    // TLS materials: the board's TRNG, the broker's root CA, and the record
    // buffers (16 640 bytes read is the enforced minimum — a TLS 1.3 peer
    // may send full-size records). `init_with` keeps the arrays off the stack.
    #[cfg(feature = "tls")]
    let mqtt = {
        static TLS_READ_BUF: StaticCell<[u8; 16_640]> = StaticCell::new();
        static TLS_WRITE_BUF: StaticCell<[u8; 4_096]> = StaticCell::new();
        let mqtt = mqtt.with_tls(TlsOptions::new(
            rng,
            MQTT_CA_DER,
            TLS_READ_BUF.init_with(|| [0; 16_640]),
            TLS_WRITE_BUF.init_with(|| [0; 4_096]),
        ));
        match MQTT_CREDENTIALS {
            Some((username, password)) => mqtt.with_credentials(username, password),
            None => mqtt,
        }
    };

    let mut builder = AimDbBuilder::new()
        .runtime(runtime.clone())
        .with_connector(mqtt)
        .with_connector(
            SerialServer::new(serial_rx, serial_tx).security_policy(SecurityPolicy::read_only()),
        );

    // ========================================================================
    // TEMPERATURE SENSORS (outbound: AimDB → MQTT)
    // Using compile-time safe SensorKey enum - typos caught at compile time!
    // ========================================================================

    builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .source(indoor_temp_producer)
            .tap(temperature_logger)
            .link_to(SensorKey::TempIndoor.link_address().unwrap())
            .with_serializer(|_ctx, temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    builder.configure::<Temperature>(SensorKey::TempOutdoor, |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .source(outdoor_temp_producer)
            .tap(temperature_logger)
            .link_to(SensorKey::TempOutdoor.link_address().unwrap())
            .with_serializer(|_ctx, temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    builder.configure::<Temperature>(SensorKey::TempServerRoom, |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .source(server_room_temp_producer)
            .tap(temperature_logger)
            .link_to(SensorKey::TempServerRoom.link_address().unwrap())
            .with_serializer(|_ctx, temp: &Temperature| Ok(temp.to_json_vec()))
            .finish();
    });

    // ========================================================================
    // COMMAND CONSUMERS (inbound: MQTT → AimDB)
    // Using compile-time safe CommandKey enum
    // ========================================================================

    builder.configure::<TemperatureCommand>(CommandKey::TempIndoor, |reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .tap(command_consumer)
            .link_from(CommandKey::TempIndoor.link_address().unwrap())
            .with_deserializer(|_ctx, data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    builder.configure::<TemperatureCommand>(CommandKey::TempOutdoor, |reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SpmcRing)
            .with_remote_access()
            .tap(command_consumer)
            .link_from(CommandKey::TempOutdoor.link_address().unwrap())
            .with_deserializer(|_ctx, data: &[u8]| TemperatureCommand::from_json(data))
            .finish();
    });

    info!("✅ Database configured with multi-sensor MQTT:");
    info!("   OUTBOUND: sensors/temp/indoor, outdoor, server_room");
    info!("   INBOUND:  commands/temp/indoor, outdoor");
    info!("   Broker:   {}", broker_url.as_str());
    info!("   SERIAL (read-only AimX over USART3 / ST-LINK VCP):");
    info!(
        "     aimdb --features transport-serial --connect serial:///dev/ttyACM0?baud=115200 record list"
    );
    info!("");
    #[cfg(not(feature = "tls"))]
    {
        info!(
            "Subscribe: mosquitto_sub -h {} -t 'sensors/#' -v",
            MQTT_BROKER_IP
        );
        info!(
            "Command:   mosquitto_pub -h {} -t 'commands/temp/indoor' \\",
            MQTT_BROKER_IP
        );
        info!("             -m '{{\"action\":\"read\",\"sensor_id\":\"test\"}}'");
    }
    #[cfg(feature = "tls")]
    info!("TLS: first connect waits for the automatic SNTP time sync");
    info!("");

    static DB_CELL: StaticCell<aimdb_core::AimDb> = StaticCell::new();
    let (db, db_runner) = builder.build().await.expect("Failed to build database");
    let _db = DB_CELL.init(db);

    info!("✅ Database running with background services");

    // Drive the AimDB runner (all connector/tap/source futures) and LED blink concurrently.
    embassy_futures::join::join(db_runner.run(), async {
        loop {
            led.set_high();
            Timer::after(Duration::from_millis(100)).await;
            led.set_low();
            Timer::after(Duration::from_millis(900)).await;
        }
    })
    .await;
}
