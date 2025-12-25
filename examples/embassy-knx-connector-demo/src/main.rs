#![no_std]
#![no_main]

//! KNX Connector Demo for Embassy Runtime
//!
//! Demonstrates bidirectional KNX/IP integration on embedded hardware with AimDB:
//! - Multiple temperature sensors publishing to different records
//! - Multiple light monitors receiving from different addresses
//! - Outbound light control via button press
//!
//! ## Shared Code
//!
//! This demo uses `knx-connector-demo-common` for data types and monitors,
//! demonstrating AimDB's "write once, run anywhere" capability. The same
//! business logic runs on MCU (Embassy), edge (Tokio), and cloud (Tokio).
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

extern crate alloc;

use aimdb_core::{AimDbBuilder, RecordKey, RuntimeContext};
use aimdb_embassy_adapter::{
    EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExt, EmbassyRecordRegistrarExtCustom,
};
use aimdb_knx_connector::dpt::{Dpt1, Dpt9, DptDecode, DptEncode};
use aimdb_knx_connector::embassy_client::KnxConnectorBuilder;
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue};
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::peripherals::ETH;
use embassy_stm32::rng::Rng;
use embassy_stm32::{Config, bind_interrupts, eth, interrupt, peripherals, rng};
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// Import shared types, monitors, and keys from common crate
use knx_connector_demo_common::{
    LightControl, LightControlKey, LightKey, LightState, TemperatureKey, TemperatureReading,
    light_monitor, temperature_monitor,
};

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// Interrupt bindings for Ethernet, RNG, and EXTI
bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
    EXTI13 => exti::InterruptHandler<interrupt::typelevel::EXTI13>;
});

type Device =
    Ethernet<'static, ETH, GenericPhy<embassy_stm32::eth::Sma<'static, peripherals::ETH_SMA>>>;

/// Network task that runs the embassy-net stack
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
    runner.run().await
}

// ============================================================================
// BUTTON HANDLER (platform-specific: GPIO only on embedded)
// ============================================================================

/// Button handler that toggles light on button press
async fn button_handler(
    ctx: RuntimeContext<EmbassyAdapter>,
    producer: aimdb_core::Producer<LightControl, EmbassyAdapter>,
    mut button: ExtiInput<'static>,
) {
    let log = ctx.log();
    let time = ctx.time();

    log.info("🔘 Button handler started - press USER button to toggle light\n");

    let mut light_on = false;

    loop {
        button.wait_for_falling_edge().await;

        // Debounce
        time.sleep(time.millis(50)).await;
        if button.is_high() {
            continue;
        }

        light_on = !light_on;

        let state = LightControl::new("1/0/6", light_on);

        match producer.produce(state).await {
            Ok(_) => {
                log.info(&alloc::format!(
                    "✅ Published to KNX: 1/0/6 = {}",
                    if light_on { "ON ✨" } else { "OFF" }
                ));
            }
            Err(e) => {
                log.error(&alloc::format!("❌ Failed to publish: {:?}", e));
            }
        }

        button.wait_for_rising_edge().await;
        time.sleep(time.millis(50)).await;
    }
}

