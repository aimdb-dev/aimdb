//! Example demonstrating full AimDB service integration with RuntimeContext

use aimdb_core::{DatabaseSpec, RuntimeContext, DbResult, Spawn};
use aimdb_executor::Runtime;
use aimdb_tokio_adapter::{new_database, TokioAdapter};
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

    // Create database with Tokio runtime
    let spec = DatabaseSpec::<TokioAdapter>::builder().build();
    let db = new_database(spec)?;

    println!("✅ AimDB database created successfully");

    // Run services using the new clean API
    println!("\n🎯 Spawning services - simple and explicit:");

    let adapter = db.adapter();
    let ctx1 = RuntimeContext::from_runtime(adapter.clone());
    let ctx2 = RuntimeContext::from_runtime(adapter.clone());

    // Spawn services directly - no macro-generated methods!
    // This is MUCH simpler: just call adapter.spawn() with the service future
    let _handle1 = adapter.spawn(async move {
        if let Err(e) = data_processor_service(ctx1).await {
            eprintln!("Data processor error: {:?}", e);
        }
    }).map_err(|e| aimdb_core::DbError::from(e))?;

    let _handle2 = adapter.spawn(async move {
        if let Err(e) = monitoring_service(ctx2).await {
            eprintln!("Monitoring service error: {:?}", e);
        }
    }).map_err(|e| aimdb_core::DbError::from(e))?;

    println!("\n⚡ Services spawned successfully!");

    // Give the services some time to run
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("\n🎉 All services completed successfully!");
    Ok(())
}
