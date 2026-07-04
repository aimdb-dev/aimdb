use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Temperature {
    pub celsius: f32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Temperature>("temp.indoor", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .source(|ctx, producer| async move {
                let time = ctx.time();
                for celsius in [21.0, 22.5, 24.1] {
                    producer.produce(Temperature { celsius });
                    time.sleep_secs(1).await;
                }
            })
            .tap(|ctx, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(t) = reader.recv().await {
                    ctx.log().info(&format!("temp: {:.1}°C", t.celsius));
                }
            });
    });

    // `.run()` builds the database, collects every producer/consumer/transform
    // future, and drives them all on a single `FuturesUnordered`. It blocks
    // until shutdown. For programmatic access to the `AimDb` handle, call
    // `.build().await?` directly — it returns `(AimDb, AimDbRunner)`.
    builder.run().await?;
    Ok(())
}
