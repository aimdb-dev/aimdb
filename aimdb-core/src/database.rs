//! AimDB Database Implementation
//!
//! This module provides the core database implementation for AimDB, supporting async
//! in-memory storage with real-time synchronization across MCU → edge → cloud environments.

use crate::{DbResult, RuntimeAdapter};

/// Core trait for runnable databases - must be implemented by adapter-specific database types
/// to provide their own runtime-specific `run()` implementations.
pub trait Runnable {
    /// Runs the database main loop
    ///
    /// This method starts the database runtime and keeps it running indefinitely.
    /// It handles events, manages services and coordinates record operations.
    ///
    /// # Returns
    /// Never returns - runs indefinitely
    fn run(self) -> impl core::future::Future<Output = ()> + Send;
}

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};

#[cfg(feature = "std")]
use std::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};

/// Service registration function type
///
/// This type represents a closure that registers a service with a database instance.
/// The closure receives a reference to the database and can spawn services as needed.
pub type ServiceRegistrationFn<A> = Box<dyn FnOnce(&Database<A>) + Send + 'static>;

/// Database specification
///
/// This struct holds the configuration for creating a database instance.
/// It is runtime-agnostic and works with any RuntimeAdapter implementation.
pub struct DatabaseSpec<A: RuntimeAdapter> {
    records: Vec<String>,
    services: Vec<ServiceRegistrationFn<A>>,
    _phantom: core::marker::PhantomData<A>,
}

impl<A: RuntimeAdapter> DatabaseSpec<A> {
    /// Creates a new specification builder
    ///
    /// # Returns
    /// A builder for constructing database specifications
    pub fn builder() -> DatabaseSpecBuilder<A> {
        DatabaseSpecBuilder::new()
    }
}

/// Builder for database specifications
pub struct DatabaseSpecBuilder<A: RuntimeAdapter> {
    records: Vec<String>,
    services: Vec<ServiceRegistrationFn<A>>,
    _phantom: core::marker::PhantomData<A>,
}

impl<A: RuntimeAdapter> DatabaseSpecBuilder<A> {
    /// Creates a new database specification builder
    fn new() -> Self {
        Self {
            records: Vec::new(),
            services: Vec::new(),
            _phantom: core::marker::PhantomData,
        }
    }

    /// Adds a record type to the database specification
    ///
    /// # Arguments
    /// * `name` - The name of the record type
    ///
    /// # Returns
    /// Self for method chaining
    pub fn record(mut self, name: &str) -> Self {
        #[cfg(feature = "tracing")]
        tracing::debug!("Adding record type: {}", name);

        self.records.push(name.into());
        self
    }

    /// Adds a service to the database specification
    ///
    /// Services are registered as closures that configure how they should
    /// be spawned when the database is initialized.
    ///
    /// # Arguments
    /// * `service_fn` - A closure that configures the service
    ///
    /// # Returns
    /// Self for method chaining
    pub fn service<F>(mut self, service_fn: F) -> Self
    where
        F: FnOnce(&Database<A>) + Send + 'static,
    {
        #[cfg(feature = "tracing")]
        tracing::debug!("Registering service function");

        self.services.push(Box::new(service_fn));
        self
    }

    /// Builds the final database specification
    ///
    /// # Returns
    /// A database specification ready for use in database initialization
    pub fn build(self) -> DatabaseSpec<A> {
        #[cfg(feature = "tracing")]
        tracing::info!(
            "Building database spec with {} records and {} services",
            self.records.len(),
            self.services.len()
        );

        DatabaseSpec {
            records: self.records,
            services: self.services,
            _phantom: core::marker::PhantomData,
        }
    }
}

/// AimDB Database implementation
///
/// Provides a runtime-agnostic database implementation that uses a RuntimeAdapter
/// for runtime-specific operations like task spawning and timing.
///
/// # Design Philosophy
///
/// - **Runtime Agnostic**: Core behavior doesn't depend on specific runtimes
/// - **Async First**: All operations are async for consistency
/// - **Error Handling**: Comprehensive error propagation
/// - **Service Management**: Integrated service spawning capabilities
///
/// # Usage
///
/// ```rust,no_run
/// # async fn example<A: aimdb_core::RuntimeAdapter>(adapter: A) {
/// use aimdb_core::{Database, DatabaseSpec};
///
/// let spec = DatabaseSpec::builder()
///     .record("sensors")
///     .record("metrics")
///     .build();
///
/// let db = Database::new(adapter, spec);
/// let sensors = db.record("sensors");
///
/// // Note: To run the database, use adapter-specific implementations:
/// // - For Embassy: aimdb_embassy_adapter::embassy::init(spawner, spec)
/// // - For Tokio: aimdb_tokio_adapter::tokio::init(spec)
/// # }
/// ```
pub struct Database<A: RuntimeAdapter> {
    adapter: A,
    records: BTreeMap<String, Record>,
}

