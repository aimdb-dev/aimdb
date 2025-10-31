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
//! 3. Creating producers and consumers in sync context
//! 4. Setting and getting values using blocking operations
//! 5. Multi-threaded producer-consumer patterns
//! 6. Clean shutdown with detach()

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

    // Step 2: Create consumers before producing
    println!("2. Creating consumers for Temperature...");
    let consumer1 = handle.consumer::<Temperature>()?;
    let consumer2 = handle.consumer::<Temperature>()?;
    // Alternative with custom capacity for high-frequency data:
    // let consumer1 = handle.consumer_with_capacity::<Temperature>(1000)?;
    println!("   ✓ Two consumers created\n");

    // Step 3: Spawn consumer threads
    println!("3. Starting consumer threads...");

    let consumer1_handle = thread::spawn(move || {
        println!("   [Consumer 1] Started, waiting for values...");
        for i in 0..5 {
            match consumer1.get() {
                Ok(temp) => {
                    println!("   [Consumer 1] Got #{}: {:.1}°C", i + 1, temp.celsius);
                }
                Err(e) => {
                    println!("   [Consumer 1] Error: {}", e);
                    break;
                }
            }
        }
        println!("   [Consumer 1] Finished");
    });

    let consumer2_handle = thread::spawn(move || {
        println!("   [Consumer 2] Started, waiting for values...");
        for i in 0..5 {
            // Use try_get for non-blocking reads
            match consumer2.try_get() {
                Ok(temp) => {
                    println!("   [Consumer 2] Got #{}: {:.1}°C", i + 1, temp.celsius);
                }
                Err(_) => {
                    // No data yet, sleep and retry
                    thread::sleep(Duration::from_millis(50));
                    // Try blocking get instead
                    if let Ok(temp) = consumer2.get_with_timeout(Duration::from_millis(500)) {
                        println!(
                            "   [Consumer 2] Got #{} (after timeout): {:.1}°C",
                            i + 1,
                            temp.celsius
                        );
                    }
                }
            }
        }
        println!("   [Consumer 2] Finished");
    });

    // Give consumers time to start
    thread::sleep(Duration::from_millis(100));
    println!("   ✓ Consumer threads started\n");

    // Step 4: Create a synchronous producer
    println!("4. Creating producer and producing values...");
    let producer = handle.producer::<Temperature>()?;
    // Alternative with custom capacity:
    // let producer = handle.producer_with_capacity::<Temperature>(500)?;
    println!("   ✓ Producer created\n");

    // Step 5: Produce values
    println!("5. Producing temperature values...");

    for i in 0..10 {
        let temp = Temperature {
            celsius: 20.0 + i as f32 * 0.5,
            timestamp_ms: i * 1000,
        };
        println!("   Main: Setting temperature {:.1}°C", temp.celsius);

        // Use blocking send
        if let Err(e) = producer.set(temp) {
            eprintln!("   Error setting value: {}", e);
        }

        thread::sleep(Duration::from_millis(100));
    }

    println!("   ✓ All values produced\n");

    // Step 6: Wait for consumers to finish
    println!("6. Waiting for consumer threads...");
    consumer1_handle.join().unwrap();
    consumer2_handle.join().unwrap();
    println!("   ✓ All consumers finished\n");

    // Step 7: Clean shutdown
    println!("7. Shutting down...");
    // Give async tasks time to process remaining values
    thread::sleep(Duration::from_millis(200));

    // Detach the handle to gracefully shut down the runtime thread
    handle.detach()?;
    println!("   ✓ Database detached successfully\n");

    println!("=== Demo Complete ===");
    println!("\nThis example demonstrated:");
    println!("  • Pure synchronous context (no #[tokio::main])");
    println!("  • Multiple independent consumers");
    println!("  • Blocking (get), timeout (get_timeout), and non-blocking (try_get) reads");
    println!("  • Blocking (set), timeout (set_timeout), and non-blocking (try_set) writes");
    println!("  • Multi-threaded producer-consumer patterns");

    Ok(())
}
