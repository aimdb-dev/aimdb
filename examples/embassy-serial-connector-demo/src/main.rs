#![no_std]
#![no_main]

//! AimX over serial on an STM32H563ZI Nucleo — the board *serves* the AimX toolset
//! over a UART, so a host can `record.list` / `record.get` it across the wire.
//! This exercises the `no_std` AimX server dispatch (Issue #120) over the real
//! `aimdb-serial-connector` Embassy transport (COBS over `embedded-io-async`).
//!
//! ## Wiring (no extra cabling on a Nucleo-H563ZI)
//!
//! The AimX server rides **USART3 (PD8 = TX, PD9 = RX)**, which on the
//! Nucleo-H563ZI is bridged to the **ST-LINK Virtual COM Port** — the same USB
//! cable you flash/debug with. On the host it appears as `/dev/ttyACM0`. defmt
//! logs stream separately over **RTT (SWD)**, so they don't collide with the data
//! UART.
//!
//! ⚠️ The VCP↔USART routing is board-specific (ST-LINK solder bridges per UM3115).
//! If your board routes the VCP elsewhere, change the `USART3 / PD8 / PD9` line
//! below — or wire a USB-TTL dongle to any free USART and point the host at the
//! resulting `/dev/ttyUSB0`.
//!
//! ## Host side
//!
//! ```bash
//! cargo run -p aimdb-serial-connector --example serial_demo \
//!     --features _test-tokio -- client /dev/ttyACM0 115200
//! ```
//!
//! You should see `record.list` return the `counter` record, then `counter`
//! climbing once per second — values produced on the MCU, read over serial.

extern crate alloc;

use aimdb_core::{AimDbBuilder, Producer};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExtCustom};
use aimdb_serial_connector::embassy_transport::SerialServer;
use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::usart::{BufferedUart, Config as UartConfig};
use embassy_stm32::{Config, bind_interrupts, peripherals, usart};
use embassy_time::{Duration, Timer};
use serde::{Deserialize, Serialize};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

bind_interrupts!(struct Irqs {
    USART3 => usart::BufferedInterruptHandler<peripherals::USART3>;
});

/// The record the board exposes over serial.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Counter {
    value: u64,
}

/// Drive a value into `counter` once per second.
#[embassy_executor::task]
async fn counter_task(producer: Producer<Counter>) {
    let mut value = 0u64;
    loop {
        value += 1;
        producer.produce(Counter { value });
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize the heap for the allocator.
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 32768; // 32 KB
        static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe {
            let heap_ptr = core::ptr::addr_of_mut!(HEAP);
            ALLOCATOR.init((*heap_ptr).as_ptr() as usize, HEAP_SIZE)
        }
    }

    // Clock tree: HSE 8 MHz (from the ST-LINK MCO) → PLL1 → 250 MHz. Same as the
    // other H563 demos in this repo.
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
    info!("AimX serial server booting on STM32H563ZI");

    // USART3 on PD8 (TX) / PD9 (RX) — the ST-LINK Virtual COM Port on the
    // Nucleo-H563ZI. `BufferedUart` because both split halves implement
    // `embedded-io-async` Read/Write, which the serial connector needs.
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
    let (tx, rx) = uart.split();

    // Build the db: one remotely-readable `counter` record, served over the UART.
    let runtime = alloc::sync::Arc::new(EmbassyAdapter::default());
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(SerialServer::new(rx, tx));
    builder.configure::<Counter>("counter", |reg| {
        reg.buffer_sized::<4, 2>(EmbassyBufferType::SingleLatest)
            .with_remote_access();
    });

    static DB_CELL: StaticCell<aimdb_core::AimDb<EmbassyAdapter>> = StaticCell::new();
    let (db, db_runner) = builder.build().await.expect("build db");
    let db = DB_CELL.init(db);

    let producer = db.producer::<Counter>("counter").expect("producer");
    spawner.spawn(unwrap!(counter_task(producer)));

    info!("serving AimX over USART3 / ST-LINK VCP (record: counter) — connect the host now");
    db_runner.run().await;
}
