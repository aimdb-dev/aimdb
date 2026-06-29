use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

// Enum of colors for the LED
#[derive(Debug, Clone, PartialEq, Eq)]
enum Color {
    Red,
    Green,
    Blue,
}

// Struct representing the LED state
#[derive(Debug, Clone, PartialEq, Eq)]
struct Led {
    color: Color,
}

// Main function
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== hello-mailbox-async: Mailbox buffer demo ===\n");

    // configuration
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Led>("actuator.led", |reg| {
        reg.buffer(BufferCfg::Mailbox)
            .source(|ctx, producer| async move {
                // Produce quickly BEFORE creating the consumer
                println!("Firing three rapid commands BEFORE consumer exists: Red → Green → Blue");
                producer.produce(Led { color: Color::Red });
                producer.produce(Led {
                    color: Color::Green,
                });
                producer.produce(Led { color: Color::Blue });
                ctx.time().sleep_millis(100).await;
            })
            .tap(|ctx, consumer| async move {
                // Now we create the consumer — it will only see the last value in the Mailbox
                let mut reader = consumer.subscribe();
                ctx.time().sleep_millis(100).await;

                match reader.recv().await {
                    Ok(msg) => println!("   ✓ Got: {:?}  ← only the latest survived", msg.color),
                    Err(_) => println!("   (mailbox was already empty)"),
                }
            });
    });
    builder.run().await?;
    println!("Shutting down...");
    println!("   ✓ Done.");
    Ok(())
}
