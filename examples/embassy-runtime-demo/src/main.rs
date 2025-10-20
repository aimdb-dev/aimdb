#![no_std]
#![no_main]

//! Example demonstrating AimDB buffer integration with Embassy runtime
//!
//! This demo showcases all three buffer types in a no_std embedded environment:
//! 1. SPMC Ring Buffer - high-frequency telemetry with multiple consumers
//! 2. SingleLatest - configuration updates with intermediate skipping
//! 3. Mailbox - command processing with overwrite semantics

extern crate alloc;

use aimdb_core::{Database, RuntimeContext, buffer::Buffer};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBuffer, EmbassyDatabaseBuilder};
use aimdb_examples_shared::{CommandAction, ConfigUpdate, DeviceCommand, TelemetryReading};
use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

// Simple embedded allocator (required by some dependencies)
#[global_allocator]
static ALLOCATOR: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

//
// ============================================================================
// BUFFER DEMONSTRATION TASKS
// ============================================================================
//

// Type alias for telemetry buffer: 10 capacity, 4 subs, 1 pub, 4 watch receivers
type TelemetryBuffer = EmbassyBuffer<TelemetryReading, 10, 4, 1, 4>;

#[embassy_executor::task]
async fn telemetry_producer_task(
    buffer: &'static TelemetryBuffer,
    adapter: &'static EmbassyAdapter,
) {
    let ctx = RuntimeContext::new(adapter);
    let log = ctx.log();
    let time = ctx.time();

    log.info("📡 Telemetry producer starting");

    for i in 0..10u32 {
        let reading = TelemetryReading {
            sensor_id: 1,
            temperature: 2300 + (i as i32 % 500),
            humidity: 5000 + (i % 2000),
        };

        buffer.push(reading);
        log.info("📊 Telemetry reading produced");

        time.sleep(time.millis(50)).await;
    }

    log.info("📡 Telemetry producer completed");
}

#[embassy_executor::task(pool_size = 2)]
async fn telemetry_consumer_task(
    buffer: &'static TelemetryBuffer,
    adapter: &'static EmbassyAdapter,
    consumer_id: u32,
) {
    let ctx = RuntimeContext::new(adapter);
    let reader = buffer.subscribe();

    if let Err(e) =
        aimdb_examples_shared::telemetry_consumer_service(ctx, reader, consumer_id).await
    {
        error!(
            "Consumer {} error: {:?}",
            consumer_id,
            defmt::Debug2Format(&e)
        );
    }
}

// Type alias for config buffer: 1 capacity (not used for Watch), 4 subs, 1 pub, 4 watch receivers
type ConfigBuffer = EmbassyBuffer<ConfigUpdate, 1, 4, 1, 4>;

#[embassy_executor::task]
async fn config_producer_task(buffer: &'static ConfigBuffer, adapter: &'static EmbassyAdapter) {
    let ctx = RuntimeContext::new(adapter);
    let log = ctx.log();
    let time = ctx.time();

    log.info("⚙️  Config producer starting");

    for version in 1..=10u32 {
        let config = ConfigUpdate {
            version,
            sampling_rate_hz: 10 * version,
            enable_alerts: version % 2 == 0,
        };

        buffer.push(config);
        log.info("📝 Config update published");

        // Rapid updates to demonstrate skipping
        time.sleep(time.millis(30)).await;
    }

    log.info("⚙️  Config producer completed");
}

#[embassy_executor::task]
async fn config_consumer_task(buffer: &'static ConfigBuffer, adapter: &'static EmbassyAdapter) {
    let ctx = RuntimeContext::new(adapter);
    let reader = buffer.subscribe();

    if let Err(e) = aimdb_examples_shared::config_consumer_service(ctx, reader).await {
        error!("Config consumer error: {:?}", defmt::Debug2Format(&e));
    }
}

// Type alias for command buffer: 1 capacity (mailbox), 4 subs, 1 pub, 4 watch receivers
type CommandBuffer = EmbassyBuffer<DeviceCommand, 1, 4, 1, 4>;

#[embassy_executor::task]
async fn command_producer_task(buffer: &'static CommandBuffer, adapter: &'static EmbassyAdapter) {
    let ctx = RuntimeContext::new(adapter);
    let log = ctx.log();
    let time = ctx.time();

    log.info("🎮 Command producer starting");

    let actions = [
        CommandAction::StartSampling,
        CommandAction::StopSampling,
        CommandAction::Calibrate,
        CommandAction::Reset,
    ];

    for i in 0..10u32 {
        let cmd = DeviceCommand {
            command_id: i,
            action: actions[(i % 4) as usize].clone(),
        };

        buffer.push(cmd);
        log.info("🎯 Command sent");

        // Send commands faster than they can be processed
        time.sleep(time.millis(20)).await;
    }

    log.info("🎮 Command producer completed");
}

