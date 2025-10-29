//! # AimDB Sync API Demo
//!
//! This example demonstrates the synchronous API wrapper for AimDB.
//!
//! The sync API allows you to use AimDB from non-async contexts such as:
//! - Legacy codebases that don't use async/await
//! - FFI boundaries where async is problematic
//! - Simple scripts that don't need full async runtime
//! - Testing scenarios where blocking is acceptable
//!
//! ## What This Demo Shows
//!
//! 1. Building an AimDB instance with a typed record
//! 2. Attaching the database to get a sync handle
//! 3. Creating a producer in sync context
//! 4. Setting values using blocking operations
//! 5. Clean shutdown with detach()

use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_sync::AimDbBuilderSyncExt; // Extension trait for .attach()
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt}; // Extension for .buffer()
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Temperature sensor data
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Temperature {
    celsius: f32,
    timestamp_ms: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== AimDB Sync API Demo ===\n");

    // Step 1: Build the database and attach it for sync API usage
    println!("1. Building database and attaching for sync API...");

    // Create TokioAdapter (it's just a marker type, no Tokio context needed yet)
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Temperature>(|reg| {
        // Configure a ring buffer with capacity for 10 values
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            // Add a simple consumer that logs values
            .tap(|_ctx, consumer| async move {
                let Ok(mut reader) = consumer.subscribe() else {
                    eprintln!("Failed to subscribe to temperature buffer");
                    return;
                };
                while let Ok(temp) = reader.recv().await {
                    println!("   [Database] Received: {:.1}°C", temp.celsius);
                }
            });
    });

    // Build happens inside the runtime thread where Tokio context exists
    let handle = builder.attach()?;
    println!("   ✓ Database attached successfully\n");

    // Step 2: Create a synchronous producer
    println!("2. Creating producer for Temperature...");
    let producer = handle.producer::<Temperature>()?;
    println!("   ✓ Producer created\n");

    // Step 3: Use the producer from sync context
    println!("3. Producing temperature values...");

    // Clone producer for use in another thread
    let producer_clone = producer.clone();
    let thread_handle = thread::spawn(move || {
        for i in 0..5 {
            let temp = Temperature {
                celsius: 20.0 + i as f32 * 0.5,
                timestamp_ms: i * 1000,
            };
            println!("   Thread: Setting temperature {:.1}°C", temp.celsius);

            // Blocking send - will wait if channel is full
            if let Err(e) = producer_clone.set(temp) {
                eprintln!("   Error setting value: {}", e);
            }

            thread::sleep(Duration::from_millis(100));
        }
    });

    // Also produce from main thread
    thread::sleep(Duration::from_millis(50)); // Offset timing slightly
    for i in 5..10 {
        let temp = Temperature {
            celsius: 20.0 + i as f32 * 0.5,
            timestamp_ms: i * 1000,
        };
        println!("   Main: Setting temperature {:.1}°C", temp.celsius);

        // Use try_set to demonstrate non-blocking operation
        match producer.try_set(temp) {
            Ok(()) => println!("   Main: Sent immediately"),
            Err(e) => println!("   Main: Could not send: {}", e),
        }

        thread::sleep(Duration::from_millis(100));
    }

    // Wait for spawned thread to complete
    thread_handle.join().unwrap();
    println!("   ✓ All values produced\n");

    // Step 4: Demonstrate timeout operation
    println!("4. Testing set_timeout...");
    let temp = Temperature {
        celsius: 25.5,
        timestamp_ms: 10000,
    };
    match producer.set_timeout(temp, Duration::from_millis(500)) {
        Ok(()) => println!("   ✓ Set with timeout succeeded\n"),
        Err(e) => println!("   ✗ Set with timeout failed: {}\n", e),
    }

    // Step 5: Clean shutdown
    println!("5. Shutting down...");
    // Give async tasks time to process values
    thread::sleep(Duration::from_millis(200));

    // Detach the handle to gracefully shut down the runtime thread
    handle.detach()?;
    println!("   ✓ Database detached successfully\n");

    println!("=== Demo Complete ===");
    println!("\nNote: This example demonstrates the producer API.");
    println!("Consumer API will be added in a future update.");

    Ok(())
}
