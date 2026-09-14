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
//! 1. Start the bench broker on a machine the board can reach over the LAN.
//!    It enforces authentication on both listeners, so a CONNECT that lost its
//!    credentials is refused rather than quietly accepted:
//! ```bash
//! cd ../../dev/mosquitto && ./gen-certs.sh && docker compose up -d
//! ```
//!
//! 2. Put the address and credentials it prints into the constants below.
//!
//! 3. Build and flash from this directory — its `.cargo/config.toml` selects
//!    the thumbv8m target and the probe-rs runner:
//! ```bash
//! cargo run --release
//! ```
//!
//! 4. Watch the traffic, and send the board a command:
//! ```bash
//! mosquitto_sub -h <broker> -p 1883 -u aimdb -P aimdb-bench -t 'sensors/#' -v
//! mosquitto_pub -h <broker> -p 1883 -u aimdb -P aimdb-bench \
//!     -t commands/temp/indoor -m '{"action":"read","sensor_id":"indoor-001"}'
//! ```
//!
//! ## TLS (`mqtts://`)
//!
//! `--features tls` switches the same demo to port 8883. The dialer resolves
//! the host, `embedded-tls` verifies the broker against the CA compiled in at
//! `ca.der`, and — this board having no RTC — certificate validity is dated by
//! the connector's own SNTP task, so the first handshake waits for a time sync.
//!
//! `gen-certs.sh` writes `ca.der` into this directory. `MQTT_BROKER_HOST` must
//! then be the same string the script was given: it is what the certificate is
//! verified against, and `embedded-tls` reads only `DNS:` SANs (an `IP:` SAN is
//! skipped), which is why the script puts even an IPv4 literal in as one.
//!
//! ```bash
//! cargo run --release --features tls
//! ```

extern crate alloc;

use aimdb_core::remote::SecurityPolicy;
use aimdb_core::{AimDbBuilder, Producer, RecordKey, RuntimeContext};
use aimdb_embassy_adapter::io::EmbassyUart;
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExtCustom};
use aimdb_serial_connector::SerialServer;
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

use aimdb_embassy_adapter::net::EmbassyNet;
use aimdb_mqtt_connector::MqttConnector;
#[cfg(feature = "tls")]
use aimdb_mqtt_connector::TlsOptions;

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
//
// Each cycles its readings endlessly rather than stopping after a fixed
// count: reconnect, re-subscribe and ping cadence only become observable
// while something is still publishing.
// ============================================================================

