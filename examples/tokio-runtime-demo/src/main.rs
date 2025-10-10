//! Example demonstrating full AimDB service integration with unified API

use aimdb_core::{Database, DbResult, RuntimeContext};
use aimdb_executor::Runtime;
use aimdb_tokio_adapter::{TokioAdapter, TokioDatabaseBuilder};
use std::time::Duration;

// These wrap the shared implementations for clean separation
async fn data_processor_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    aimdb_examples_shared::data_processor_service(ctx).await
}

async fn monitoring_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    aimdb_examples_shared::monitoring_service(ctx).await
}

#[tokio::main]
async fn main() -> DbResult<()> {
    println!("🔧 Setting up AimDB with Tokio runtime...");

    // Create database using the new unified builder API
    let db = Database::<TokioAdapter>::builder().build()?;

    println!("✅ AimDB database created successfully");

    // Get runtime context for services
    let ctx = db.context();

    println!("\n🎯 Spawning services with unified API:");

    // Spawn services using the unified spawn() method
    // This works the same way across both Tokio and Embassy!
    let ctx1 = ctx.clone();
    db.spawn(async move {
        if let Err(e) = data_processor_service(ctx1).await {
            eprintln!("Data processor error: {:?}", e);
        }
    })?;

    let ctx2 = ctx.clone();
    db.spawn(async move {
        if let Err(e) = monitoring_service(ctx2).await {
            eprintln!("Monitoring service error: {:?}", e);
        }
    })?;

    println!("\n⚡ Services spawned successfully!");

    // Give the services some time to run
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("\n🎉 All services completed successfully!");
    Ok(())
}
