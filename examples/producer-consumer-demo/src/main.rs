//! Producer-Consumer Pattern Demo
//!
//! This example demonstrates the unified Database API in AimDB,
//! showing:
//! - Type-safe record registration with TypeId
//! - Self-registering records via RecordT trait
//! - Cross-record communication via Emitter
//! - Producer and consumer pipelines
//! - Built-in call tracking and observability
//!
//! # Architecture
//!
//! ```text
//! Messages (input) → Producer → Consumers → (may emit) → Alerts
//!                                ↓
//!                           UpperConsumer
//!                           (if len > threshold)
//!                                ↓
//!                          Alerts (output)
//! ```

use aimdb_core::{Database, Emitter, RecordRegistrar, RecordT};
use aimdb_tokio_adapter::{TokioAdapter, TokioDatabaseBuilder};
use std::sync::Arc;

/* ================== Record Types ================== */

/// Represents incoming message data
#[derive(Clone, Debug)]
pub struct Messages(pub String);

/// Represents alerts generated from messages
#[derive(Clone, Debug)]
pub struct Alerts(pub String);

/* ================== Configuration ================== */

/// Shared configuration for message processing
#[derive(Clone)]
pub struct SharedStringsCfg {
    pub prefix: String,
    pub alert_threshold_len: usize,
}

/// Configuration for message producer
#[derive(Clone)]
pub struct MsgProducer {
    pub tag: String,
    pub min_len: usize,
}

impl MsgProducer {
    pub async fn run(&self, _em: &Emitter, msg: &Messages) {
        if msg.0.len() < self.min_len {
            println!("[drop] {}", msg.0);
            return;
        }
        println!("[{}] {}", self.tag, msg.0);

        // Simulate some async work
        #[cfg(feature = "std")]
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Consumer that converts messages to uppercase
#[derive(Clone)]
pub struct UpperConsumer;

impl UpperConsumer {
    pub async fn run(&self, em: &Emitter, shared: Arc<SharedStringsCfg>, msg: &Messages) {
        println!("{} [upper] {}", shared.prefix, msg.0.to_uppercase());

        // Cross-emit: long messages raise an Alert
        if msg.0.len() > shared.alert_threshold_len {
            let _ = em.emit(Alerts(format!("[derived] {}", msg.0))).await;
        }
    }
}

/* ================== Self-Registering Records ================== */

impl RecordT for Messages {
    type Config = (Arc<SharedStringsCfg>, Arc<MsgProducer>, Arc<UpperConsumer>);

    fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, cfg: &Self::Config) {
        let (shared, prod, upper) = cfg.clone();

        // Clone for closures
        let prod_for_producer = prod.clone();
        let shared_for_consumer = shared.clone();
        let upper_for_consumer = upper.clone();

        // Use fluent API - chain the calls
        let _ = reg
            .producer(move |em, msg| {
                let prod = prod_for_producer.clone();
                async move {
                    prod.run(&em, &msg).await;
                }
            })
            .consumer(move |em, msg| {
                let shared = shared_for_consumer.clone();
                let upper = upper_for_consumer.clone();
                async move {
                    upper.run(&em, shared, &msg).await;
                }
            });
    }
}

impl RecordT for Alerts {
    type Config = Arc<SharedStringsCfg>;

    fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, cfg: &Self::Config) {
        let shared_for_consumer = cfg.clone();

        // Use fluent API - chain the calls
        let _ = reg
            .producer(move |_em, a| async move {
                println!("[ALERT] {}", a.0);
            })
            .consumer(move |_em, a| {
                let shared = shared_for_consumer.clone();
                async move {
                    println!("{} [sink] {}", shared.prefix, a.0);
                }
            });
    }
}

/* ================== Main Demo ================== */

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║     AimDB Producer-Consumer Pattern Demo (Tokio)         ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    // Create configurations
    let shared = Arc::new(SharedStringsCfg {
        prefix: "[STR]".into(),
        alert_threshold_len: 10,
    });
    let prod = Arc::new(MsgProducer {
        tag: "[sensor-A]".into(),
        min_len: 3,
    });
    let upper = Arc::new(UpperConsumer);

    println!("✓ Configuration created");

    // Build database with self-registering records using the unified API
    println!("Building database with type-safe records...");
    let db = Database::<TokioAdapter>::builder()
        .record::<Messages>(&(shared.clone(), prod.clone(), upper.clone()))
        .record::<Alerts>(&shared)
        .build()?;
    println!("✓ Database built successfully");

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Emitting messages (data flows through pipeline):");

    // Emit some messages - these will flow through the producer-consumer pipeline
    db.produce(Messages("hi".into())).await?;
    println!();

    db.produce(Messages("hello".into())).await?;
    println!();

    db.produce(Messages("this one is definitely long".into()))
        .await?;
    println!();

    // Give async tasks time to complete
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Inspecting call statistics:");

    // Inspect producer statistics
    if let Some((calls, last)) = db.producer_stats::<Messages>() {
        println!("📊 Messages producer:");
        println!("   • Calls: {}", calls);
        println!("   • Last:  {:?}", last);
    }

    // Inspect consumer statistics
    let consumers = db.consumer_stats::<Messages>();
    for (i, (calls, last)) in consumers.into_iter().enumerate() {
        println!("\n📊 Messages consumer #{}:", i + 1);
        println!("   • Calls: {}", calls);
        println!("   • Last:  {:?}", last);
    }

    // Inspect Alerts producer stats
    if let Some((calls, last)) = db.producer_stats::<Alerts>() {
        println!("\n📊 Alerts producer:");
        println!("   • Calls: {}", calls);
        println!("   • Last:  {:?}", last);
    }

    Ok(())
}