/// Indoor temperature sensor producer
async fn indoor_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    log.info("🏠 Starting INDOOR temperature producer...\n");

    for i in (0..5).cycle() {
        let temp = Temperature::new("indoor-001", 22.0 + (i as f32 * 0.5)); // Indoor temps: 22-24°C

        log.info(&alloc::format!(
            "🏠 Indoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        Timer::after(Duration::from_secs(2)).await;
    }
}

/// Outdoor temperature sensor producer
async fn outdoor_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    log.info("🌳 Starting OUTDOOR temperature producer...\n");

    for i in (0..5).cycle() {
        let temp = Temperature::new("outdoor-001", 5.0 + (i as f32 * 1.0)); // Outdoor temps: 5-9°C (cold!)

        log.info(&alloc::format!(
            "🌳 Outdoor sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        Timer::after(Duration::from_secs(2)).await;
    }
}

/// Server room temperature sensor producer
async fn server_room_temp_producer(ctx: RuntimeContext, temperature: Producer<Temperature>) {
    let log = ctx.log();
    log.info("🖥️  Starting SERVER ROOM temperature producer...\n");

    for i in (0..5).cycle() {
        let temp = Temperature::new("server-room-001", 18.0 + (i as f32 * 0.2)); // Server room: 18-19°C (cooled)

        log.info(&alloc::format!(
            "🖥️  Server room sensor producing: {:.1}°C",
            temp.celsius
        ));

        temperature.produce(temp);

        Timer::after(Duration::from_secs(2)).await;
    }
}

// ============================================================================
// MQTT CONFIGURATION
// ============================================================================

/// Where the broker is on your network — an IPv4 literal or a DNS name.
///
/// On a `tls` build this is also what the certificate is verified against, so
/// it must match the host `dev/mosquitto/gen-certs.sh` was given.
const MQTT_BROKER_HOST: &str = "192.168.1.10";

/// Broker port: 1883 plain, 8883 TLS.
#[cfg(not(feature = "tls"))]
const MQTT_BROKER_PORT: u16 = 1883;
#[cfg(feature = "tls")]
const MQTT_BROKER_PORT: u16 = 8883;

/// MQTT CONNECT credentials, which travel in the broker URL below.
const MQTT_USERNAME: &str = "aimdb";
const MQTT_PASSWORD: &str = "aimdb-bench";

/// The broker's root CA, DER-encoded. `gen-certs.sh` writes it here.
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

    // Initialize network stack (TLS builds carry one extra socket: SNTP)
    #[cfg(not(feature = "tls"))]
    static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
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

    // Build the broker URL. The scheme selects the transport; the authority
    // carries the credentials, which both backends read.
    //
    // Nothing un-escapes this string on the way to the CONNECT, so a password
    // needing percent-encoding (`@`, `:`, `/`) belongs in
    // `.with_credentials(..)` on the builder below instead.
    use alloc::format;
    #[cfg(not(feature = "tls"))]
    let scheme = "mqtt";
    #[cfg(feature = "tls")]
    let scheme = "mqtts";
    let broker_url = format!(
        "{}://{}:{}@{}:{}",
        scheme, MQTT_USERNAME, MQTT_PASSWORD, MQTT_BROKER_HOST, MQTT_BROKER_PORT
    );

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
    // Plain `mqtt://`: the adapter owns the socket, so the buffers are the
    // caller's and visible here. The same line on another runtime's adapter
    // needs no change in the connector.
    #[cfg(not(feature = "tls"))]
    let mqtt = {
        static MQTT_RX: StaticCell<[u8; 4096]> = StaticCell::new();
        static MQTT_TX: StaticCell<[u8; 4096]> = StaticCell::new();
        MqttConnector::new(&broker_url)
            .transport(EmbassyNet::tcp(
                *stack,
                MQTT_RX.init([0; 4096]),
                MQTT_TX.init([0; 4096]),
            ))
            .with_client_id("embassy-demo-001")
    };

    // `mqtts://` dials through the same transport as `mqtt://`; the adapter
    // resolves the host. The board's TRNG, the broker's root CA, and the record
    // buffers (16 640 bytes read is the enforced minimum — a TLS 1.3 peer may
    // send full-size records). `init_with` keeps the arrays off the stack.
    // This board has no RTC, so the validity clock comes from SNTP.
    #[cfg(feature = "tls")]
    let mqtt = {
        static MQTT_RX: StaticCell<[u8; 4096]> = StaticCell::new();
        static MQTT_TX: StaticCell<[u8; 4096]> = StaticCell::new();
        static TLS_READ_BUF: StaticCell<[u8; 16_640]> = StaticCell::new();
        static TLS_WRITE_BUF: StaticCell<[u8; 4_096]> = StaticCell::new();
        MqttConnector::new(&broker_url)
            .tls(
                EmbassyNet::tcp(*stack, MQTT_RX.init([0; 4096]), MQTT_TX.init([0; 4096])),
                TlsOptions::new(
                    rng,
                    MQTT_CA_DER,
                    TLS_READ_BUF.init_with(|| [0; 16_640]),
                    TLS_WRITE_BUF.init_with(|| [0; 4_096]),
                )
                .with_sntp(stack, "pool.ntp.org"),
            )
            .with_client_id("embassy-demo-001")
    };

    let mut builder = AimDbBuilder::new()
        .runtime(runtime.clone())
        .with_connector(mqtt)
        .with_connector(
            SerialServer::new(EmbassyUart::new(serial_rx, serial_tx))
                .security_policy(SecurityPolicy::read_only()),
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
    // Without the authority: the URL carries the password, and this line goes
    // to the RTT log.
    info!(
        "   Broker:   {}://{}:{}",
        scheme, MQTT_BROKER_HOST, MQTT_BROKER_PORT
    );
    info!("   SERIAL (read-only AimX over USART3 / ST-LINK VCP):");
    info!(
        "     aimdb --features transport-serial --connect serial:///dev/ttyACM0?baud=115200 record list"
    );
    info!("");
    #[cfg(not(feature = "tls"))]
    {
        info!(
            "Subscribe: mosquitto_sub -h {} -u {} -P <password> -t 'sensors/#' -v",
            MQTT_BROKER_HOST, MQTT_USERNAME
        );
        info!(
            "Command:   mosquitto_pub -h {} -u {} -P <password> -t 'commands/temp/indoor' \\",
            MQTT_BROKER_HOST, MQTT_USERNAME
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
