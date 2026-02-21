# aimdb-persistence

Optional persistence layer for AimDB. Adds long-term record history to any
AimDB database without touching `aimdb-core` — persistence is implemented as a
**buffer subscriber**, just like `.tap()`, keeping it fully within AimDB's
existing producer–consumer architecture.

## Overview

`aimdb-persistence` provides the traits and extension methods that wire a
pluggable storage backend into the AimDB builder and query API:

| Crate component | What it adds |
|---|---|
| [`PersistenceBackend`] trait | Interface for concrete backends (SQLite, Postgres, …) |
| `AimDbBuilderPersistExt` | `.with_persistence(backend, retention)` on the builder |
| `RecordRegistrarPersistExt` | `.persist("my_record::key")` on record configuration |
| `AimDbQueryExt` | `.query_latest()` / `.query_range()` on a live `AimDb<R>` |

For a concrete backend, add [`aimdb-persistence-sqlite`](../aimdb-persistence-sqlite/README.md).

## Installation

```toml
[dependencies]
aimdb-persistence        = "0.1"
aimdb-persistence-sqlite = "0.1"          # or another backend
```

Optional features:

```toml
# Enable structured logging via the `tracing` crate
aimdb-persistence = { version = "0.1", features = ["tracing"] }
```

## Quick Start

```rust
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::AimDbBuilder;
use aimdb_persistence::{AimDbBuilderPersistExt, AimDbQueryExt, RecordRegistrarPersistExt};
use aimdb_persistence_sqlite::SqliteBackend;
use aimdb_tokio_adapter::TokioAdapter;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Accuracy {
    value: f64,
    city: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);

    // 1. Create a backend — keeps data for 7 days.
    let backend = Arc::new(SqliteBackend::new("./data/history.db")?);

    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_persistence(backend, Duration::from_secs(7 * 24 * 3600));

    // 2. Opt individual records into persistence.
    builder.configure::<Accuracy>("accuracy::vienna", |reg| {
        reg.persist("accuracy::vienna");
    });
    builder.configure::<Accuracy>("accuracy::berlin", |reg| {
        reg.persist("accuracy::berlin");
    });

    let db = builder.build().await?;

    // 3. Query historical data.
    let latest: Vec<Accuracy> = db.query_latest("accuracy::*", 10).await?;
    println!("Latest 10 per city: {} rows total", latest.len());

    let since = 1_700_000_000_000_u64; // Unix ms
    let until = 1_800_000_000_000_u64;
    let range: Vec<Accuracy> = db.query_range("accuracy::vienna", since, until).await?;
    println!("Vienna in range: {} rows", range.len());

    Ok(())
}
```

## Architecture

```
  AimDB producer / connector
          │
          ▼
  ┌───────────────┐
  │  Record buffer │  ← typed ring / latest / SPMC
  └───────┬───────┘
          │ tap (side-effect subscriber)
          ▼
  ┌────────────────────┐
  │  Persistence sub.  │  (spawned by .persist())
  │  serde_json::to_   │
  │  value(&T)         │
  └────────┬───────────┘
           │ async write
           ▼
  ┌─────────────────────┐
  │  PersistenceBackend │  ← trait object (SqliteBackend, …)
  └─────────────────────┘
```

**Key design properties:**

- **Zero coupling to `aimdb-core`** — persistence is an optional crate; the
  core crate has no dependency on storage types.
- **Runtime-agnostic** — the subscriber serialises values synchronously and
  delegates I/O to the backend trait, which is responsible for its own async
  strategy.
- **Retention via `on_start()` hook** — a cleanup task is registered during
  `with_persistence()` and runs once at startup, then every 24 hours.

## API Reference

### Builder extension

```rust
// Configure backend + retention in one call.
builder.with_persistence(
    Arc::new(SqliteBackend::new("./db")?),
    Duration::from_secs(30 * 24 * 3600), // 30-day retention
);
```

### Record registration

```rust
builder.configure::<MyRecord>("my_record::key", |reg| {
    reg.persist("my_record::key");
    // .persist() accepts anything that converts to String:
    // reg.persist(format!("my_record::{}", city));
});
```

`T` must implement `serde::Serialize`. `.with_remote_access()` is **not**
required — the persistence subscriber taps the typed buffer directly.

### Query methods

```rust
use aimdb_persistence::AimDbQueryExt;

// Latest N values per matching record (pattern supports `*` wildcard).
let latest: Vec<MyRecord> = db.query_latest("my_record::*", 5).await?;

// All values in a time range (Unix milliseconds).
let range: Vec<MyRecord> = db
    .query_range("my_record::vienna", start_ms, end_ms)
    .await?;

// Untyped query (returns raw JSON — used by the AimX protocol handler).
use aimdb_persistence::QueryParams;
let raw = db.query_raw("my_record::*", QueryParams {
    limit_per_record: Some(1),
    ..Default::default()
}).await?;
```

`query_latest` applies `limit_per_record` rows per matching record.
`query_range` returns **all** matching rows in the time window (no implicit
truncation).

## Error Handling

```rust
use aimdb_persistence::PersistenceError;

match db.query_latest::<MyRecord>("my_record::*", 10).await {
    Ok(rows) => { /* … */ }
    Err(PersistenceError::NotConfigured) => {
        // .with_persistence() was not called on the builder
    }
    Err(PersistenceError::Backend(msg)) => {
        // Storage-layer error (e.g. disk full, SQLite locked)
        eprintln!("Backend error: {msg}");
    }
    Err(PersistenceError::BackendShutdown) => {
        // Writer thread or channel unexpectedly closed
    }
    Err(e) => eprintln!("Other: {e}"),
}
```

Rows that fail to deserialise as `T` are skipped with a `tracing::warn!`
(when the `tracing` feature is enabled) rather than failing the entire query.

## Implementing a Custom Backend

```rust
use aimdb_persistence::backend::{BoxFuture, PersistenceBackend, QueryParams, StoredValue};
use aimdb_persistence::PersistenceError;
use serde_json::Value;

struct MyBackend { /* … */ }

impl PersistenceBackend for MyBackend {
    fn store<'a>(
        &'a self,
        record_name: &'a str,
        value: &'a Value,
        timestamp: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        Box::pin(async move {
            // persist (record_name, value, timestamp) …
            Ok(())
        })
    }

    fn query<'a>(
        &'a self,
        record_pattern: &'a str,
        params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
        Box::pin(async move {
            // query by pattern, honouring params.limit_per_record /
            // start_time / end_time …
            Ok(vec![])
        })
    }

    fn cleanup(&self, older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async move {
            // delete rows where stored_at < older_than, return count deleted
            Ok(0)
        })
    }
}
```

## Features

| Feature | Default | Description |
|---|---|---|
| `std` | yes | Enables `std`-backed types; required for Tokio runtimes |
| `tracing` | no | Emit structured log events via the `tracing` crate |

## Related Crates

| Crate | Role |
|---|---|
| [`aimdb-persistence-sqlite`](../aimdb-persistence-sqlite/README.md) | SQLite backend (bundled, WAL mode, dedicated writer thread) |
| [`aimdb-core`](../aimdb-core/README.md) | Core AimDB types; `Extensions` TypeMap |
| [`aimdb-tokio-adapter`](../aimdb-tokio-adapter/README.md) | Tokio runtime adapter |

## License

See [LICENSE](../LICENSE).