#[embassy_executor::task]
async fn command_consumer_task(buffer: &'static CommandBuffer, adapter: &'static EmbassyAdapter) {
    let ctx = RuntimeContext::new(adapter);
    let reader = buffer.subscribe();

    if let Err(e) = aimdb_examples_shared::command_consumer_service(ctx, reader).await {
        error!("Command consumer error: {:?}", defmt::Debug2Format(&e));
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize heap for the allocator
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 16384; // Increased for buffer demos
        static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe {
            let heap_ptr = core::ptr::addr_of_mut!(HEAP);
            ALLOCATOR.init((*heap_ptr).as_ptr() as usize, HEAP_SIZE)
        }
    }

    let p = embassy_stm32::init(Default::default());
    info!("🔧 Setting up AimDB with Embassy runtime...");
    info!("📦 Demonstrating all three buffer types:");

    // Create database using the new unified builder API
    static DB_CELL: StaticCell<aimdb_embassy_adapter::EmbassyDatabase> = StaticCell::new();
    let db = DB_CELL.init(Database::<EmbassyAdapter>::builder().build(spawner));

    info!("✅ AimDB database created successfully");

    let adapter_ref = db.adapter();

    // Setup LED for visual feedback
    let mut led = Output::new(p.PB0, Level::High, Speed::Low);

    // ========================================================================
    // DEMO 1: SPMC Ring Buffer - High-frequency telemetry
    // ========================================================================
    info!("📡 DEMO 1: SPMC Ring Buffer (Telemetry)");

    static TELEMETRY_BUFFER: StaticCell<TelemetryBuffer> = StaticCell::new();
    let telemetry_buffer = TELEMETRY_BUFFER.init(TelemetryBuffer::new_spmc());

    spawner.spawn(telemetry_producer_task(telemetry_buffer, adapter_ref).expect("spawn failed"));
    spawner.spawn(telemetry_consumer_task(telemetry_buffer, adapter_ref, 1).expect("spawn failed"));
    spawner.spawn(telemetry_consumer_task(telemetry_buffer, adapter_ref, 2).expect("spawn failed"));

    // Let telemetry demo run
    for _ in 0..4 {
        led.toggle();
        Timer::after(Duration::from_millis(200)).await;
    }

    // ========================================================================
    // DEMO 2: SingleLatest - Configuration updates
    // ========================================================================
    info!("⚙️  DEMO 2: SingleLatest Buffer (Configuration)");

    static CONFIG_BUFFER: StaticCell<ConfigBuffer> = StaticCell::new();
    let config_buffer = CONFIG_BUFFER.init(ConfigBuffer::new_watch());

    spawner.spawn(config_producer_task(config_buffer, adapter_ref).expect("spawn failed"));
    spawner.spawn(config_consumer_task(config_buffer, adapter_ref).expect("spawn failed"));

    // Let config demo run
    for _ in 0..3 {
        led.toggle();
        Timer::after(Duration::from_millis(200)).await;
    }

    // ========================================================================
    // DEMO 3: Mailbox - Command processing
    // ========================================================================
    info!("🎮 DEMO 3: Mailbox Buffer (Commands)");

    static COMMAND_BUFFER: StaticCell<CommandBuffer> = StaticCell::new();
    let command_buffer = COMMAND_BUFFER.init(CommandBuffer::new_mailbox());

    spawner.spawn(command_producer_task(command_buffer, adapter_ref).expect("spawn failed"));
    spawner.spawn(command_consumer_task(command_buffer, adapter_ref).expect("spawn failed"));

    // Let command demo run
    for _ in 0..3 {
        led.toggle();
        Timer::after(Duration::from_millis(200)).await;
    }

    info!("🎉 All buffer demonstrations completed!");
    info!("📊 Summary:");
    info!("   ✅ SPMC Ring: Multiple consumers with lag detection");
    info!("   ✅ SingleLatest: Automatic intermediate value skipping");
    info!("   ✅ Mailbox: Overwrite semantics for commands");
    info!("   ✅ Runtime-agnostic services work in no_std!");

    // Keep LED on to signal completion
    led.set_high();

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
