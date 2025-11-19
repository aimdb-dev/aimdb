#![no_std]
#![no_main]

//! KNX Connector Demo for Embassy Runtime
//!
//! Demonstrates bidirectional KNX/IP integration on embedded hardware with AimDB:
//! - Inbound: Monitor KNX bus telegrams → process in AimDB
//! - Outbound: Control KNX devices from AimDB (toggle light on button press)
//! - Real-time logging of light switches and temperature sensors
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
//!    - Pressing USER button (blue button) to toggle light on 1/0/6
//!    - Pressing physical KNX switches
//!    - Sending telegrams via ETS
//!
//! The demo will log all KNX activity in real-time.

extern crate alloc;

use aimdb_core::{AimDbBuilder, Consumer, RuntimeContext};
use aimdb_embassy_adapter::{
    EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExt, EmbassyRecordRegistrarExtCustom,
};
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue};
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
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

type Device =
    Ethernet<'static, ETH, GenericPhy<embassy_stm32::eth::Sma<'static, peripherals::ETH_SMA>>>;

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
    group_address: HeaplessString<16>, // "9/1/0"
    celsius: f32,
    #[allow(dead_code)]
    timestamp: u32,
}

/// Light control command to send to KNX bus (DPT 1.001)
#[derive(Clone, Debug)]
struct LightControl {
    #[allow(dead_code)]
    group_address: HeaplessString<16>, // "1/0/6"
    is_on: bool,
    #[allow(dead_code)]
    timestamp: u32,
}

