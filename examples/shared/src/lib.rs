//! Shared service implementations for AimDB examples
//!
//! These services are runtime-agnostic and can be used with any Runtime implementation
//! (TokioAdapter, EmbassyAdapter, etc.)
//!

#![cfg_attr(not(feature = "std"), no_std)]

use aimdb_core::{DbResult, RuntimeContext};
use aimdb_executor::Runtime;

/// Background data processing service
///
/// Demonstrates runtime-agnostic service that processes data in batches.
/// Generic over any Runtime implementation.
///
/// This service demonstrates the clean accessor API:
/// - Store accessors at the beginning: `let log = ctx.log(); let time = ctx.time();`
/// - Use them throughout the service for clean, efficient code
pub async fn data_processor_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    // Store accessors for reuse throughout the service
    let log = ctx.log();
    let time = ctx.time();

    log.info("🚀 Data processor service started");

    for i in 1..=5 {
        match i {
            1 => log.info("📊 Processing batch 1/5"),
            2 => log.info("📊 Processing batch 2/5"),
            3 => log.info("📊 Processing batch 3/5"),
            4 => log.info("📊 Processing batch 4/5"),
            5 => log.info("📊 Processing batch 5/5"),
            _ => {}
        }

        // Clean time operations using stored accessor
        time.sleep(time.millis(200)).await;

        match i {
            1 => log.info("✅ Batch 1 completed"),
            2 => log.info("✅ Batch 2 completed"),
            3 => log.info("✅ Batch 3 completed"),
            4 => log.info("✅ Batch 4 completed"),
            5 => log.info("✅ Batch 5 completed"),
            _ => {}
        }
    }

    log.info("🏁 Data processor service completed");

    Ok(())
}

/// Monitoring and health check service
///
/// Demonstrates runtime-agnostic service that performs periodic health checks.
/// Measures timing using the runtime context.
///
/// This service demonstrates the clean accessor API with timing measurements.
/// Accessors are stored once and reused throughout the service.
pub async fn monitoring_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    // Store accessors at the beginning for clean, efficient code
    let log = ctx.log();
    let time = ctx.time();

    log.info("📈 Monitoring service started");

    for i in 1..=3 {
        let start_time = time.now();

        match i {
            1 => log.info("🔍 Health check 1/3"),
            2 => log.info("🔍 Health check 2/3"),
            3 => log.info("🔍 Health check 3/3"),
            _ => {}
        }

        // Clean time operations using stored accessor
        time.sleep(time.millis(150)).await;

        let end_time = time.now();
        let _duration = time.duration_since(end_time, start_time).unwrap();

        log.info("💚 System healthy");
    }

    log.info("📈 Monitoring service completed");

    Ok(())
}
