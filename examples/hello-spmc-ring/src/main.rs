//! AimDB SPMC Ring Sync Demo
//! This example demonstrates the `SpmcRing` buffer using AimDB's synchronous API.
//!
//! ## What This Demo Shows
//!
//! 1. Building an AimDB instance with a typed record and 2 tap observers (slow and fast)
//! 2. Attaching the database to get a sync handle
//! 3. Creating a producer in sync context
//! 4. Setting values using blocking operations
//! 5. Demonstrating how SPMC ring keeps a bounded backlog per consumer
//! 6. Clean shutdown with detach()

use std::{
    fmt::Display,
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, Consumer, RuntimeContext};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

#[derive(Debug, Clone)]
struct Temperature {
    /// Temperature in degrees Celsius
    celsius: f32,

    /// Unix timestamp (secs)
    timestamp_secs: u64,
}

impl Display for Temperature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "At {}: {}°C", self.timestamp_secs, self.celsius)
    }
}

const KEY: &str = "sensor.temperature";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== hello-spmc-ring: SPMC ring buffer sync demo ===\n");
    println!("Ring size:          10");
    println!("Producer interval:   50 ms");
    println!("Fast tap delay:      none");
    println!("Slow tap delay:      100 ms");
    println!("\n");
    // Step 1: Build the database and attach it for sync API usage
    println!("1. Building database and attaching for sync API...");

    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Temperature>(KEY, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            // Register 2 taps that observe temperature values
            .tap(sensor_observer_fast)
            .tap(sensor_observer_slow);
    });

    // Build happens inside the runtime thread where Tokio context exists
    let handle = builder.attach()?;

    let producer = handle.producer::<Temperature>(KEY)?;

    for i in 0..10 {
        let now = SystemTime::now();
        let since_the_epoch = now.duration_since(UNIX_EPOCH).expect("Time went backwards");

        // Get timestamp in seconds (u64)
        let timestamp_secs = since_the_epoch.as_secs();
        let temperature = Temperature {
            celsius: 30.0 + i as f32 * 0.5,
            timestamp_secs,
        };

        println!("   Main: Setting Temperature {}", temperature);

        producer.set(temperature)?;

        thread::sleep(Duration::from_millis(50));
    }

    // Give time to slow tap to be able to observe the rest of the values
    thread::sleep(Duration::from_millis(1000));

    handle.detach()?;

    Ok(())
}

async fn sensor_observer_fast(ctx: RuntimeContext, consumer: Consumer<Temperature>) {
    let mut reader = consumer.subscribe();

    while let Ok(temp) = reader.recv().await {
        ctx.log()
            .info(&format!("[fast] observed temperature {}", temp));
    }
}

async fn sensor_observer_slow(ctx: RuntimeContext, consumer: Consumer<Temperature>) {
    let mut reader = consumer.subscribe();
    let time = ctx.time();

    while let Ok(temp) = reader.recv().await {
        ctx.log()
            .info(&format!("[slow] observed temperature {}", temp));

        time.sleep_millis(100).await;
    }
}
