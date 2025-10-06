//! Embassy Database Implementation
//!
//! This module provides the Embassy-specific database implementation that wraps
//! the core database with Embassy runtime capabilities.

use crate::runtime::EmbassyAdapter;
use aimdb_core::{Database, DatabaseSpec, DatabaseSpecBuilder, Record, Runnable};

#[cfg(feature = "embassy-runtime")]
use embassy_executor::Spawner;

/// Embassy database implementation
///
/// This is a newtype wrapper that combines the core database implementation
/// with the Embassy runtime adapter, providing Embassy-specific functionality.
pub struct EmbassyDatabase(Database<EmbassyAdapter>);

impl EmbassyDatabase {
    /// Creates a new Embassy database instance
    pub fn new(adapter: EmbassyAdapter, spec: EmbassyDatabaseSpec) -> Self {
        Self(Database::new(adapter, spec))
    }

    /// Gets a record handle by name
    pub fn record(&self, name: &str) -> Record {
        self.0.record(name)
    }

    /// Gets access to the underlying adapter for service spawning
    ///
    /// Use this with services defined using the `#[service]` macro:
    /// ```rust,no_run
    /// # use aimdb_embassy_adapter::EmbassyDatabase;
    /// # use aimdb_core::service;
    ///
    /// // Define a service using the service macro
    /// #[service]
    /// async fn sensor_monitor(ctx: aimdb_core::RuntimeContext<aimdb_embassy_adapter::EmbassyAdapter>) -> aimdb_core::DbResult<()> {
    ///     // Service implementation
    ///     Ok(())
    /// }
    ///
    /// # async fn example(db: EmbassyDatabase) -> aimdb_core::DbResult<()> {
    /// // Spawn service through the generated service struct
    /// SensorMonitorService::spawn_embassy(db.adapter())?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn adapter(&self) -> &EmbassyAdapter {
        self.0.adapter()
    }
}

/// Embassy database specification type alias  
///
/// This is a convenience type for database specifications used with Embassy.
pub type EmbassyDatabaseSpec = DatabaseSpec<EmbassyAdapter>;

/// Embassy-specific database specification builder
pub type EmbassyDatabaseSpecBuilder = DatabaseSpecBuilder<EmbassyAdapter>;

/// Embassy-specific record implementation
pub type EmbassyRecord = Record;

/// Creates a new Embassy database instance
///
/// This function creates a database using the shared core implementation
/// with an Embassy runtime adapter that manages the provided spawner.
///
/// # Arguments
/// * `spawner` - The Embassy spawner for task management
/// * `spec` - Database specification defining records and services
///
/// # Returns
/// A configured Embassy database ready for use
///
/// # Example
/// ```rust,no_run
/// # #[cfg(all(not(feature = "std"), feature = "embassy-runtime"))]
/// # {
/// use embassy_executor::Spawner;
/// use aimdb_embassy_adapter::{new_database, EmbassyDatabaseSpec};
///
/// #[embassy_executor::main]
/// async fn main(spawner: Spawner) {
///     let spec = EmbassyDatabaseSpec::builder()
///         .record("sensors")
///         .build();
///     
///     let db = new_database(spawner, spec);
///     aimdb_core::run(db).await;
/// }
/// # }
/// ```
#[cfg(feature = "embassy-runtime")]
pub fn new_database(spawner: Spawner, spec: EmbassyDatabaseSpec) -> EmbassyDatabase {
    #[cfg(feature = "tracing")]
    tracing::info!("Creating Embassy database with core implementation");

    // Create the Embassy adapter that will manage the spawner
    let adapter = EmbassyAdapter::new_with_spawner(spawner);

    // Use the Embassy database wrapper
    EmbassyDatabase::new(adapter, spec)
}

