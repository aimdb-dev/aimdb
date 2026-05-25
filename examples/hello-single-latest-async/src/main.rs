use aimdb_core::{buffer::BufferCfg, AimDbBuilder, Consumer, Producer, RuntimeContext};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
struct FeatureGate {
    rollout_percent: u8,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== hello-single-latest-async: SingleLatest buffer demo ===\n");

    let adapter = Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<FeatureGate>("config.checkout_rollout", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .source(rollout_source)
            .tap(rollout_observer);
    });

    let (_db, runner) = builder.build().await?;
    // Drive the database futures concurrently with the demo's wait — without
    // this the registered source/tap futures never run.
    tokio::spawn(runner.run());

    tokio::time::sleep(Duration::from_millis(700)).await;
    println!("Done. SingleLatest keeps only the current value for each subscriber.");

    Ok(())
}

async fn rollout_source(ctx: RuntimeContext<TokioAdapter>, producer: Producer<FeatureGate>) {
    let time = ctx.time();

    time.sleep(time.millis(50)).await;

    for rollout_percent in [0, 10, 25] {
        publish_rollout(&producer, rollout_percent).await;
    }

    for rollout_percent in [50, 100] {
        time.sleep(time.millis(120)).await;
        publish_rollout(&producer, rollout_percent).await;
    }
}

async fn publish_rollout(producer: &Producer<FeatureGate>, rollout_percent: u8) {
    let gate = FeatureGate { rollout_percent };
    producer.produce(gate);
}

async fn rollout_observer(ctx: RuntimeContext<TokioAdapter>, consumer: Consumer<FeatureGate>) {
    let mut reader = consumer.subscribe();
    let time = ctx.time();

    let mut first = true;
    while let Ok(gate) = reader.recv().await {
        if first {
            first = false;
            if gate.rollout_percent != 0 {
                println!(
                    "   (rollouts before {}% were overwritten before the tap could read them - SingleLatest keeps only the latest)",
                    gate.rollout_percent
                );
            }
        }
        println!("tap observed current rollout: {}%", gate.rollout_percent);
        if gate.rollout_percent == 100 {
            break;
        }
        time.sleep(time.millis(90)).await;
    }
}
