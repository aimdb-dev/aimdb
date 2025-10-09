//! Embassy Database Implementation
//!
//! This module provides Embassy-specific extensions to the core database,
//! including the build() method that requires an Embassy Spawner.

use crate::runtime::EmbassyAdapter;
use aimdb_core::{Database, DatabaseSpec, DatabaseSpecBuilder, Record};

#[cfg(feature = "embassy-runtime")]
use embassy_executor::Spawner;

/// Type alias for Embassy database
///
/// This provides a convenient type for working with databases on the Embassy runtime.
pub type EmbassyDatabase = Database<EmbassyAdapter>;

/// Type alias for Embassy database specification  
///
/// This is a convenience type for database specifications used with Embassy.
pub type EmbassyDatabaseSpec = DatabaseSpec<EmbassyAdapter>;

/// Type alias for Embassy-specific database specification builder
pub type EmbassyDatabaseSpecBuilder = DatabaseSpecBuilder<EmbassyAdapter>;

/// Type alias for Embassy-specific record implementation
pub type EmbassyRecord = Record;

/// Extension trait for building Embassy databases
///
/// This trait adds a build() method to DatabaseSpecBuilder<EmbassyAdapter>
/// that requires a Spawner, reflecting Embassy's runtime requirements.
#[cfg(feature = "embassy-runtime")]
pub trait EmbassyDatabaseBuilder {
    /// Builds an Embassy database from the specification
    ///
    /// This method creates a new EmbassyAdapter with the provided spawner
    /// and initializes the database with the configured records.
    ///
    /// # Arguments
    /// * `spawner` - The Embassy spawner for task management
    ///
    /// # Returns
    /// A configured Embassy database ready for use
    ///
    /// # Example
    /// ```rust,no_run
    /// # #[cfg(all(not(feature = "std"), feature = "embassy-runtime"))]
    /// # {
    /// use embassy_executor::Spawner;
    /// use aimdb_core::Database;
    /// use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyDatabaseBuilder};
    ///
    /// #[embassy_executor::main]
    /// async fn main(spawner: Spawner) {
    ///     let db = Database::<EmbassyAdapter>::builder()
    ///         .record("sensors")
    ///         .record("metrics")
    ///         .build(spawner);
    ///     
    ///     // Use the database
    /// }
    /// # }
    /// ```
    fn build(self, spawner: Spawner) -> Database<EmbassyAdapter>;
}

#[cfg(feature = "embassy-runtime")]
impl EmbassyDatabaseBuilder for DatabaseSpecBuilder<EmbassyAdapter> {
    fn build(self, spawner: Spawner) -> Database<EmbassyAdapter> {
        #[cfg(feature = "tracing")]
        tracing::info!("Building Embassy database with spawner");

        let adapter = EmbassyAdapter::new_with_spawner(spawner);
        let spec = self.into_spec();
        Database::new(adapter, spec)
    }
}
