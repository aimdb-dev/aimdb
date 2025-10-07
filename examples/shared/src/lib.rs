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
    #[cfg(feature = "std")]
    println!("🚀 Data processor service started at: {:?}", ctx.now());
    #[cfg(not(feature = "std"))]
    defmt::info!("🚀 Data processor service started");

    for i in 1..=5 {
        #[cfg(feature = "std")]
        println!("📊 Processing batch {}/5", i);
        #[cfg(not(feature = "std"))]
        defmt::info!("📊 Processing batch {}/5", i);

        // Use the runtime context's sleep capability
        ctx.sleep(Duration::from_millis(200)).await;

        #[cfg(feature = "std")]
        println!("✅ Batch {} completed", i);
        #[cfg(not(feature = "std"))]
        defmt::info!("✅ Batch {} completed", i);
    }

    #[cfg(feature = "std")]
    println!("🏁 Data processor service completed at: {:?}", ctx.now());
    #[cfg(not(feature = "std"))]
    defmt::info!("🏁 Data processor service completed");

    Ok(())
}

/// Monitoring and health check service
///
/// Demonstrates runtime-agnostic service that performs periodic health checks.
/// Measures timing using the runtime context.
pub async fn monitoring_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    #[cfg(feature = "std")]
    println!("📈 Monitoring service started at: {:?}", ctx.now());
    #[cfg(not(feature = "std"))]
    defmt::info!("📈 Monitoring service started");

    for i in 1..=3 {
        let start_time = ctx.now();

        #[cfg(feature = "std")]
        println!("🔍 Health check {}/3", i);
        #[cfg(not(feature = "std"))]
        defmt::info!("🔍 Health check {}/3", i);

        // Use the runtime context's sleep capability
        ctx.sleep(Duration::from_millis(150)).await;

        let end_time = ctx.now();
        let duration = ctx.duration_since(end_time, start_time).unwrap();

        #[cfg(feature = "std")]
        println!("💚 System healthy (check took: {:?})", duration);
        #[cfg(not(feature = "std"))]
        defmt::info!("💚 System healthy (check took: {} ticks)", duration);
    }

    #[cfg(feature = "std")]
    println!("📈 Monitoring service completed at: {:?}", ctx.now());
    #[cfg(not(feature = "std"))]
    defmt::info!("📈 Monitoring service completed");

    Ok(())
}
