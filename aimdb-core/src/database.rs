//! AimDB Database Implementation
//!
//! This module provides the core database implementation for AimDB, supporting async
//! in-memory storage with real-time synchronization across MCU → edge → cloud environments.

use crate::{DbError, DbResult, RuntimeAdapter, RuntimeContext};
use aimdb_executor::Spawn;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{collections::BTreeMap, string::String, vec::Vec};

#[cfg(feature = "std")]
use std::{collections::BTreeMap, string::String, vec::Vec};

/// Database specification
///
/// This struct holds the configuration for creating a database instance.
/// It is runtime-agnostic and works with any RuntimeAdapter implementation.
#[allow(dead_code)] // Used by adapter crates
pub struct DatabaseSpec<A: RuntimeAdapter> {
    pub(crate) records: Vec<String>,
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
///
/// This builder provides a fluent API for configuring databases.
/// Runtime-specific build methods are implemented in adapter crates.
pub struct DatabaseSpecBuilder<A: RuntimeAdapter> {
    records: Vec<String>,
    _phantom: core::marker::PhantomData<A>,
}

impl<A: RuntimeAdapter> DatabaseSpecBuilder<A> {
    /// Creates a new database specification builder
    fn new() -> Self {
        Self {
            records: Vec::new(),
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

    /// Internal method to convert builder to spec
    ///
    /// This is used by adapter-specific build methods.
    #[allow(dead_code)] // Used by adapter crates
    pub fn into_spec(self) -> DatabaseSpec<A> {
        #[cfg(feature = "tracing")]
        tracing::info!("Building database spec with {} records", self.records.len());

        DatabaseSpec {
            records: self.records,
            _phantom: core::marker::PhantomData,
        }
    }
}

/// AimDB Database implementation
///
/// Provides a runtime-agnostic database implementation that acts as a service
/// orchestrator, managing records and spawned services.
///
/// # Design Philosophy
///
/// - **Runtime Agnostic**: Core behavior doesn't depend on specific runtimes
/// - **Async First**: All operations are async for consistency
/// - **Service Orchestration**: Actually manages spawned services
/// - **Error Handling**: Comprehensive error propagation
///
/// # Usage
///
/// ```rust,no_run
/// # #[cfg(feature = "tokio-runtime")]
/// # {
/// use aimdb_core::Database;
/// use aimdb_tokio_adapter::TokioAdapter;
///
/// # #[tokio::main]
/// # async fn main() -> aimdb_core::DbResult<()> {
/// let db = Database::<TokioAdapter>::builder()
///     .record("sensors")
///     .record("metrics")
///     .build()?;
///
/// // Spawn services
/// let ctx = db.context();
/// db.spawn(async move {
///     // Service implementation
/// })?;
/// # Ok(())
/// # }
/// # }
/// ```
pub struct Database<A: RuntimeAdapter> {
    adapter: A,
    records: BTreeMap<String, Record>,
}

impl<A: RuntimeAdapter> Database<A> {
    /// Creates a new database builder
    ///
    /// Use this to start configuring a database. The builder provides
    /// runtime-specific build methods in the adapter crates.
    ///
    /// # Returns
    /// A builder for constructing database specifications
    ///
    /// # Example
    /// ```rust,no_run
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// use aimdb_core::Database;
    /// use aimdb_tokio_adapter::TokioAdapter;
    ///
    /// # async fn example() -> aimdb_core::DbResult<()> {
    /// let db = Database::<TokioAdapter>::builder()
    ///     .record("sensors")
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub fn builder() -> DatabaseSpecBuilder<A> {
        DatabaseSpecBuilder::new()
    }

    /// Internal constructor for creating a database from a spec
    ///
    /// This is called by adapter-specific build methods and should not
    /// be used directly.
    #[allow(dead_code)] // Used by adapter crates
    pub fn new(adapter: A, spec: DatabaseSpec<A>) -> Self {
        #[cfg(feature = "tracing")]
        tracing::info!("Initializing database with {} records", spec.records.len());

        let mut records = BTreeMap::new();

        // Initialize records based on the spec
        for record_name in spec.records {
            #[cfg(feature = "tracing")]
            tracing::debug!("Initializing record: {}", record_name);

            records.insert(record_name.clone(), Record::new(record_name));
        }

        Self { adapter, records }
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
    /// This allows direct access to the runtime adapter for advanced use cases.
    ///
    /// # Example
    /// ```rust,ignore
    /// # use aimdb_core::Database;
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// # async fn example<A: aimdb_core::RuntimeAdapter>(db: Database<A>) {
    /// let adapter = db.adapter();
    /// // Use adapter directly
    /// # }
    /// # }
    /// ```
    pub fn adapter(&self) -> &A {
        &self.adapter
    }

    /// Creates a RuntimeContext for this database
    ///
    /// The context provides services with access to runtime capabilities
    /// like timing and logging.
    ///
    /// # Returns
    /// A RuntimeContext configured for this database's runtime
    ///
    /// # Example
    /// ```rust,ignore
    /// # use aimdb_core::Database;
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// # async fn example<A: aimdb_executor::Runtime>(db: Database<A>) {
    /// let ctx = db.context();
    /// // Pass ctx to services
    /// # }
    /// # }
    /// ```
    pub fn context(&self) -> RuntimeContext<A>
    where
        A: aimdb_executor::Runtime + Clone,
    {
        #[cfg(feature = "std")]
        {
            RuntimeContext::from_arc(std::sync::Arc::new(self.adapter.clone()))
        }
        #[cfg(not(feature = "std"))]
        {
            // For no_std, we need a static reference - this would typically be handled
            // by the caller storing the adapter in a static cell first
            // For now, we'll document this limitation
            panic!("context() not supported in no_std without static reference - use adapter() directly")
        }
    }
}

// Spawn implementation for databases with spawn-capable adapters
impl<A> Database<A>
where
    A: RuntimeAdapter + Spawn,
{
    /// Spawns a service on the database's runtime
    ///
    /// This method provides a unified interface for spawning services
    /// across different runtime adapters.
    ///
    /// # Arguments
    /// * `future` - The service future to spawn
    ///
    /// # Returns
    /// `DbResult<()>` indicating whether the spawn succeeded
    ///
    /// # Example
    /// ```rust,ignore
    /// # use aimdb_core::Database;
    /// # use aimdb_executor::{Runtime, Spawn};
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// async fn my_service<R: Runtime>(ctx: aimdb_core::RuntimeContext<R>) -> aimdb_core::DbResult<()> {
    ///     // Service implementation
    ///     Ok(())
    /// }
    ///
    /// # async fn example<A: Runtime + Spawn>(db: Database<A>) -> aimdb_core::DbResult<()> {
    /// let ctx = db.context();
    /// db.spawn(async move {
    ///     if let Err(e) = my_service(ctx).await {
    ///         eprintln!("Service error: {:?}", e);
    ///     }
    /// })?;
    /// # Ok(())
    /// # }
    /// # }
    /// ```
    pub fn spawn<F>(&self, future: F) -> DbResult<()>
    where
        F: core::future::Future<Output = ()> + Send + 'static,
    {
        #[cfg(feature = "tracing")]
        tracing::debug!("Spawning service on database runtime");

        self.adapter.spawn(future).map_err(DbError::from)?;
        Ok(())
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
