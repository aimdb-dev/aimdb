//! Example demonstrating full AimDB service integration with RuntimeContext

use aimdb_core::{service, DatabaseSpec, RuntimeContext};
// Note: DbResult appears unused because the #[service] macro rewrites it to aimdb_core::DbResult
// but it's needed for the source code to be readable
#[allow(unused_imports)]
use aimdb_core::DbResult;
use aimdb_executor::Runtime;
use aimdb_tokio_adapter::{new_database, TokioAdapter};
use std::time::Duration;

// Wrap shared service implementations with #[service] macro for adapter-specific spawning
#[service]
async fn data_processor_service<R: Runtime>(ctx: RuntimeContext<R>) -> DbResult<()> {
    aimdb_examples_shared::data_processor_service(ctx).await
}

#[service]
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
    println!("\n🎯 Spawning services via TokioAdapter:");

    let adapter = db.adapter();

    // Spawn services using the new clean API
    let _handle1 = DataProcessorService::spawn_tokio(adapter)?;
    let _handle2 = MonitoringService::spawn_tokio(adapter)?;

    println!("\n⚡ Services spawned successfully!");

    // Give the services some time to run
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("\n🎉 All services completed successfully!");
    println!("🔍 Service names:");
    println!("  - {}", DataProcessorService::service_name());
    println!("  - {}", MonitoringService::service_name());

    Ok(())
}
