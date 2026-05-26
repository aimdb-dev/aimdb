//! Core database implementation
//!
//! `AimDb` is the central coordination point for AimDB, managing type-safe
//! in-memory storage with type-safe records and data synchronization across
//! MCU → edge → cloud environments.

use crate::{AimDb, DbResult, RuntimeAdapter, RuntimeContext};
use core::fmt::Debug;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};

/// AimDB Database implementation
///
/// Unified database combining runtime adapter management with type-safe record
/// registration and producer-consumer patterns. See `examples/` for usage patterns.
///
/// This is a thin wrapper around `AimDb<R>` that adds adapter-specific functionality.
/// Most users should use `AimDbBuilder` directly to create databases.
pub struct Database<A: RuntimeAdapter + 'static> {
    adapter: A,
    aimdb: AimDb<A>,
}

impl<A: RuntimeAdapter + 'static> Database<A> {
    /// Internal accessor for the AimDb instance
    ///
    /// Used by adapter crates. Should not be used by application code.
    #[doc(hidden)]
    pub fn inner_aimdb(&self) -> &AimDb<A> {
        &self.aimdb
    }

    /// Creates a new database from adapter and AimDb
    ///
    /// # Arguments
    /// * `adapter` - The runtime adapter
    /// * `aimdb` - The configured AimDb instance
    ///
    /// Most users should use `AimDbBuilder` directly instead of this constructor.
    pub fn new(adapter: A, aimdb: AimDb<A>) -> DbResult<Self> {
        #[cfg(feature = "tracing")]
        tracing::info!("Initializing unified database with typed records");

        Ok(Self { adapter, aimdb })
    }

    /// Gets a reference to the runtime adapter
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

    /// Produces typed data to a specific record by key
    ///
    /// # Example
    /// ```rust,ignore
    /// # fn example<A: aimdb_core::RuntimeAdapter>(db: aimdb_core::Database<A>) -> aimdb_core::DbResult<()> {
    /// db.produce("sensor.temp", SensorData { temp: 23.5 })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn produce<T>(&self, key: impl AsRef<str>, data: T) -> DbResult<()>
    where
        T: Send + 'static + Clone + core::fmt::Debug,
    {
        self.aimdb.produce(key, data)
    }

    /// Subscribes to a record by key
    ///
    /// Creates a subscription to the configured buffer for the given record key.
    /// Returns a boxed reader for receiving values asynchronously.
    ///
    /// # Example
    /// ```rust,ignore
    /// # async fn example<A: aimdb_core::RuntimeAdapter>(db: aimdb_core::Database<A>) -> aimdb_core::DbResult<()> {
    /// let mut reader = db.subscribe::<SensorData>("sensor.temp")?;
    ///
    /// loop {
    ///     match reader.recv().await {
    ///         Ok(data) => println!("Received: {:?}", data),
    ///         Err(e) => {
    ///             eprintln!("Error: {:?}", e);
    ///             break;
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn subscribe<T>(
        &self,
        key: impl AsRef<str>,
    ) -> DbResult<Box<dyn crate::buffer::BufferReader<T> + Send>>
    where
        T: Send + Sync + 'static + Debug + Clone,
    {
        self.aimdb.subscribe(key)
    }

    /// Creates a RuntimeContext for this database
    ///
    /// Provides services with access to runtime capabilities (timing, logging) plus the emitter.
    ///
    /// # Example
    /// ```rust,ignore
    /// # use aimdb_core::Database;
    /// # #[cfg(feature = "tokio-runtime")]
    /// # {
    /// # async fn example<A: aimdb_executor::Runtime + Clone>(db: Database<A>) {
    /// let ctx = db.context();
    /// // Pass ctx to services
    /// # }
    /// # }
    /// ```
    pub fn context(&self) -> RuntimeContext<A>
    where
        A: aimdb_executor::Runtime + Clone,
    {
        RuntimeContext::from_arc(Arc::new(self.adapter.clone()))
    }
}
