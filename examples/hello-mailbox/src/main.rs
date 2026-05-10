use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_sync::AimDbBuilderSyncExt;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// Enum of colors for the LED
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Color {
    Red,
    Green,
    Blue,
}

// Struct representing the LED state
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Led {
    color: Color,
}

// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== hello-mailbox: Mailbox buffer demo ===\n");

    // configuration
    let adapter = Arc::new(TokioAdapter);
    let mut builder = AimDbBuilder::new().runtime(adapter);

    builder.configure::<Led>("actuator.led", |reg| {
        reg.buffer(BufferCfg::Mailbox);
    });

    let handle = builder.attach()?;
    let producer = handle.producer::<Led>("actuator.led")?;

    {
        // Produce quickly BEFORE creating the consumer
        println!("   Round 1 ");
        println!("1. Firing three rapid commands BEFORE consumer exists: Red → Green → Blue");
        producer.set(Led { color: Color::Red })?;
        producer.set(Led {
            color: Color::Green,
        })?;
        producer.set(Led { color: Color::Blue })?;
        thread::sleep(Duration::from_millis(100));

        // Now we create the consumer — it will only see the last value in the Mailbox
        println!("2. Consumer created AFTER the burst — reads once:");
        let consumer = handle.consumer::<Led>("actuator.led")?;
        thread::sleep(Duration::from_millis(100));

        match consumer.try_get() {
            Ok(msg) => println!("   ✓ Got: {:?}  ← only the latest survived", msg.color),
            Err(_) => println!("   (mailbox was already empty)"),
        }

        println!("   (Red and Green were overwritten before anyone could read them)\n");
    }

    println!("   Round 2 ");
    println!("1. Firing two rapid commands BEFORE consumer exists: Red → Green");
    // Create the color burst again
    producer.set(Led { color: Color::Red })?;
    producer.set(Led {
        color: Color::Green,
    })?;
    thread::sleep(Duration::from_millis(100));

    println!("2. Consumer created AFTER the burst — reads once:");
    let consumer2 = handle.consumer::<Led>("actuator.led")?;
    thread::sleep(Duration::from_millis(100));

    match consumer2.try_get() {
        Ok(msg) => println!(
            "   ✓ Got: {:?}  ← only the latest survived (Green)",
            msg.color
        ),
        Err(_) => println!("   (mailbox was already empty)"),
    }

    println!("3. Shutting down...");
    handle.detach()?;
    println!("   ✓ Done.");
    Ok(())
}
