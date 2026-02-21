# aimdb-persistence-sqlite

SQLite persistence backend for AimDB. Stores long-term record history in a
local SQLite database using WAL mode and a dedicated writer thread so the async
executor is never blocked.

> **Runtime requirement:** Requires a **Tokio** runtime. The writer thread
> communicates with async callers via `tokio::sync::oneshot`. Do not use this
> crate with the Embassy adapter — it will not link.

## Installation

```toml
[dependencies]
aimdb-persistence        = "0.1"
aimdb-persistence-sqlite = "0.1"
```

SQLite is **bundled** (via `rusqlite`'s `bundled` feature) — no system SQLite
is needed.

Optional features:

```toml
aimdb-persistence-sqlite = { version = "0.1", features = ["tracing"] }
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
struct Accuracy { value: f64, city: String }

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);

    // Open (or create) the database file.
    let backend = Arc::new(SqliteBackend::new("./data/history.db")?);

    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_persistence(backend, Duration::from_secs(7 * 24 * 3600));

    builder.configure::<Accuracy>("accuracy::vienna", |reg| {
        reg.persist("accuracy::vienna");
    });

    let db = builder.build().await?;

    let latest: Vec<Accuracy> = db.query_latest("accuracy::*", 5).await?;
    println!("{} rows returned", latest.len());

    Ok(())
}
```

## Architecture

```
  async caller (AimDB task)
        │
        │  mpsc::SyncSender<DbCommand>  (bound = 64)
        ▼
  ┌─────────────────────────────────────┐
  │  aimdb-sqlite  OS thread            │
  │                                     │
  │  rusqlite::Connection  (WAL mode)   │
  │  ┌──────────────────────────────┐   │
  │  │  record_history table        │   │
  │  │  id, record_name, value_json │   │
  │  │  stored_at (Unix ms, i64)    │   │
  │  └──────────────────────────────┘   │
  └─────────────────────────────────────┘
        │
        │  tokio::sync::oneshot  (reply)
        ▼
  async caller receives Result<_, PersistenceError>
```

**Key design properties:**

- **Non-blocking writes** — each `store()` call enqueues a `DbCommand` and
  `await`s a oneshot reply; the writer thread does all SQLite I/O synchronously
  on its own OS thread.
- **Single writer, concurrent readers** — WAL mode allows readers (queries) to
  proceed concurrently with the single writer.
- **Prepared-statement cache** — `prepare_cached()` is used for all hot paths
  (INSERT, DELETE, SELECT) to avoid repeated SQL compilation.
- **Graceful shutdown** — the writer thread exits cleanly when all
  `SqliteBackend` clones (i.e. all `SyncSender` handles) are dropped.

## Database Schema

```sql
CREATE TABLE IF NOT EXISTS record_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    record_name TEXT    NOT NULL,
    value_json  TEXT    NOT NULL,
    stored_at   INTEGER NOT NULL   -- Unix timestamp in milliseconds (i64)
);

CREATE INDEX IF NOT EXISTS idx_record_time
    ON record_history(record_name, stored_at DESC);
```

The `WITH … ROW_NUMBER()` window function is used for efficient "top-N per
group" queries without a full table scan.

## API

### `SqliteBackend::new(path)`

Opens (or creates) a database at `path`. Schema and WAL mode are initialised
synchronously before the writer thread is spawned — no `block_on` needed, no
Tokio runtime required at construction time.

```rust
let backend = SqliteBackend::new("/var/db/aimdb.sqlite")?;
let backend = SqliteBackend::new(":memory:")?; // in-memory (tests)
```

Returns `Err(PersistenceError::Backend(_))` if the file cannot be opened or
the schema cannot be applied.

### Implemented trait methods

| Method | Description |
|---|---|
| `store(name, value, timestamp)` | Insert one row |
| `query(pattern, params)` | Window-function SELECT with optional `*` wildcard, time range, and per-record limit |
| `cleanup(older_than)` | DELETE rows where `stored_at < older_than`; returns row count |

### `QueryParams`

```rust
QueryParams {
    limit_per_record: Some(10),     // None → return all rows in window
    start_time:       Some(ms),     // None → no lower bound
    end_time:         Some(ms),     // None → no upper bound
}
```

### Pattern matching

`record_name LIKE ?1 ESCAPE '\\'` is used for pattern queries. The AimDB
wildcard `*` is mapped to SQL `%`; literal `%` and `_` in record names are
escaped automatically. Only `*` is recognised as a wildcard — `?` is treated
as a literal character.

### Timestamp handling

Timestamps are stored as SQLite `INTEGER` (signed 64-bit). `u64` values from
the persistence layer are checked with `i64::try_from` before insertion;
out-of-range values (> `i64::MAX`) are rejected with
`PersistenceError::Backend` rather than silently truncated.

## Features

| Feature | Default | Description |
|---|---|---|
| `tracing` | no | Emit structured log events (corrupted rows, writer thread lifecycle) |

## Testing

```bash
# Unit tests (uses in-memory / tempfile databases, no external dependencies)
cargo test -p aimdb-persistence-sqlite
```

The test suite covers:
- Store and wildcard query
- Time-range query with boundary inclusion
- Retention cleanup (row count verification)
- SQL LIKE pattern escaping (`_` must not match arbitrary characters)

## Related Crates

| Crate | Role |
|---|---|
| [`aimdb-persistence`](../aimdb-persistence/README.md) | Trait definitions and builder/query extension traits |
| [`aimdb-core`](../aimdb-core/README.md) | Core AimDB builder and `Extensions` TypeMap |
| [`aimdb-tokio-adapter`](../aimdb-tokio-adapter/README.md) | Required Tokio runtime adapter |

## License

See [LICENSE](../LICENSE).
