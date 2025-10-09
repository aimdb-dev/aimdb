#![no_std]
#![no_main]

//! Example demonstrating full AimDB service integration with unified API

use aimdb_core::{Database, RuntimeContext};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyDatabaseBuilder};
use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// Plain Embassy tasks - no #[service] macro!
// These wrap the shared implementations for clean separation

#[embassy_executor::task]
async fn data_processor_task(adapter: &'static EmbassyAdapter) {
    let ctx = RuntimeContext::new(adapter);
    if let Err(e) = aimdb_examples_shared::data_processor_service(ctx).await {
        error!("Data processor error: {:?}", defmt::Debug2Format(&e));
    }
}

#[embassy_executor::task]
async fn monitoring_task(adapter: &'static EmbassyAdapter) {
    let ctx = RuntimeContext::new(adapter);
    if let Err(e) = aimdb_examples_shared::monitoring_service(ctx).await {
        error!("Monitoring service error: {:?}", defmt::Debug2Format(&e));
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize heap for the allocator
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 8192;
        static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe {
            let heap_ptr = core::ptr::addr_of_mut!(HEAP);
            ALLOCATOR.init((*heap_ptr).as_ptr() as usize, HEAP_SIZE)
        }
    }

    let p = embassy_stm32::init(Default::default());
    info!("🔧 Setting up AimDB with Embassy runtime...");

    // Create database using the new unified builder API
    // Use StaticCell to make db live for 'static
    static DB_CELL: StaticCell<aimdb_embassy_adapter::EmbassyDatabase> = StaticCell::new();
    let db = DB_CELL.init(
        Database::<EmbassyAdapter>::builder()
            .record("sensors")
            .record("metrics")
            .build(spawner),
    );

    info!("✅ AimDB database created successfully");

    // Setup LED for visual feedback
    let mut led = Output::new(p.PB0, Level::High, Speed::Low);

    info!("🎯 Spawning services with unified API:");

    let adapter_ref = db.adapter();

    // Spawn services using Embassy tasks
    spawner.spawn(data_processor_task(adapter_ref).unwrap());
    spawner.spawn(monitoring_task(adapter_ref).unwrap());

    info!("⚡ Services spawned successfully!");

    // Blink LED while services run
    for i in 0..10 {
        info!("LED blink {}/10", i + 1);
        led.set_high();
        Timer::after(Duration::from_millis(250)).await;

        led.set_low();
        Timer::after(Duration::from_millis(250)).await;
    }

    info!("🎉 All services completed successfully!");

    // Keep LED on to signal completion
    led.set_high();

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
