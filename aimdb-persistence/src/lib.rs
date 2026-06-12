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
//! ```no_run
//! use aimdb_persistence::{AimDbBuilderPersistExt, RecordRegistrarPersistExt, AimDbQueryExt};
//! // A real backend, e.g. `aimdb_persistence_sqlite::SqliteBackend`:
//! # use aimdb_persistence::{BoxFuture, PersistenceBackend, PersistenceError, QueryParams, StoredValue};
//! # struct SqliteBackend;
//! # impl SqliteBackend { fn new(_p: &str) -> Result<Self, PersistenceError> { Ok(Self) } }
//! # impl PersistenceBackend for SqliteBackend {
//! #     fn store<'a>(
//! #         &'a self, _n: &'a str, _v: &'a serde_json::Value, _t: u64,
//! #     ) -> BoxFuture<'a, Result<(), PersistenceError>> { Box::pin(async { Ok(()) }) }
//! #     fn query<'a>(
//! #         &'a self, _p: &'a str, _q: QueryParams,
//! #     ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> { Box::pin(async { Ok(vec![]) }) }
//! #     fn cleanup(&self, _t: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
//! #         Box::pin(async { Ok(0) })
//! #     }
//! # }
//! # use aimdb_core::buffer::BufferCfg;
//! # use aimdb_core::AimDbBuilder;
//! # use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! # use std::sync::Arc;
//! # use std::time::Duration;
//! # #[derive(Clone, Debug, serde::Serialize)] struct MyRecord { value: f32 }
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! # let runtime = Arc::new(TokioAdapter::new()?);
//! let backend = Arc::new(SqliteBackend::new("./data/history.db")?);
//!
//! let mut builder = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_persistence(backend.clone(), Duration::from_secs(7 * 24 * 3600));
//!
//! builder.configure::<MyRecord>("my.record", |reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 500 })
//!        .persist("my.record");
//! });
//!
//! let (db, runner) = builder.build().await?;
//!
//! // Query historical data (any `DeserializeOwned` shape; `Value` shown here)
//! let latest: Vec<serde_json::Value> = db.query_latest("my_record::*", 1).await?;
//! # Ok(())
//! # }
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
