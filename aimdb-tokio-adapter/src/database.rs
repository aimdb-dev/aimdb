//! Tokio Database Implementation
//!
//! This module provides a simplified Tokio-specific database implementation
//! that wraps the core database with Tokio runtime capabilities.

use crate::runtime::TokioAdapter;
use aimdb_core::{Database, DatabaseSpec, DatabaseSpecBuilder, Record, Runnable};

/// Tokio database implementation
///
/// A simple wrapper around the core database that provides Tokio runtime integration.
pub struct TokioDatabase(Database<TokioAdapter>);

impl TokioDatabase {
    /// Creates a new Tokio database instance
    pub fn new(adapter: TokioAdapter, spec: TokioDatabaseSpec) -> Self {
        Self(Database::new(adapter, spec))
    }

    /// Gets a record handle by name
    pub fn record(&self, name: &str) -> Record {
        self.0.record(name)
    }

    /// Gets access to the underlying adapter for service spawning
    pub fn adapter(&self) -> &TokioAdapter {
        self.0.adapter()
    }
}

/// Tokio database specification type alias
pub type TokioDatabaseSpec = DatabaseSpec<TokioAdapter>;

/// Tokio-specific database specification builder
pub type TokioDatabaseSpecBuilder = DatabaseSpecBuilder<TokioAdapter>;

/// Creates a new Tokio database instance
///
/// # Example
/// ```rust,no_run
/// use aimdb_tokio_adapter::{new_database, TokioDatabaseSpec};
///
/// #[tokio::main]
/// async fn main() {
///     let spec = TokioDatabaseSpec::builder().build();
///     let db = new_database(spec).unwrap();
/// }
/// ```
pub fn new_database(spec: TokioDatabaseSpec) -> aimdb_core::DbResult<TokioDatabase> {
    #[cfg(feature = "tracing")]
    tracing::info!("Creating Tokio database");

    let adapter = TokioAdapter::new()?;
    Ok(TokioDatabase::new(adapter, spec))
}

/// Tokio-specific Runnable implementation
impl Runnable for TokioDatabase {
    async fn run(self) {
        #[cfg(feature = "tracing")]
        tracing::info!("Starting Tokio database");

        // For now, just delegate to the core database
        // TODO: Implement Tokio-specific database operations
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            #[cfg(feature = "tracing")]
            tracing::trace!("Tokio database loop");
        }
    }
}
