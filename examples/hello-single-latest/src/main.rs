//! # AimDB SingleLatest Sync Demo
//! This example demonstrates the `SingleLatest` buffer primitive using AimDB's synchronous API.
//!
//! ## What This Demo Shows
//!
//! 1. Building an AimDB instance with a typed record and 2 tap observers (slow and fast)
//! 2. Attaching the database to get a sync handle
//! 3. Creating a producer in sync context
//! 4. Setting values using blocking operations
//! 5. Clean shutdown with detach()

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, Consumer, RuntimeContext};
use aimdb_sync::AimDbBuilderSyncExt; // Extension trait for .attach()
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt}; // Extension for .buffer()
use std::{sync::Arc, thread, time::Duration};

#[derive(Debug, Clone)]
struct FeatureFlag {
    rollout_percent: f32,
}

const KEY: &str = "config.checkout_rollout";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== AimDB SingleLatest Sync Demo ===\n");

    // Step 1: Build the database and attach it for sync API usage
    println!("1. Building database and attaching for sync API...");

    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<FeatureFlag>(KEY, |reg| {
        // Configure a SingleLatest buffer
        reg.buffer(BufferCfg::SingleLatest)
            // Register 2 taps that observe rollout percentage values
            .tap(rollout_observer_fast)
            .tap(rollout_observer_slow);
    });

    // Build happens inside the runtime thread where Tokio context exists
    let handle = builder.attach()?;

    // Step 2: Create a synchronous producer
    println!("2. Creating producer and producing values...");

    let producer = handle.producer::<FeatureFlag>(KEY)?;

    // Step 3: Produce values
    println!("3. Producing rollout percentage values");

    for i in 0..5 {
        let feature_flag = FeatureFlag {
            rollout_percent: i as f32 * 25.0,
        };
        println!(
            "   Main: Setting FeatureFlag {}%",
            feature_flag.rollout_percent
        );

        // Use blocking send
        producer.set(feature_flag)?;

        // Producer publishes new value every second
        if i < 4 {
            thread::sleep(Duration::from_millis(1000));
        }
    }

    // Give the slow tap time to observe the final update before shutdown. Its read cycle is 2000ms, therefore wait a little longer than that.
    thread::sleep(Duration::from_millis(2500));

    // Step 4: Clean shutdown
    // Detach the handle to gracefully shut down the runtime thread

    println!("4. Shutting down...");

    handle.detach()?;

    println!("Done. The sync producer published rollout updates observed by fast and slow SingleLatest taps");

    Ok(())
}

// A fast tap observer
async fn rollout_observer_fast(ctx: RuntimeContext, consumer: Consumer<FeatureFlag>) {
    let mut reader = consumer.subscribe();

    while let Ok(feature_flag) = reader.recv().await {
        ctx.log().info(&format!(
            "[fast] rollout: {}%",
            feature_flag.rollout_percent
        ));
    }
}

// A slow tap observer
async fn rollout_observer_slow(ctx: RuntimeContext, consumer: Consumer<FeatureFlag>) {
    let mut reader = consumer.subscribe();
    let time = ctx.time();

    while let Ok(feature_flag) = reader.recv().await {
        ctx.log().info(&format!(
            "[slow] rollout: {}%",
            feature_flag.rollout_percent
        ));

        time.sleep_millis(2000).await;
    }
}
