#![no_std]
#![no_main]

//! Example demonstrating full AimDB service integration with Embassy runtime

use aimdb_core::{DatabaseSpec, RuntimeContext, service};
// Note: DbResult appears unused because the #[service] macro rewrites it to aimdb_core::DbResult
// but it's needed for the source code to be readable
#[allow(unused_imports)]
use aimdb_core::DbResult;
use aimdb_embassy_adapter::{EmbassyAdapter, new_database};
use aimdb_executor::Runtime;
use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

// Wrap shared service implementations with #[service] macro for adapter-specific spawning
#[service]
async fn data_processor_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    aimdb_examples_shared::data_processor_service(ctx).await
}

#[service]
async fn monitoring_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    aimdb_examples_shared::monitoring_service(ctx).await
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

    // Create database with Embassy runtime (spawner goes first)
    // Use StaticCell to make db live for 'static
    static DB_CELL: StaticCell<aimdb_embassy_adapter::EmbassyDatabase> = StaticCell::new();
    let spec = DatabaseSpec::<EmbassyAdapter>::builder().build();
    let db = DB_CELL.init(new_database(spawner, spec));

    info!("✅ AimDB database created successfully");

    // Setup LED for visual feedback
    let mut led = Output::new(p.PB0, Level::High, Speed::Low);

    info!("🎯 Spawning services via EmbassyAdapter:");

    let adapter = db.adapter();

    // Spawn services using the new clean API (spawner is already captured by adapter)
    let _handle1 = DataProcessorService::spawn_embassy(adapter).unwrap();
    let _handle2 = MonitoringService::spawn_embassy(adapter).unwrap();

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
    info!("🔍 Service names:");
    info!("  - {}", DataProcessorService::service_name());
    info!("  - {}", MonitoringService::service_name());

    // Keep LED on to signal completion
    led.set_high();

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