impl Temperature {
    /// Parse DPT 9.001 (2-byte float temperature)
    fn from_knx_dpt9(data: &[u8]) -> Result<f32, alloc::string::String> {
        use alloc::string::ToString;

        // Determine where the actual temperature bytes are based on frame length
        let temp_bytes = if data.len() >= 4 {
            // Full frame: TPCI + APCI + 2 data bytes
            // Temperature is in bytes [2] and [3]
            [data[2], data[3]]
        } else if data.len() == 3 {
            // Short frame with control byte: TPCI/APCI + 2 data bytes
            // Temperature is in bytes [1] and [2]
            [data[1], data[2]]
        } else if data.len() == 2 {
            // Just the temperature bytes (no control bytes)
            [data[0], data[1]]
        } else {
            return Err("DPT 9.001 requires at least 2 bytes".to_string());
        };

        let raw = u16::from_be_bytes(temp_bytes);

        // DPT 9.001 format:
        // Bit 15: Sign (0=positive, 1=negative)
        // Bits 14-11: Exponent (4 bits, unsigned)
        // Bits 10-0: Mantissa (11 bits, unsigned)
        // Formula: value = (0.01 * mantissa) * 2^exponent * (sign ? -1 : 1)
        let sign_bit = (raw >> 15) & 0x01;
        let exponent = ((raw >> 11) & 0x0F) as i32;
        let mantissa = (raw & 0x07FF) as i16;

        // Apply sign to mantissa
        let signed_mantissa = if sign_bit == 1 { -mantissa } else { mantissa };

        // Calculate temperature: (0.01 * mantissa) * 2^exponent
        let value = (0.01 * signed_mantissa as f32) * micromath::F32Ext::powi(2.0, exponent);

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

/// Button handler that toggles light on button press
/// Uses the blue USER button (PC13) on STM32 Nucleo boards
async fn button_handler(
    ctx: RuntimeContext<EmbassyAdapter>,
    producer: aimdb_core::Producer<LightControl, EmbassyAdapter>,
    mut button: ExtiInput<'static>,
) {
    let log = ctx.log();

    log.info("🔘 Button handler started - press USER button to toggle light\n");
    log.info("   (This sends GroupValueWrite to KNX bus on 1/0/6)\n");

    // Check initial button state
    let initial_state = if button.is_high() {
        "HIGH (not pressed)"
    } else {
        "LOW (pressed)"
    };
    log.info(&alloc::format!(
        "   Initial button state: {}\n",
        initial_state
    ));

    let mut light_on = false;

    loop {
        log.info("⏳ Waiting for button press...\n");

        // Wait for button press (button is active low)
        button.wait_for_falling_edge().await;

        log.info("🔽 Button press detected!\n");

        // Debounce delay
        embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;

        // Ignore if button is no longer pressed (debounce)
        if button.is_high() {
            continue;
        }

        // Toggle light state
        light_on = !light_on;

        let mut group_address = HeaplessString::<16>::new();
        let _ = group_address.push_str("1/0/6");

        let state = LightControl {
            group_address,
            is_on: light_on,
            timestamp: 0,
        };

        match producer.produce(state).await {
            Ok(_) => {
                log.info(&alloc::format!(
                    "✅ Published to KNX: 1/0/6 = {} (sent to bus)",
                    if light_on { "ON ✨" } else { "OFF" }
                ));
            }
            Err(e) => {
                log.error(&alloc::format!("❌ Failed to publish: {:?}", e));
            }
        }

        // Wait for button release
        button.wait_for_rising_edge().await;
        embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
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

    // Setup LED for visual feedback (green LED on Nucleo)
    let mut led = Output::new(p.PB0, Level::Low, Speed::Low);

    // Setup USER button (blue button PC13 on Nucleo) with pull-up and interrupt support
    let button = ExtiInput::new(p.PC13, p.EXTI13, Pull::Down);

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
        p.ETH_SMA, // SMA peripheral (replaces old SMA pin)
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
            // Subscribe from KNX temperature sensor (group address 9/1/0)
            .link_from("knx://9/1/0")
            .with_deserializer(|data: &[u8]| {
                // DPT 9.001 can arrive in different formats depending on how the NPDU is structured:
                // - 4 bytes: [TPCI, APCI, temp_high, temp_low] (standard)
                // - 3 bytes: [combined_TPCI_APCI, temp_high, temp_low] (some gateways)
                // - 2 bytes: [temp_high, temp_low] (raw temperature data)
                if data.len() < 2 {
                    return Err(alloc::format!(
                        "Temperature data too short: {} bytes (need at least 2)",
                        data.len()
                    ));
                }

                let celsius = Temperature::from_knx_dpt9(data)?;
                let mut group_address = HeaplessString::<16>::new();
                let _ = group_address.push_str("9/1/0");

                Ok(Temperature {
                    group_address,
                    celsius,
                    timestamp: 0,
                })
            })
            .finish();
    });

    // Configure LightControl record (outbound: AimDB → KNX)
    // Configure outbound light control with button handler as data source
    builder.configure::<LightControl>(|reg| {
        reg.buffer_sized::<8, 2>(EmbassyBufferType::SingleLatest)
            .source_with_context(button, button_handler)
            // Publish to KNX group address 1/0/6 (light control)
            .link_to("knx://1/0/6")
            .with_serializer(|state: &LightControl| {
                // DPT 1.001 - boolean (1 byte)
                Ok(alloc::vec![if state.is_on { 0x01 } else { 0x00 }])
            })
            .finish();
    });

    info!("✅ Database configured with KNX bus monitor:");
    info!("   INBOUND (KNX → AimDB):");
    info!("     - knx://1/0/7 (light monitoring, DPT 1.001)");
    info!("     - knx://9/1/0 (temperature monitoring, DPT 9.001)");
    info!("   OUTBOUND (AimDB → KNX):");
    info!("     - knx://1/0/6 (light control, DPT 1.001)");
    info!("   Gateway: {}:{}", KNX_GATEWAY_IP, KNX_GATEWAY_PORT);
    info!("");
    info!("💡 The demo will:");
    info!("   1. Connect to the KNX/IP gateway");
    info!("   2. Monitor KNX bus for telegrams on configured addresses");
    info!("   3. Control light on 1/0/6 when USER button is pressed");
    info!("   4. Log all KNX activity in real-time");
    info!("");
    info!("   Trigger events by:");
    info!("   - Pressing USER button (blue) to toggle light (1/0/6)");
    info!("   - Pressing physical KNX switches");
    info!("   - Sending telegrams via ETS");
    info!("");
    info!("   Press Reset button to restart.\n");

    static DB_CELL: StaticCell<aimdb_core::AimDb<EmbassyAdapter>> = StaticCell::new();
    let _db = DB_CELL.init(builder.build().await.expect("Failed to build database"));

    info!("✅ Database running with background services");
    info!("   - light_monitor (consumes LightState from KNX)");
    info!("   - temperature_monitor (consumes Temperature from KNX)");
    info!("   - button_handler (produces LightControl to KNX)");
    info!("   - KNX connector (handles bus communication)\n");

    // Main loop - blink LED to show system is alive
    // All services run in the background
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(100)).await;
        led.set_low();
        Timer::after(Duration::from_millis(900)).await;
    }
}
