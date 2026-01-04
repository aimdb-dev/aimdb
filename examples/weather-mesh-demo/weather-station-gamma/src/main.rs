#![no_std]
#![no_main]

//! # Weather Station Gamma
//!
//! MCU-based weather station running on Embassy (STM32H563ZI with Ethernet).
//! Generates synthetic sensor data like beta, but on embedded hardware.
//!
//! ## Hardware Requirements
//!
//! - STM32H563ZI Nucleo board
//! - Ethernet connection to network with MQTT broker
//!
//! ## Running
//!
//! 1. Start MQTT broker on your network
//! 2. Update MQTT_BROKER_IP constant below
//! 3. Flash to target:
//! ```bash
//! cd examples/weather-mesh-demo/weather-station-gamma
//! cargo build --release
//! cargo flash --release
//! ```

extern crate alloc;

use aimdb_core::{AimDbBuilder, Producer, RecordKey, RuntimeContext};
use aimdb_data_contracts::{Simulatable, SimulationConfig, SimulationParams};
use aimdb_embassy_adapter::{
    EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExt, EmbassyRecordRegistrarExtCustom,
};
use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue};
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::peripherals::ETH;
use embassy_stm32::rng::Rng;
use embassy_stm32::{bind_interrupts, eth, peripherals, rng, Config};
use embassy_time::{Duration, Timer};
use rand::SeedableRng;
use static_cell::StaticCell;
use weather_mesh_common::{Humidity, NodeKey, Temperature};
use {defmt_rtt as _, panic_probe as _};

// Simple embedded allocator
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// MQTT broker address - update this to match your network
const MQTT_BROKER_IP: &str = "192.168.1.3";
const MQTT_BROKER_PORT: u16 = 1883;

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

/// Temperature producer - generates synthetic data
async fn temperature_producer(
    ctx: RuntimeContext<EmbassyAdapter>,
    producer: Producer<Temperature, EmbassyAdapter>,
) {
    let log = ctx.log();
    log.info("🌡️  Starting temperature producer...");

    let config = SimulationConfig {
        enabled: true,
        interval_ms: 5000,
        params: SimulationParams {
            base: 22.0,     // Portable: ~22°C
            variation: 5.0, // ±5°C (more variation)
            step: 0.3,      // Random walk
            trend: 0.0,
        },
    };

    let mut rng = rand::rngs::SmallRng::from_seed([42; 16]);
    let mut prev: Option<Temperature> = None;

    loop {
        let now = ctx.time().now().as_millis();
        let temp = Temperature::simulate(&config, prev.as_ref(), &mut rng, now);

        log.info(&alloc::format!("📊 Temp: {:.1}°C", temp.celsius));

        if let Err(e) = producer.produce(temp.clone()).await {
            log.error(&alloc::format!("❌ Failed to produce: {:?}", e));
        }

        prev = Some(temp);
        Timer::after(Duration::from_secs(5)).await;
    }
}

/// Humidity producer - generates synthetic data
async fn humidity_producer(
    ctx: RuntimeContext<EmbassyAdapter>,
    producer: Producer<Humidity, EmbassyAdapter>,
) {
    let log = ctx.log();
    log.info("💧 Starting humidity producer...");

    let config = SimulationConfig {
        enabled: true,
        interval_ms: 5000,
        params: SimulationParams {
            base: 55.0,      // Portable: ~55%
            variation: 15.0, // ±15%
            step: 0.3,       // Random walk
            trend: 0.0,
        },
    };

    let mut rng = rand::rngs::SmallRng::from_seed([84; 16]);
    let mut prev: Option<Humidity> = None;

    loop {
        let now = ctx.time().now().as_millis();
        let humidity = Humidity::simulate(&config, prev.as_ref(), &mut rng, now);

        log.info(&alloc::format!("📊 Humidity: {:.1}%", humidity.percent));

        if let Err(e) = producer.produce(humidity.clone()).await {
            log.error(&alloc::format!("❌ Failed to produce: {:?}", e));
        }

        prev = Some(humidity);
        Timer::after(Duration::from_secs(5)).await;
    }
}

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

    info!("🚀 Weather Station Gamma (STM32H563ZI)");

    // Configure MCU clocks for STM32H563ZI
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

    let mut builder = AimDbBuilder::new().runtime(runtime.clone()).with_connector(
        MqttConnectorBuilder::new(&broker_url).with_client_id("weather-station-gamma"),
    );

    // Configure temperature record
    let temp_topic = NodeKey::Gamma.link_address().unwrap();
    let temp_full = format!("{}temperature", temp_topic);
    builder.configure::<Temperature>(NodeKey::Gamma, |reg| {
        reg.buffer_sized::<16, 1>(EmbassyBufferType::SpmcRing)
            .source(temperature_producer)
            .link_to(&temp_full)
            .with_serializer(|t: &Temperature| {
                // Manual JSON serialization for no_std
                let whole = t.celsius as i32;
                let frac = ((t.celsius - whole as f32).abs() * 10.0 + 0.5) as i32 % 10;
                Ok(alloc::format!(
                    r#"{{"celsius":{}.{},"timestamp":{}}}
"#,
                    whole,
                    frac,
                    t.timestamp
                )
                .into_bytes())
            })
            .finish();
    });

    // Configure humidity record
    let humidity_full = format!("{}humidity", temp_topic);
    builder.configure::<Humidity>(NodeKey::Gamma, |reg| {
        reg.buffer_sized::<16, 1>(EmbassyBufferType::SpmcRing)
            .source(humidity_producer)
            .link_to(&humidity_full)
            .with_serializer(|h: &Humidity| {
                // Manual JSON serialization for no_std
                let whole = h.percent as i32;
                let frac = ((h.percent - whole as f32).abs() * 10.0 + 0.5) as i32 % 10;
                Ok(alloc::format!(
                    r#"{{"percent":{}.{},"timestamp":{}}}
"#,
                    whole,
                    frac,
                    h.timestamp
                )
                .into_bytes())
            })
            .finish();
    });

    info!("✅ Database configured with synthetic sensors:");
    info!("   Temperature: {}", temp_full.as_str());
    info!("   Humidity: {}", humidity_full.as_str());
    info!("   Broker: {}:{}", MQTT_BROKER_IP, MQTT_BROKER_PORT);
    info!("");

    static DB_CELL: StaticCell<aimdb_core::AimDb<EmbassyAdapter>> = StaticCell::new();
    let _db = DB_CELL.init(builder.build().await.expect("Failed to build database"));

    info!("✅ Database running");
    info!("🎯 Weather Station Gamma ready!");

    // Main loop - blink LED to show system is alive
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(100)).await;
        led.set_low();
        Timer::after(Duration::from_millis(900)).await;
    }
}
