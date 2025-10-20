//! Example demonstrating AimDB buffer integration with Tokio runtime
//!
//! This demo showcases all three buffer types:
//! 1. SPMC Ring Buffer - high-frequency telemetry with multiple consumers
//! 2. SingleLatest - configuration updates with intermediate skipping
//! 3. Mailbox - command processing with overwrite semantics

use aimdb_core::{
    buffer::{Buffer, BufferCfg},
    Database, DbResult,
};
use aimdb_examples_shared::*;
use aimdb_tokio_adapter::{TokioAdapter, TokioBuffer, TokioDatabaseBuilder};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> DbResult<()> {
    println!("🔧 Setting up AimDB with Tokio runtime...");
    println!("📦 Demonstrating all three buffer types:\n");

    // Create database
    let db = Database::<TokioAdapter>::builder().build()?;
    println!("✅ AimDB database created successfully\n");

    // Get runtime context for services
    let ctx = db.context();

    // ========================================================================
    // DEMO 1: SPMC Ring Buffer - High-frequency telemetry
    // ========================================================================
    println!("📡 DEMO 1: SPMC Ring Buffer (Telemetry)");
    println!("   - Multiple consumers reading at different speeds");
    println!("   - Lag detection when consumers fall behind");
    println!("   - Capacity: 10 messages\n");

    // Create telemetry buffer with SPMC ring (capacity 10)
    let telemetry_buffer = Arc::new(TokioBuffer::<TelemetryReading>::new(&BufferCfg::SpmcRing {
        capacity: 10,
    }));

    // Subscribe consumers before producing
    let consumer1_reader = telemetry_buffer.subscribe();
    let consumer2_reader = telemetry_buffer.subscribe();

    // Spawn producer that writes to buffer
    let ctx_producer = ctx.clone();
    let telemetry_buf_producer = telemetry_buffer.clone();
    db.spawn(async move {
        let log = ctx_producer.log();
        let time = ctx_producer.time();
        log.info("📡 Telemetry producer starting");

        for i in 0..10u32 {
            let reading = TelemetryReading {
                sensor_id: 1,
                temperature: 2300 + (i as i32 % 500),
                humidity: 5000 + (i % 2000),
            };

            // Push to buffer (synchronous operation)
            telemetry_buf_producer.push(reading.clone());

            println!(
                "📊 Telemetry #{}: temp={}.{:02}°C humidity={}.{:02}%",
                i,
                reading.temperature / 100,
                reading.temperature % 100,
                reading.humidity / 100,
                reading.humidity % 100
            );

            time.sleep(time.millis(50)).await;
        }
        log.info("📡 Telemetry producer completed");
    })?;

    // Spawn consumers using the shared consumer service
    let ctx1 = ctx.clone();
    db.spawn(async move {
        if let Err(e) = telemetry_consumer_service(ctx1, consumer1_reader, 1).await {
            eprintln!("Consumer 1 error: {:?}", e);
        }
    })?;

    let ctx2 = ctx.clone();
    db.spawn(async move {
        if let Err(e) = telemetry_consumer_service(ctx2, consumer2_reader, 2).await {
            eprintln!("Consumer 2 error: {:?}", e);
        }
    })?;

    tokio::time::sleep(Duration::from_millis(800)).await;

    // ========================================================================
    // DEMO 2: SingleLatest - Configuration updates
    // ========================================================================
    println!("\n⚙️  DEMO 2: SingleLatest Buffer (Configuration)");
    println!("   - Only latest value matters");
    println!("   - Intermediate updates automatically skipped");
    println!("   - Fast producer, slow consumer\n");

    let config_buffer = Arc::new(TokioBuffer::<ConfigUpdate>::new(&BufferCfg::SingleLatest));

    let config_reader = config_buffer.subscribe();

    // Spawn config producer that writes to buffer
    let ctx_config_prod = ctx.clone();
    let config_buf_prod = config_buffer.clone();
    db.spawn(async move {
        let log = ctx_config_prod.log();
        let time = ctx_config_prod.time();
        log.info("⚙️  Config producer starting");

        for version in 1..=10u32 {
            let config = ConfigUpdate {
                version,
                sampling_rate_hz: 10 * version,
                enable_alerts: version % 2 == 0,
            };

            // Push to buffer (synchronous operation)
            config_buf_prod.push(config.clone());

            println!(
                "📝 Config v{}: rate={}Hz alerts={}",
                config.version, config.sampling_rate_hz, config.enable_alerts
            );

            // Rapid updates to demonstrate skipping
            time.sleep(time.millis(30)).await;
        }
        log.info("⚙️  Config producer completed");
    })?;

    // Spawn config consumer
    let ctx_config_cons = ctx.clone();
    db.spawn(async move {
        if let Err(e) = config_consumer_service(ctx_config_cons, config_reader).await {
            eprintln!("Config consumer error: {:?}", e);
        }
    })?;

    tokio::time::sleep(Duration::from_millis(600)).await;

    // ========================================================================
    // DEMO 3: Mailbox - Command processing
    // ========================================================================
    println!("\n🎮 DEMO 3: Mailbox Buffer (Commands)");
    println!("   - Single-slot capacity");
    println!("   - Old unread commands overwritten");
    println!("   - Latest command wins\n");

    let command_buffer = Arc::new(TokioBuffer::<DeviceCommand>::new(&BufferCfg::Mailbox));

    let command_reader = command_buffer.subscribe();

    // Spawn command producer that writes to buffer
    let ctx_cmd_prod = ctx.clone();
    let cmd_buf_prod = command_buffer.clone();
    db.spawn(async move {
        let log = ctx_cmd_prod.log();
        let time = ctx_cmd_prod.time();
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

            // Push to buffer (synchronous operation)
            cmd_buf_prod.push(cmd.clone());

            println!("🎯 Command #{}: {:?}", cmd.command_id, cmd.action);

            // Send commands faster than they can be processed
            time.sleep(time.millis(20)).await;
        }
        log.info("🎮 Command producer completed");
    })?;

    // Spawn command consumer
    let ctx_cmd_cons = ctx.clone();
    db.spawn(async move {
        if let Err(e) = command_consumer_service(ctx_cmd_cons, command_reader).await {
            eprintln!("Command consumer error: {:?}", e);
        }
    })?;

    tokio::time::sleep(Duration::from_millis(600)).await;

    println!("\n🎉 All buffer demonstrations completed successfully!");
    println!("\n📊 Summary:");
    println!("   ✅ SPMC Ring: Multiple consumers with lag detection");
    println!("   ✅ SingleLatest: Automatic intermediate value skipping");
    println!("   ✅ Mailbox: Overwrite semantics for commands");
    println!("   ✅ Runtime-agnostic services work seamlessly!\n");

    Ok(())
}
