use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== hello-mailbox-async: Mailbox buffer demo ===\n");

    // configuration
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Led>("actuator.led", |reg| {
        reg.buffer(BufferCfg::Mailbox)
            .source(|_ctx, producer| async move {
                println!("Firing three rapid commands BEFORE consumer exists: Red → Green → Blue");
                producer.produce(Led { color: Color::Red });
                producer.produce(Led {
                    color: Color::Green,
                });
                producer.produce(Led { color: Color::Blue });
                thread::sleep(Duration::from_millis(100));
            })
            .tap(|_ctx, consumer| async move {
                let mut reader = consumer.subscribe();
                thread::sleep(Duration::from_millis(100));

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