/// Initializes Embassy database from a generic core specification
///
/// This function provides the clean initialization API that converts a generic
/// `DatabaseSpec` to an Embassy-specific implementation. This is the recommended
/// way to create Embassy databases from generic specifications.
///
/// # Arguments
/// * `spawner` - The Embassy spawner for task management  
/// * `spec` - Generic database specification from aimdb-core
///
/// # Returns
/// A configured Embassy database ready for use
///
/// # Example
/// ```rust,no_run
/// #![no_std]
/// #![no_main]
/// use embassy_executor::Spawner;
/// use aimdb_core::DatabaseSpec;
/// use aimdb_embassy_adapter::database;
///
/// #[embassy_executor::main]
/// async fn main(spawner: Spawner) {
///     let spec = DatabaseSpec::builder()
///         .record("temperature")
///         .build();
///     
///     let db = database::init(spawner, spec);
///     // Note: Use adapter-specific run implementation
/// }
/// ```
/// A configured Embassy database ready for use with `aimdb_core::run()`
///
/// # Example
/// ```rust,no_run
/// # #[cfg(all(not(feature = "std"), feature = "embassy-runtime"))]
/// # {
/// use embassy_executor::Spawner;
/// use aimdb_core::DatabaseSpec;
/// use aimdb_embassy_adapter::embassy;
///
/// #[embassy_executor::main]
/// async fn main(spawner: Spawner) {
///     let spec = DatabaseSpec::builder()
///         .record("sensors")
///         .service(|db| {
///             let rec = db.record("sensors");
///             // Service spawning logic here
///         })
///         .build();
///     
///     let db = embassy::init(spawner, spec);
///     aimdb_core::run(db).await;
/// }
/// # }
/// ```
#[cfg(feature = "embassy-runtime")]
pub fn init<A: aimdb_core::RuntimeAdapter>(
    spawner: Spawner,
    spec: aimdb_core::DatabaseSpec<A>,
) -> EmbassyDatabase {
    #[cfg(feature = "tracing")]
    tracing::info!("Embassy initialization from generic DatabaseSpec");

    // Convert the generic specification to Embassy-specific
    let embassy_spec = convert_spec_to_embassy(spec);

    // Create the Embassy adapter with spawner
    let adapter = EmbassyAdapter::new_with_spawner(spawner);

    // Create and return the Embassy database
    EmbassyDatabase::new(adapter, embassy_spec)
}

/// Converts a generic DatabaseSpec to Embassy-specific EmbassyDatabaseSpec
///
/// This function handles the conversion between the generic core specification
/// and the Embassy-specific implementation, preserving records and services.
#[cfg(feature = "embassy-runtime")]
fn convert_spec_to_embassy<A: aimdb_core::RuntimeAdapter>(
    _spec: aimdb_core::DatabaseSpec<A>,
) -> EmbassyDatabaseSpec {
    #[cfg(feature = "tracing")]
    tracing::debug!("Converting generic spec to Embassy spec");

    // TODO: Implement proper conversion that preserves:
    // - All record definitions from the original spec
    // - Service registration functions adapted for Embassy runtime
    // - Any other configuration options

    // For now, create a basic Embassy spec
    // This will be enhanced as we implement the full service system
    EmbassyDatabaseSpec::builder()
        .record("placeholder") // Temporary - should extract from original spec
        .build()
}

/// Embassy-specific Runnable implementation
///
/// This provides the Embassy-specific database runtime loop that uses
/// Embassy's async primitives for timing and coordination.
impl Runnable for EmbassyDatabase {
    async fn run(self) {
        #[cfg(feature = "tracing")]
        tracing::info!("Starting Embassy database main loop");

        let (_adapter, _records) = self.0.into_parts();

        loop {
            // Embassy-specific timing using embassy-time
            #[cfg(feature = "embassy-time")]
            embassy_time::Timer::after(embassy_time::Duration::from_millis(100)).await;

            // Fallback for when embassy-time is not available
            #[cfg(not(feature = "embassy-time"))]
            {
                // Use embassy executor yield
                #[cfg(feature = "embassy-runtime")]
                embassy_executor::raw::yield_now().await;

                #[cfg(not(feature = "embassy-runtime"))]
                {
                    // Minimal fallback - just return immediately
                    // In real embedded scenarios, embassy-time should be used
                    core::hint::spin_loop();
                }
            }

            // TODO: Implement actual database operations:
            // - Service health checking using Embassy tasks
            // - Record synchronization with Embassy channels
            // - Event processing with Embassy async primitives
            // - Memory management suitable for embedded systems

            #[cfg(feature = "tracing")]
            tracing::trace!("Embassy database loop iteration");
        }
    }
}