// ============================================================================
// KNX CONFIGURATION
// ============================================================================

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
    info!("   Using shared types from knx-connector-demo-common");

    // Configure MCU clocks for STM32H563ZI
    let mut config = Config::default();
    {
        use embassy_stm32::rcc::*;
        use embassy_stm32::time::Hertz;

        config.rcc.hsi = None;
        config.rcc.hsi48 = Some(Default::default());
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

    let mut led = Output::new(p.PB0, Level::Low, Speed::Low);
    let button = ExtiInput::new(p.PC13, p.EXTI13, Pull::Down, Irqs);

    let mut rng = Rng::new(p.RNG, Irqs);
    let mut seed = [0; 8];
    rng.fill_bytes(&mut seed);
    let seed = u64::from_le_bytes(seed);

    info!("🔧 Initializing Ethernet...");

    let mac_addr = [0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF];

    static PACKETS: StaticCell<PacketQueue<4, 4>> = StaticCell::new();

    let device = Ethernet::new(
        PACKETS.init(PacketQueue::<4, 4>::new()),
        p.ETH,
        Irqs,
        p.PA1,
        p.PA7,
        p.PC4,
        p.PC5,
        p.PG13,
        p.PB15,
        p.PG11,
        mac_addr,
        p.ETH_SMA,
        p.PA2,
        p.PC1,
    );

    let config = embassy_net::Config::dhcpv4(Default::default());

    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    static STACK_CELL: StaticCell<embassy_net::Stack<'static>> = StaticCell::new();

    let (stack_obj, runner) =
        embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

    let stack: &'static _ = STACK_CELL.init(stack_obj);

    spawner.spawn(unwrap!(net_task(runner)));

    info!("⏳ Waiting for network configuration (DHCP)...");

    stack.wait_config_up().await;

    info!("✅ Network ready!");
    if let Some(config) = stack.config_v4() {
        info!("   IP address: {}", config.address);
    }

    for _ in 0..3 {
        led.set_high();
        Timer::after(Duration::from_millis(100)).await;
        led.set_low();
        Timer::after(Duration::from_millis(100)).await;
    }

    info!("🔌 Initializing KNX client...");

    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new_with_network(spawner, stack));

    use alloc::format;
    let gateway_url = format!("knx://{}:{}", KNX_GATEWAY_IP, KNX_GATEWAY_PORT);

    let mut builder = AimDbBuilder::new()
        .runtime(runtime.clone())
        .with_connector(KnxConnectorBuilder::new(&gateway_url));

    // ========================================================================
    // TEMPERATURE SENSORS (inbound: KNX → AimDB)
    // Using shared types and monitors from knx-connector-demo-common
    // ========================================================================

    builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .tap(temperature_monitor)
            .link_from(TemperatureKey::LivingRoom.link_address().unwrap())
            .with_deserializer(|data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(TemperatureReading::new("Living Room", celsius))
            })
            .finish();
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Bedroom, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .tap(temperature_monitor)
            .link_from(TemperatureKey::Bedroom.link_address().unwrap())
            .with_deserializer(|data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(TemperatureReading::new("Bedroom", celsius))
            })
            .finish();
    });

    builder.configure::<TemperatureReading>(TemperatureKey::Kitchen, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .tap(temperature_monitor)
            .link_from(TemperatureKey::Kitchen.link_address().unwrap())
            .with_deserializer(|data: &[u8]| {
                let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
                Ok(TemperatureReading::new("Kitchen", celsius))
            })
            .finish();
    });

    // ========================================================================
    // LIGHT MONITORS (inbound: KNX → AimDB)
    // Using shared types and monitors from knx-connector-demo-common
    // ========================================================================

    builder.configure::<LightState>(LightKey::Main, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .tap(light_monitor)
            .link_from(LightKey::Main.link_address().unwrap())
            .with_deserializer(|data: &[u8]| {
                let is_on = Dpt1::Switch.decode(data).unwrap_or(false);
                Ok(LightState::new("1/0/7", is_on))
            })
            .finish();
    });

    builder.configure::<LightState>(LightKey::Hallway, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .tap(light_monitor)
            .link_from(LightKey::Hallway.link_address().unwrap())
            .with_deserializer(|data: &[u8]| {
                let is_on = Dpt1::Switch.decode(data).unwrap_or(false);
                Ok(LightState::new("1/0/8", is_on))
            })
            .finish();
    });

    // ========================================================================
    // LIGHT CONTROL (outbound: AimDB → KNX)
    // Using shared LightControl type from knx-connector-demo-common
    // ========================================================================

    builder.configure::<LightControl>(LightControlKey::Control, |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .source_with_context(button, button_handler)
            .link_to(LightControlKey::Control.link_address().unwrap())
            .with_serializer(|state: &LightControl| {
                let mut buf = [0u8; 1];
                let len = Dpt1::Switch.encode(state.is_on, &mut buf).unwrap_or(0);
                Ok(buf[..len].to_vec())
            })
            .finish();
    });

    info!("✅ Database configured:");
    info!("   INBOUND (KNX → AimDB):");
    info!("     - temp.livingroom  (9/1/0)");
    info!("     - temp.bedroom     (9/0/1)");
    info!("     - temp.kitchen     (9/1/2)");
    info!("     - lights.main      (1/0/7)");
    info!("     - lights.hallway   (1/0/8)");
    info!("   OUTBOUND (AimDB → KNX):");
    info!("     - lights.control   (1/0/6)");
    info!("   Gateway: {}:{}", KNX_GATEWAY_IP, KNX_GATEWAY_PORT);
    info!("");
    info!("Press USER button to toggle light (1/0/6)");

    static DB_CELL: StaticCell<aimdb_core::AimDb<EmbassyAdapter>> = StaticCell::new();
    let _db = DB_CELL.init(builder.build().await.expect("Failed to build database"));

    info!("✅ Database running");

    // Main loop - blink LED to show system is alive
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(100)).await;
        led.set_low();
        Timer::after(Duration::from_millis(900)).await;
    }
}
