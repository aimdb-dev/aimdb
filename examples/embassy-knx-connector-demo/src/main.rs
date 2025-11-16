#![no_std]
#![no_main]

//! KNX Connector Demo for Embassy Runtime
//!
//! Demonstrates KNX/IP bus monitoring on embedded hardware with AimDB.
//!
//! ## Hardware Requirements
//!
//! - STM32H563ZI Nucleo board (or similar with Ethernet)
//! - Ethernet connection to network with KNX/IP gateway
//! - KNX/IP gateway (e.g., MDT SCN-IP000.03, Gira X1, ABB IP Interface)
//!
//! ## Running
//!
//! 1. Ensure KNX/IP gateway is accessible on your network
//!
//! 2. Update KNX_GATEWAY_IP constant below to match your gateway
//!
//! 3. Update group addresses to match your KNX installation
//!
//! 4. Flash to target:
//! ```bash
//! cargo run --example embassy-knx-connector-demo --features embassy-runtime
//! ```
//!
//! 5. Trigger KNX events by:
//!    - Pressing physical KNX switches
//!    - Sending telegrams via ETS
//!
//! The demo will log all received KNX telegrams in real-time.

extern crate alloc;

use aimdb_core::{AimDbBuilder, Consumer, RuntimeContext};
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

use aimdb_knx_connector::embassy_client::KnxConnectorBuilder;

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
// KNX DATA TYPES
// ============================================================================

/// Light state from KNX bus (DPT 1.001)
#[derive(Clone, Debug)]
struct LightState {
    group_address: HeaplessString<16>, // "1/0/7"
    is_on: bool,
    #[allow(dead_code)]
    timestamp: u32,
}

/// Temperature from KNX bus (DPT 9.001)
#[derive(Clone, Debug)]
struct Temperature {
    group_address: HeaplessString<16>, // "1/1/10"
    celsius: f32,
    #[allow(dead_code)]
    timestamp: u32,
}

impl Temperature {
    /// Parse DPT 9.001 (2-byte float temperature)
    fn from_knx_dpt9(data: &[u8]) -> Result<f32, alloc::string::String> {
        use alloc::string::ToString;

        if data.len() < 2 {
            return Err("DPT 9.001 requires 2 bytes".to_string());
        }

        let raw = i16::from_be_bytes([data[0], data[1]]);
        let exponent = ((raw as u16) >> 11) & 0x0F;
        let mantissa = (raw as u16) & 0x7FF;
        let sign = if (raw as u16 & 0x8000) != 0 {
            -1.0
        } else {
            1.0
        };

        // Formula: value = sign * mantissa * 2^(exponent - 12) * 0.01
        let value =
            sign * (mantissa as f32) * micromath::F32Ext::powi(2.0, exponent as i32 - 12) * 0.01;

        Ok(value)
    }
}

/// Consumer that logs incoming KNX light telegrams
async fn light_monitor(
    ctx: RuntimeContext<EmbassyAdapter>,
    consumer: Consumer<LightState, EmbassyAdapter>,
) {
    let log = ctx.log();

    log.info("� Light monitor started - watching KNX bus...\n");

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to light buffer");
        return;
    };

    while let Ok(state) = reader.recv().await {
        log.info(&alloc::format!(
            "🔵 KNX telegram: {} = {}",
            state.group_address.as_str(),
            if state.is_on { "ON ✨" } else { "OFF" }
        ));
    }
}

/// Consumer that logs incoming KNX temperature telegrams
async fn temperature_monitor(
    ctx: RuntimeContext<EmbassyAdapter>,
    consumer: Consumer<Temperature, EmbassyAdapter>,
) {
    let log = ctx.log();

    log.info("🌡️  Temperature monitor started - watching KNX bus...\n");

    let Ok(mut reader) = consumer.subscribe() else {
        log.error("Failed to subscribe to temperature buffer");
        return;
    };

    while let Ok(temp) = reader.recv().await {
        log.info(&alloc::format!(
            "🌡️  KNX temperature: {} = {:.1}°C",
            temp.group_address.as_str(),
            temp.celsius
        ));
    }
}

//
// ============================================================================
// KNX CONFIGURATION
// ============================================================================
//

/// KNX/IP gateway IP address (modify for your network)
const KNX_GATEWAY_IP: &str = "192.168.1.19";

/// KNX/IP gateway port (default: 3671)
const KNX_GATEWAY_PORT: u16 = 3671;

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

    info!("🚀 Starting Embassy KNX Connector Demo");

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

    info!("🔌 Initializing KNX client...");

    // Create AimDB database with Embassy adapter
    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new_with_network(spawner, stack));

    info!("🔧 Creating database with KNX bus monitor...");

    // Build KNX gateway URL
    use alloc::format;
    let gateway_url = format!("knx://{}:{}", KNX_GATEWAY_IP, KNX_GATEWAY_PORT);

    let mut builder = AimDbBuilder::new()
        .runtime(runtime.clone())
        .with_connector(KnxConnectorBuilder::new(&gateway_url));

    // Configure LightState record (inbound: KNX → AimDB)
    builder.configure::<LightState>(|reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SingleLatest)
            .tap(light_monitor)
            // Subscribe from KNX group address 1/0/7 (light switch monitoring)
            .link_from("knx://1/0/7")
            .with_deserializer(|data: &[u8]| {
                let is_on = data.first().map(|&b| b != 0).unwrap_or(false);
                let mut group_address = HeaplessString::<16>::new();
                let _ = group_address.push_str("1/0/7");

                Ok(LightState {
                    group_address,
                    is_on,
                    timestamp: 0, // Would use embassy_time::Instant in production
                })
            })
            .finish();
    });

    // Configure Temperature record (inbound: KNX → AimDB)
    builder.configure::<Temperature>(|reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SingleLatest)
            .tap(temperature_monitor)
            // Subscribe from KNX temperature sensor (group address 1/1/10)
            .link_from("knx://1/1/10")
            .with_deserializer(|data: &[u8]| {
                let celsius = Temperature::from_knx_dpt9(data)?;
                let mut group_address = HeaplessString::<16>::new();
                let _ = group_address.push_str("1/1/10");

                Ok(Temperature {
                    group_address,
                    celsius,
                    timestamp: 0,
                })
            })
            .finish();
    });

    info!("✅ Database configured with KNX bus monitor:");
    info!("   INBOUND (KNX → AimDB):");
    info!("     - knx://1/0/7 (light monitoring, DPT 1.001)");
    info!("     - knx://1/1/10 (temperature monitoring, DPT 9.001)");
    info!("   Gateway: {}:{}", KNX_GATEWAY_IP, KNX_GATEWAY_PORT);
    info!("");
    info!("💡 The demo will:");
    info!("   1. Connect to the KNX/IP gateway");
    info!("   2. Monitor KNX bus for telegrams on configured addresses");
    info!("   3. Log all received KNX telegrams in real-time");
    info!("");
    info!("   Trigger events by:");
    info!("   - Pressing physical KNX switches");
    info!("   - Sending telegrams via ETS");
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
