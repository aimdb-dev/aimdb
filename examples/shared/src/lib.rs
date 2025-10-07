//! Shared service implementations for AimDB examples
//!
//! These services are runtime-agnostic and can be used with any Runtime implementation
//! (TokioAdapter, EmbassyAdapter, etc.)
//!
//! These are plain async functions that can be wrapped with the #[service] macro
//! in the consuming crate.

#![cfg_attr(not(feature = "std"), no_std)]

use aimdb_core::{DbResult, RuntimeContext};
use aimdb_executor::Runtime;

#[cfg(not(feature = "std"))]
use core::time::Duration;
#[cfg(feature = "std")]
use std::time::Duration;

/// Background data processing service
///
/// Demonstrates runtime-agnostic service that processes data in batches.
/// Generic over any Runtime implementation.
pub async fn data_processor_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    ctx.info("🚀 Data processor service started");

    for i in 1..=5 {
        match i {
            1 => ctx.info("📊 Processing batch 1/5"),
            2 => ctx.info("📊 Processing batch 2/5"),
            3 => ctx.info("📊 Processing batch 3/5"),
            4 => ctx.info("📊 Processing batch 4/5"),
            5 => ctx.info("📊 Processing batch 5/5"),
            _ => {}
        }

        // Use the runtime context's sleep capability
        ctx.sleep(Duration::from_millis(200)).await;

        match i {
            1 => ctx.info("✅ Batch 1 completed"),
            2 => ctx.info("✅ Batch 2 completed"),
            3 => ctx.info("✅ Batch 3 completed"),
            4 => ctx.info("✅ Batch 4 completed"),
            5 => ctx.info("✅ Batch 5 completed"),
            _ => {}
        }
    }

    ctx.info("🏁 Data processor service completed");

    Ok(())
}

/// Monitoring and health check service
///
/// Demonstrates runtime-agnostic service that performs periodic health checks.
/// Measures timing using the runtime context.
pub async fn monitoring_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    ctx.info("📈 Monitoring service started");

    for i in 1..=3 {
        let start_time = ctx.now();

        match i {
            1 => ctx.info("🔍 Health check 1/3"),
            2 => ctx.info("🔍 Health check 2/3"),
            3 => ctx.info("🔍 Health check 3/3"),
            _ => {}
        }

        // Use the runtime context's sleep capability
        ctx.sleep(Duration::from_millis(150)).await;

        let end_time = ctx.now();
        let _duration = ctx.duration_since(end_time, start_time).unwrap();

        ctx.info("💚 System healthy");
    }

    ctx.info("📈 Monitoring service completed");

    Ok(())
}
