//! Tokio Database Implementation
//!
//! This module provides Tokio-specific extensions to the core database,
//! including the build() method for easy initialization.

use crate::runtime::TokioAdapter;
use aimdb_core::{Database, DatabaseSpec, DatabaseSpecBuilder, DbResult};

/// Type alias for Tokio database
///
/// This provides a convenient type for working with databases on the Tokio runtime.
pub type TokioDatabase = Database<TokioAdapter>;

/// Type alias for Tokio database specification
pub type TokioDatabaseSpec = DatabaseSpec<TokioAdapter>;

/// Type alias for Tokio database specification builder
pub type TokioDatabaseSpecBuilder = DatabaseSpecBuilder<TokioAdapter>;

/// Extension trait for building Tokio databases
///
/// This trait adds a build() method to DatabaseSpecBuilder<TokioAdapter>,
/// enabling clean initialization syntax.
pub trait TokioDatabaseBuilder {
    /// Builds a Tokio database from the specification
    ///
    /// This method creates a new TokioAdapter and initializes the database
    /// with the configured records.
    ///
    /// # Returns
    /// `DbResult<Database<TokioAdapter>>` - The configured database
    ///
    /// # Example
    /// ```rust,no_run
    /// use aimdb_core::Database;
    /// use aimdb_tokio_adapter::{TokioAdapter, TokioDatabaseBuilder};
    ///
    /// #[tokio::main]
    /// async fn main() -> aimdb_core::DbResult<()> {
    ///     let db = Database::<TokioAdapter>::builder()
    ///         .record("sensors")
    ///         .record("metrics")
    ///         .build()?;
    ///     
    ///     // Use the database
    ///     Ok(())
    /// }
    /// ```
    fn build(self) -> DbResult<Database<TokioAdapter>>;
}

impl TokioDatabaseBuilder for DatabaseSpecBuilder<TokioAdapter> {
    fn build(self) -> DbResult<Database<TokioAdapter>> {
        #[cfg(feature = "tracing")]
        tracing::info!("Building Tokio database");

        let adapter = TokioAdapter::new()?;
        let spec = self.into_spec();
        Ok(Database::new(adapter, spec))
    }
}
