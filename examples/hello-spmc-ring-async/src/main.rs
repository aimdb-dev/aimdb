use std::time::Duration;
use std::{fmt::Display, sync::Arc};

use aimdb_core::{buffer::BufferCfg, AimDbBuilder, Producer, RuntimeContext};
use aimdb_core::{Consumer, DbError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

#[derive(Debug, Clone, PartialEq)]
pub struct Temperature {
    /// Temperature in degrees Celsius
    pub celcius: f32,

    /// Unix timestamp (milliseconds)
    pub timestamp: u64,
}

impl Display for Temperature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "At {}: {} celcius", self.timestamp, self.celcius)
    }
}

// Main function
#[tokio::main]
async fn main() -> Result<(), DbError> {
    println!("=== hello-spmc-ring-async: SPMC ring buffer demo ===\n");
    println!("Ring size:          10");
    println!("Producer at rate    20.0 messages/sec");
    println!("Observer 01 at rate 25.0 messages/sec");
    println!("Observer 02 at rate  6.7 messages/sec");
    println!("\n");

    // configuration
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    // Two observers consuming at different rates
    builder.configure::<Temperature>("sensor.temp", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
            .source(rollout_source)
            .tap(rollout_observer01)
            .tap(rollout_observer02);
    });

    let (_db, runner) = builder.build().await?;
    tokio::spawn(runner.run());
    tokio::time::sleep(Duration::from_millis(6000)).await;

    Ok(())
}

async fn rollout_source(ctx: RuntimeContext, producer: Producer<Temperature>) {
    let time = ctx.time();
    let t0: f32 = -20.0;
    for i in 0..40 {
        time.sleep_millis(50).await;
        // Read wall-clock time through the runtime abstraction rather than
        // `SystemTime::now()`, so the example stays runtime/sim-friendly.
        let timestamp = time.unix_time().map(|(secs, _)| secs).unwrap_or(0);
        publich_rollout(&producer, t0 + (i as f32 + 1.0) * 2.0, timestamp).await;
    }
}

async fn publich_rollout(producer: &Producer<Temperature>, t: f32, timestamp: u64) {
    let temperature = Temperature {
        celcius: t,
        timestamp,
    };
    producer.produce(temperature);
}

async fn rollout_observer01(ctx: RuntimeContext, consumer: Consumer<Temperature>) {
    let mut reader = consumer.subscribe();
    let time = ctx.time();

    while let Ok(t) = reader.recv().await {
        println!("Observer 01: {}", t);
        time.sleep_millis(40).await;
    }
}

async fn rollout_observer02(ctx: RuntimeContext, consumer: Consumer<Temperature>) {
    let mut reader = consumer.subscribe();
    let time = ctx.time();

    // Consume at slower rate, must miss data
    loop {
        match reader.recv().await {
            Ok(t) => {
                println!("Observer 02: {}", t);
                time.sleep_millis(150).await;
            }
            Err(DbError::BufferLagged { lag_count, .. }) => {
                println!("Observer 02 lagged, dropped {lag_count}");
            }
            Err(_) => break,
        };
    }
}
