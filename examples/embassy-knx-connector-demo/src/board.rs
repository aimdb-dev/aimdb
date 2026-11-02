//! The board: everything that touches the STM32H563ZI Nucleo's hardware.
//!
//! [`init`] brings up the heap, clocks, Ethernet (waiting for DHCP), USART3
//! and the user button, and hands the application only what it needs. Pins
//! never leave this module: inputs come out as owned values in
//! [`NucleoInputs`], so wiring one into two records is a compile error
//! (`use of moved value`).

use aimdb_core::{Input, RecordRegistrar};
use core::fmt::Debug;
use defmt::*;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue};
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::mode::Async;
use embassy_stm32::peripherals::ETH;
use embassy_stm32::rng::Rng;
use embassy_stm32::usart::{BufferedUart, BufferedUartRx, BufferedUartTx, Config as UartConfig};
use embassy_stm32::{Config, bind_interrupts, eth, interrupt, peripherals, rng, usart};
use embassy_time::{Duration, Timer};
use knx_connector_demo_common::ButtonEvent;
use static_cell::StaticCell;

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// Interrupt bindings for Ethernet, RNG, the user button and USART3
bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
    EXTI13 => exti::InterruptHandler<interrupt::typelevel::EXTI13>;
    USART3 => usart::BufferedInterruptHandler<peripherals::USART3>;
});

type Device =
    Ethernet<'static, ETH, GenericPhy<embassy_stm32::eth::Sma<'static, peripherals::ETH_SMA>>>;

/// Network task that runs the embassy-net stack
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
    runner.run().await
}

/// What the application gets from the board.
pub struct Board {
    /// The network stack, configured (DHCP done).
    pub stack: &'static embassy_net::Stack<'static>,
    /// The green user LED (LD1, PB0).
    pub led: Output<'static>,
    /// USART3 (ST-LINK VCP, PD8 = TX / PD9 = RX), 115 200 baud.
    pub serial_tx: BufferedUartTx<'static>,
    /// See [`serial_tx`](Self::serial_tx).
    pub serial_rx: BufferedUartRx<'static>,
    /// The board's inputs, each an owned value.
    pub inputs: NucleoInputs,
}

/// The Nucleo's inputs.
pub struct NucleoInputs {
    /// The blue user button B1.
    pub b1: UserButton,
}

/// The blue user button B1 (PC13, high while pressed). Neither `Clone` nor
/// `Copy`: [`PushButton::on`] takes it by value.
pub struct UserButton(ExtiInput<'static, Async>);

/// Brings up the board and waits for the network.
pub async fn init(spawner: Spawner) -> Board {
    // Initialize heap for the allocator
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 98304; // 96KB heap (KNX + serial AimX server JSON)
        static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe {
            let heap_ptr = core::ptr::addr_of_mut!(HEAP);
            ALLOCATOR.init((*heap_ptr).as_ptr() as usize, HEAP_SIZE)
        }
    }

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
    static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
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

    // USART3 on the ST-LINK VCP: no extra cabling on a Nucleo-H563ZI.
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

    let b1 = UserButton(ExtiInput::new(p.PC13, p.EXTI13, Pull::Down, Irqs));

    Board {
        stack,
        led,
        serial_tx,
        serial_rx,
        inputs: NucleoInputs { b1 },
    }
}

/// A push button input: one [`ButtonEvent`] per press, debounced. An input
/// for every record type built `From<ButtonEvent>`.
pub struct PushButton {
    button: UserButton,
}

impl PushButton {
    /// A push button on B1.
    pub fn on(button: UserButton) -> Self {
        Self { button }
    }
}

impl<T> Input<T> for PushButton
where
    T: From<ButtonEvent> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) {
        let PushButton {
            button: UserButton(mut pin),
        } = self;
        reg.source(move |ctx, producer| async move {
            let time = ctx.time();
            let mut press = 0;
            loop {
                pin.wait_for_rising_edge().await;
                // Debounce: a bounce shorter than this is no press.
                time.sleep_millis(50).await;
                if pin.is_low() {
                    continue;
                }
                press += 1;
                producer.produce(T::from(ButtonEvent { press }));
                pin.wait_for_falling_edge().await;
                time.sleep_millis(50).await;
            }
        });
    }
}
