//! # aimdb-persistence
//!
//! Optional persistence layer for AimDB. Persistence is implemented as a
//! **buffer subscriber** — just like `.tap()` — keeping it fully within
//! AimDB's existing producer–consumer architecture.
//!
//! This crate provides:
//!
//! - [`PersistenceBackend`] trait for pluggable backends (SQLite, Postgres, …)
//! - [`AimDbBuilderPersistExt`] — adds `.with_persistence()` to the builder
//! - [`RecordRegistrarPersistExt`] — adds `.persist()` to record registration
//! - [`AimDbQueryExt`] — adds `.query_latest()` / `.query_range()` to `AimDb<R>`
//!
//! # Usage
//!
//! ```rust,ignore
//! use aimdb_persistence::{AimDbBuilderPersistExt, RecordRegistrarPersistExt, AimDbQueryExt};
//! use aimdb_persistence_sqlite::SqliteBackend;
//!
//! let backend = Arc::new(SqliteBackend::new("./data/history.db")?);
//!
//! let mut builder = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_persistence(backend.clone(), Duration::from_secs(7 * 24 * 3600));
//!
//! builder.configure::<MyRecord>(key, |reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 500 })
//!        .persist(key.to_string());
//! });
//!
//! let db = builder.build().await?;
//!
//! // Query historical data
//! let latest: Vec<MyRecord> = db.query_latest("my_record::*", 1).await?;
//! ```

pub mod backend;
pub mod builder_ext;
pub mod error;
pub mod ext;
pub mod query_ext;

// Re-exports for convenience
pub use backend::{BoxFuture, PersistenceBackend, QueryParams, StoredValue};
pub use builder_ext::{AimDbBuilderPersistExt, PersistenceState};
pub use error::PersistenceError;
pub use ext::RecordRegistrarPersistExt;
pub use query_ext::AimDbQueryExt;
