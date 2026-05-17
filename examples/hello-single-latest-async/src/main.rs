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

    let _db = builder.build().await?;

    tokio::time::sleep(Duration::from_millis(700)).await;
    println!("Done. SingleLatest keeps only the current value for each subscriber.");

    Ok(())
}

async fn rollout_source(
    ctx: RuntimeContext<TokioAdapter>,
    producer: Producer<FeatureGate, TokioAdapter>,
) {
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

async fn publish_rollout(producer: &Producer<FeatureGate, TokioAdapter>, rollout_percent: u8) {
    let gate = FeatureGate { rollout_percent };
    match producer.produce(gate).await {
        Ok(()) => println!("source published rollout: {rollout_percent}%"),
        Err(err) => eprintln!("failed to publish rollout {rollout_percent}%: {err}"),
    }
}

async fn rollout_observer(
    ctx: RuntimeContext<TokioAdapter>,
    consumer: Consumer<FeatureGate, TokioAdapter>,
) {
    let Ok(mut reader) = consumer.subscribe() else {
        eprintln!("failed to subscribe to config.checkout_rollout");
        return;
    };
    let time = ctx.time();

    while let Ok(gate) = reader.recv().await {
        println!("tap observed current rollout: {}%", gate.rollout_percent);
        if gate.rollout_percent == 100 {
            break;
        }
        time.sleep(time.millis(90)).await;
    }
}