impl<A: RuntimeAdapter> Database<A> {
    /// Creates a new database instance with the given adapter and specification
    ///
    /// # Arguments
    /// * `adapter` - Runtime adapter for async operations
    /// * `spec` - Database specification defining records and services
    ///
    /// # Returns
    /// A configured database ready for use
    pub fn new(adapter: A, spec: DatabaseSpec<A>) -> Self {
        #[cfg(feature = "tracing")]
        tracing::info!(
            "Initializing database with {} records and {} services",
            spec.records.len(),
            spec.services.len()
        );

        let mut records = BTreeMap::new();

        // Initialize records based on the spec
        for record_name in spec.records {
            #[cfg(feature = "tracing")]
            tracing::debug!("Initializing record: {}", record_name);

            records.insert(record_name.clone(), Record::new(record_name));
        }

        let db = Self { adapter, records };

        // Register and start services
        for service_fn in spec.services {
            #[cfg(feature = "tracing")]
            tracing::debug!("Registering service");

            service_fn(&db);
        }

        db
    }

    /// Gets a record handle by name
    ///
    /// # Arguments
    /// * `name` - The name of the record to retrieve
    ///
    /// # Returns
    /// A record handle for performing operations on the named record
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example<A: aimdb_core::RuntimeAdapter>(db: aimdb_core::Database<A>) {
    /// let sensors = db.record("sensors");
    /// # }
    /// ```
    pub fn record(&self, name: &str) -> Record {
        #[cfg(feature = "tracing")]
        tracing::debug!("Getting record handle for: {}", name);

        self.records.get(name).cloned().unwrap_or_else(|| {
            #[cfg(feature = "tracing")]
            tracing::warn!("Record '{}' not found, creating empty record", name);
            Record::new(name.into())
        })
    }

    /// Gets a reference to the runtime adapter
    ///
    /// This allows users to access the runtime adapter directly for service spawning.
    /// Services should be defined using the `#[service]` macro for proper runtime integration.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use aimdb_core::{service, AimDbService, Database, RuntimeAdapter};
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// # use aimdb_core::SpawnDynamically;
    ///
    /// // Define a service using the service macro
    /// #[service]
    /// async fn my_background_task() -> aimdb_core::DbResult<()> {
    ///     // Service implementation
    ///     Ok(())
    /// }
    ///
    /// # async fn example<A: SpawnDynamically>(db: Database<A>) -> aimdb_core::DbResult<()> {
    /// // Spawn service through the generated service struct
    /// MyBackgroundTaskService::spawn_on_tokio(db.adapter())?;
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub fn adapter(&self) -> &A {
        &self.adapter
    }

    /// Consumes the database and returns its components
    ///
    /// This is useful for adapter-specific `run()` implementations that need
    /// to access both the adapter and records.
    ///
    /// # Returns
    /// Tuple of (adapter, records) for use in adapter-specific run methods
    pub fn into_parts(self) -> (A, BTreeMap<String, Record>) {
        (self.adapter, self.records)
    }
}

/// Database record implementation
///
/// Records represent data containers within the database that support
/// async read and write operations with proper error handling.
///
/// # Design Philosophy
///
/// - **Async Operations**: All I/O operations are async
/// - **Type Safety**: Strong typing for field operations where possible
/// - **Error Propagation**: Comprehensive error handling
/// - **Runtime Agnostic**: Works across different async runtimes
#[derive(Debug, Clone)]
pub struct Record {
    name: String,
    // TODO: Add actual data storage
    // This should use lock-free data structures or appropriate synchronization
    // primitives that work in both std and no_std environments
    #[allow(dead_code)]
    data: BTreeMap<String, String>,
}

impl Record {
    /// Creates a new record
    fn new(name: String) -> Self {
        #[cfg(feature = "tracing")]
        tracing::debug!("Creating new record: {}", name);

        Self {
            name,
            data: BTreeMap::new(),
        }
    }

    /// Gets the name of the record
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Writes a value to a field in the record
    ///
    /// # Arguments
    /// * `field` - The name of the field to write to
    /// * `value` - The value to write (as string for now, will be generic later)
    ///
    /// # Returns
    /// `DbResult<()>` indicating success or failure
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example(mut record: aimdb_core::Record) -> aimdb_core::DbResult<()> {
    /// record.write("temperature", "23.5").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write(&mut self, field: &str, value: &str) -> DbResult<()> {
        let _field = String::from(field);
        let _value = String::from(value);
        let _record_name = self.name.clone();

        #[cfg(feature = "tracing")]
        tracing::debug!("Writing to {}.{}: {}", _record_name, _field, _value);

        // TODO: Implement actual write operation with proper synchronization
        // For now, this is a placeholder that would need proper async synchronization

        Ok(())
    }

    /// Reads a value from a field in the record
    ///
    /// # Arguments
    /// * `field` - The name of the field to read from
    ///
    /// # Returns
    /// `DbResult<Option<&str>>` containing the field value if it exists
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example(record: aimdb_core::Record) -> aimdb_core::DbResult<()> {
    /// if let Some(temp) = record.read("temperature").await? {
    ///     // Process temperature value
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn read(&self, field: &str) -> DbResult<Option<&'static str>> {
        let _field = String::from(field);
        let _record_name = self.name.clone();

        #[cfg(feature = "tracing")]
        tracing::debug!("Reading from {}.{}", _record_name, _field);

        // TODO: Implement actual read operation with proper synchronization
        // For now, this is a placeholder that would need proper async synchronization

        Ok(None)
    }
}

// Note: Runtime-specific type aliases are provided by their respective adapter crates:
// - `EmbassyDatabase` in `aimdb-embassy-adapter`
// - `TokioDatabase` in `aimdb-tokio-adapter`
