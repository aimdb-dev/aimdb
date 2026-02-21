# Persistence Backend

**Version:** 1.1  
**Status:** 📋 Proposed  
**Last Updated:** February 19, 2026  
**Milestone:** M10 — Persistence & Long-Term History  
**Depends On:** [019-M8-record-history-api](019-M8-record-history-api.md)

---

## Summary

Extend AimDB with an **optional persistence layer** implemented as a
**reader** — just like `.tap()` and `.transform()`. A persistence subscriber
receives values from the typed buffer via `.tap_raw()`, serializes them, and
writes them to a pluggable backend (initially SQLite). This keeps persistence
fully within AimDB's existing producer–consumer architecture: no new write
paths, no special-casing in the core.

**Core principle:** AimDB remains a real-time streaming database. Persistence
is a buffer subscriber, not a fundamental change to the architecture.

---

## Motivation: The Forecast Validation Problem

Validation messages arrive **~1 per hour per city**. Combined with
\`simulatable: false\`, this creates a UX problem:

| Scenario | Issue |
|----------|-------|
| **Page refresh** | AccuracyPanel empty — all in-memory data lost |
| **New visitor** | Panel empty until first validation matures (~1 hour wait) |
| **Demo mode** | Panel permanently empty — no live data source |

The in-memory \`record.drain\` API (doc-019) can't solve this because:

- Ring buffer clears on restart
- No way to query "latest validation per city" across restarts
- ~24-slot ring at 1/hour = only 1 day of history max

**Solution:** Persist validation records to SQLite. On page load, query
"latest validation per city" from persistent storage.

---

## Architecture

```

                            AimDb<R>                                      │
                                                                         │
   │  ┌──────────────────────────────────
  │   validation::vienna    validation::berlin    validation::...   │   │
  │   (ring buffer)         (ring buffer)         (ring buffer)     │   │
   │  └─────────
              │                     │                     │              │
              │ subscribe() + to_value() (T: Serialize) │               │
              ▼                     ▼                     ▼              │
   │  ┌──────
  │              Persistence Subscriber (background task)            │   │
  │              recv() → backend.store()                            │   │
   │  └─────────────────────────────
                                     │                                   │
                                     ▼                                   │
   │  ┌──
  │              SqliteBackend                                       │   │
  │              ./data/history.db                                   │   │
   │  └─────────────────────────
                                     │                                   │
   │  ┌────────────────────────
                     Query Methods                                │   │  
  │                                                                  │   │
  │  db.query_latest::<T>("accuracy::*", 1)     // latest per record │   │
  │  db.query_latest::<T>("accuracy::vienna", 10)                   │   │
  │  db.query_range::<T>("accuracy::vienna", start, end)            │   │
  └─────────────────────────────────────────────────────────────────┘   │

```

**Data Flow:**

```
WRITE: db.set("validation::vienna", value)
       │
       ├──► Ring buffer (in-memory, for real-time subscribers)
       │
       └──► Persistence subscriber → SQLite


READ:  db.query_latest::<ForecastValidation>("accuracy::*", 1)  // 1 per matching record
       │
       └──► SQLite → deserialize → Vec<ForecastValidation>
```

---

## Configuration API

```rust
use aimdb_persistence_sqlite::SqliteBackend;
use aimdb_persistence::{AimDbBuilderPersistExt, RecordRegistrarPersistExt};

let backend = Arc::new(SqliteBackend::new("./data/validations.db")?);

// Builder level: configure the persistence backend once.
// with_persistence() registers the cleanup task via on_start() and stores
// the backend in the builder's Extensions TypeMap — no block_on needed,
// because SqliteBackend::new() initializes the schema synchronously.
let mut builder = AimDbBuilder::new()
    .runtime(TokioAdapter::new())
    .with_persistence(backend.clone(), Duration::from_secs(7 * 24 * 3600));

// Record level: opt-in to persistence alongside other buffer config.
// No backend argument — .persist() retrieves the backend from the
// builder's Extensions TypeMap via extensions().get::<PersistenceState>().
// T: Serialize is required; .with_remote_access() is NOT required.
builder.configure::<ForecastValidation>(accuracy_key, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 500 })
        .tap(move |ctx, consumer| ws_tap(ctx, consumer, tx.clone(), accuracy_key.as_str()))
        .persist(accuracy_key.to_string())
        .transform::<Temperature, _>(temp_key, move |t| {
            t.with_state(ValidationState::new(trackers, city, tolerance))
                .on_value(validate_one)
        });
});

let db = builder.build().await?;
```

**Key insight:** `.persist()` is a subscriber to the buffer, just like `.tap()`. It uses
`tap_raw()` under the hood, so it goes through the runtime's `Spawn` trait — no
`tokio::spawn` hardcoding. `T: Serialize` is required; `.with_remote_access()` is not.

### Query API

```rust
// Latest validation per city (for AccuracyPanel on page load)
// Pattern "accuracy::*" matches all cities; limit=1 returns latest per record
let by_city: Vec<ForecastValidation> = db.query_latest(
    "accuracy::*",
    1,
).await?;

// Last 10 validations for one city
let history: Vec<ForecastValidation> = db.query_latest(
    "accuracy::vienna",
    10,
).await?;

// Time range query
let range: Vec<ForecastValidation> = db.query_range(
    "accuracy::vienna",
    start_ts,
    end_ts,
).await?;
```

**Key simplification:** Since each city has its own record (`accuracy::vienna`, `accuracy::berlin`, etc.),
the record name *is* the natural grouping key. No need for a separate `group_by` field.

---

## Implementation

### Crate Structure

```
aimdb/
 aimdb-persistence/             # Backend trait + .persist() extension
   └── src/
       ├── lib.rs
       ├── backend.rs             # PersistenceBackend trait
       ├── builder_ext.rs         # AimDbBuilderPersistExt trait (adds .with_persistence())
       ├── ext.rs                 # RecordRegistrarPersistExt trait (adds .persist())
       └── query_ext.rs           # AimDbQueryExt trait (adds .query_latest() / .query_range())
 aimdb-persistence-sqlite/      # SQLite implementation (requires Tokio runtime)
   └── src/lib.rs
```

> **Runtime requirement:** `aimdb-persistence-sqlite` requires a Tokio runtime.
> It uses `tokio::sync::oneshot` to bridge the blocking SQLite writer thread and
> the async caller. This is the crate's only Tokio coupling — the rest of the
> persistence stack (`aimdb-persistence`, the `PersistenceBackend` trait, the
> `.persist()` subscriber) is runtime-agnostic. Future backends such as
> `aimdb-persistence-postgres` (via `sqlx` or `tokio-postgres`) will share this
> same pattern, making the Tokio dependency consistent across all concrete
> backends. Do **not** use `SqliteBackend` with the Embassy adapter — it will
> not compile without a Tokio executor.

**Key design decision:** `aimdb-persistence` does **not** live inside `aimdb-core`.
It extends both `RecordRegistrar` (via `RecordRegistrarPersistExt`) and `AimDbBuilder`
(via `AimDbBuilderPersistExt`) from outside, exactly like `TokioRecordRegistrarExt`
adds `.tap()` and `.transform()` without touching the core. This keeps `aimdb-core`
free of persistence concerns entirely.

### Extensions TypeMap (in `aimdb-core`)

`aimdb-core` exposes a generic extension slot on both `AimDbBuilder<R>` and `AimDb<R>`.
This replaces the two ad-hoc `set_persistence_backend_any` / `set_persistence_backend`
hooks with a single, reusable mechanism any external crate can use — persistence,
metrics, custom middleware, etc.:

```rust
// aimdb-core/src/extensions.rs
use std::any::{Any, TypeId};
use hashbrown::HashMap; // already a dependency of aimdb-core

/// Generic extension storage for `AimDbBuilder` and `AimDb`.
/// External crates store typed state here during builder configuration
/// and retrieve it during record setup or at query time.
pub struct Extensions {
    map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Extensions {
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(val));
    }

    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.map.get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref())
    }
}
```

Both `AimDbBuilder<R>` and `AimDb<R>` expose:

```rust
impl<R> AimDbBuilder<R> {
    pub fn extensions(&self) -> &Extensions { &self.extensions }
    pub fn extensions_mut(&mut self) -> &mut Extensions { &mut self.extensions }
}

// AimDb<R> exposes a read-only view (extensions are frozen after build())
impl<R> AimDb<R> {
    pub fn extensions(&self) -> &Extensions { &self.extensions }
}
```

The builder moves its `Extensions` into `AimDb<R>` during `build()` — same
lifetime as the runtime adapter.

### Backend Trait

```rust
// aimdb-persistence/src/backend.rs

// No async_trait — same manual Pin<Box<dyn Future>> pattern used throughout aimdb-core.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait PersistenceBackend: Send + Sync {
    /// Store a value for a record.
    fn store<'a>(
        &'a self,
        record_name: &'a str,
        value: &'a Value,
        timestamp: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>>;

    /// Query with pattern support ("accuracy::*" matches all accuracy records).
    fn query<'a>(
        &'a self,
        record_pattern: &'a str,
        params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>>;

    /// Initialize storage (create tables, indexes).
    /// Default implementation is a no-op — backends that perform setup
    /// eagerly in `::new()` (like `SqliteBackend`) do not need to override
    /// this. Future backends (e.g. Postgres) can override it for async setup.
    fn initialize(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    /// Delete all rows older than `older_than` (Unix ms). Called automatically
    /// by the retention cleanup task registered during `with_persistence()`.
    /// Can also be called explicitly if needed.
    fn cleanup(&self, older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>>;
}

#[derive(Debug, Clone)]
pub struct StoredValue {
    pub record_name: String,
    pub value: Value,
    pub stored_at: u64,
}

#[derive(Debug, Clone, Default)]
pub struct QueryParams {
    pub limit_per_record: Option<usize>,  // For patterns: limit per matching record
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
}
```

### AimDb Query Methods

Query methods live in `aimdb-persistence/src/query_ext.rs` as an extension trait,
keeping `aimdb-core` free of `serde_json`, `DeserializeOwned`, and `PersistenceError`.
Error type is `PersistenceError` (not `DbError`), so the core error enum gains no
persistence-specific variants. Users import `use aimdb_persistence::AimDbQueryExt;`.
Type safety is enforced by convention: the record name is the type tag.
`accuracy::vienna` will only ever contain `ForecastValidation` because that is what
`.persist()` was called with. Deserialization failures are handled by **skipping the
bad row and logging**, so one corrupt or migrated row never breaks the AccuracyPanel.

```rust
// aimdb-persistence/src/query_ext.rs

pub trait AimDbQueryExt {
    /// Query latest N values per matching record.
    ///
    /// Pattern support: "accuracy::*" returns latest N from each matching record.
    /// Single record: "accuracy::vienna" returns latest N from that record only.
    ///
    /// Rows that fail to deserialize as `T` are skipped with a tracing warning
    /// rather than failing the entire query.
    fn query_latest<T: DeserializeOwned>(
        &self,
        record_pattern: &str,
        limit_per_record: usize,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>>;

    /// Query values within a time range for a single record or pattern.
    ///
    /// Rows that fail to deserialize as `T` are skipped with a tracing warning.
    fn query_range<T: DeserializeOwned>(
        &self,
        record_name: &str,
        start_ts: u64,
        end_ts: u64,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>>;
}

impl<R: RuntimeAdapter> AimDbQueryExt for AimDb<R> {
    fn query_latest<T: DeserializeOwned>(
        &self,
        record_pattern: &str,
        limit_per_record: usize,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>> {
        Box::pin(async move {
            let backend = self.extensions().get::<PersistenceState>()
                .map(|s| s.backend.clone())
                .ok_or(PersistenceError::NotConfigured)?;

            let stored = backend.query(record_pattern, QueryParams {
                limit_per_record: Some(limit_per_record),
                ..Default::default()
            }).await?;

            Ok(stored.into_iter()
                .filter_map(|sv| {
                    serde_json::from_value(sv.value).map_err(|e| {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            "Skipping persisted row for '{}': deserialization failed: {}",
                            sv.record_name, e
                        );
                    }).ok()
                })
                .collect())
        })
    }

    fn query_range<T: DeserializeOwned>(
        &self,
        record_name: &str,
        start_ts: u64,
        end_ts: u64,
    ) -> BoxFuture<'_, Result<Vec<T>, PersistenceError>> {
        Box::pin(async move {
            let backend = self.extensions().get::<PersistenceState>()
                .map(|s| s.backend.clone())
                .ok_or(PersistenceError::NotConfigured)?;

            let stored = backend.query(record_name, QueryParams {
                start_time: Some(start_ts),
                end_time: Some(end_ts),
                ..Default::default()
            }).await?;

            Ok(stored.into_iter()
                .filter_map(|sv| {
                    serde_json::from_value(sv.value).map_err(|e| {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            "Skipping persisted row for '{}': deserialization failed: {}",
                            sv.record_name, e
                        );
                    }).ok()
                })
                .collect())
        })
    }
}
```

### SQLite Backend

The `Connection` is owned exclusively by a dedicated writer thread (the actor).
Async callers communicate via channels and await a `oneshot` reply. The
`Connection` never touches the async executor and needs no mutex at all.

```rust
// aimdb-persistence-sqlite/src/lib.rs

enum DbCommand {
    Store {
        record_name: String,
        json: String,
        timestamp: u64,
        reply: oneshot::Sender<Result<(), PersistenceError>>,
    },
    Query {
        pattern: String,
        params: QueryParams,
        reply: oneshot::Sender<Result<Vec<StoredValue>, PersistenceError>>,
    },
    Cleanup {
        older_than: u64,
        reply: oneshot::Sender<Result<u64, PersistenceError>>,
    },
}

/// SQLite persistence backend.
///
/// Owns a dedicated OS thread that holds the `rusqlite::Connection`.
/// All async callers send `DbCommand` messages via a `std::sync::mpsc` channel
/// and await a `tokio::sync::oneshot` reply. The async executor is never
/// blocked; the writer thread is never awaited.
#[derive(Clone)]
pub struct SqliteBackend {
    tx: std::sync::mpsc::SyncSender<DbCommand>,
}

impl SqliteBackend {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, PersistenceError> {
        // std::sync::mpsc::sync_channel so the writer thread can call recv()
        // without a Tokio runtime. Bound of 64 provides backpressure.
        let (tx, rx) = std::sync::mpsc::sync_channel::<DbCommand>(64);
        let conn = Connection::open(path)?;

        // Enable WAL mode: readers and the single writer proceed concurrently
        // without blocking each other. Free performance win, best practice.
        conn.pragma_update(None, "journal_mode", "WAL")?;

        // Initialize schema right here, before spawning the thread.
        // The connection isn't shared yet — no concurrency concerns, no block_on.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS record_history (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                record_name TEXT    NOT NULL,
                value_json  TEXT    NOT NULL,
                stored_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_record_time
                ON record_history(record_name, stored_at DESC);"
        )?;

        std::thread::Builder::new()
            .name("aimdb-sqlite".to_string())
            .spawn(move || run_db_thread(conn, rx))?;

        Ok(Self { tx })
    }
}

/// Blocking event loop — runs entirely on the dedicated thread.
/// `std::sync::mpsc::Receiver::recv()` blocks the OS thread until a command
/// arrives or all `SyncSender` handles are dropped (returning `Err`).
fn run_db_thread(conn: Connection, rx: std::sync::mpsc::Receiver<DbCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            DbCommand::Store { record_name, json, timestamp, reply } => {
                let result = conn.execute(
                    "INSERT INTO record_history (record_name, value_json, stored_at)
                     VALUES (?1, ?2, ?3)",
                    params![record_name, json, timestamp as i64],
                ).map(|_| ()).map_err(PersistenceError::from);
                let _ = reply.send(result);
            }

            DbCommand::Query { pattern, params, reply } => {
                let result = query_sync(&conn, &pattern, params);
                let _ = reply.send(result);
            }

            DbCommand::Cleanup { older_than, reply } => {
                let result = conn.execute(
                    "DELETE FROM record_history WHERE stored_at < ?1",
                    params![older_than as i64],
                ).map(|n| n as u64).map_err(PersistenceError::from);
                let _ = reply.send(result);
            }
        }
    }
}

/// Escape SQL LIKE special characters, then replace `*` with `%`.
/// Record names are documented to match `[a-zA-Z0-9_.:-]`, so `%` and `_`
/// should never appear in practice — but we escape defensively.
fn sanitize_pattern(pattern: &str) -> String {
    pattern
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
        .replace('*', "%")
}

fn query_sync(
    conn: &Connection,
    pattern: &str,
    params: QueryParams,
) -> Result<Vec<StoredValue>, PersistenceError> {
    let limit = params.limit_per_record.unwrap_or(100) as i64;
    let sql_pattern = sanitize_pattern(pattern);

    let mut stmt = conn.prepare(
        "WITH ranked AS (
            SELECT record_name, value_json, stored_at,
                   ROW_NUMBER() OVER (PARTITION BY record_name ORDER BY stored_at DESC, id DESC) AS rn
            FROM record_history
            WHERE record_name LIKE ?1 ESCAPE '\\'
              AND (?2 IS NULL OR stored_at >= ?2)
              AND (?3 IS NULL OR stored_at <= ?3)
        )
        SELECT record_name, value_json, stored_at
        FROM ranked WHERE rn <= ?4
        ORDER BY record_name, stored_at DESC"
    )?;

    let rows = stmt.query_map(
        params![
            sql_pattern,
            params.start_time.map(|t| t as i64),
            params.end_time.map(|t| t as i64),
            limit,
        ],
        |row| {
            Ok(StoredValue {
                record_name: row.get(0)?,
                value: serde_json::from_str(&row.get::<_, String>(1)?)
                    .unwrap_or(Value::Null),
                stored_at: row.get::<_, i64>(2)? as u64,
            })
        },
    )?;

    rows.collect::<Result<Vec<_>, _>>().map_err(PersistenceError::from)
}

// Helper: enqueue a command via std::sync::mpsc (sync, non-blocking within bound)
// and await the oneshot reply from the writer thread.
macro_rules! send_cmd {
    ($tx:expr, $cmd:expr) => {{
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        $tx.send($cmd(reply_tx)).map_err(|_| PersistenceError::BackendShutdown)?;
        reply_rx.await.map_err(|_| PersistenceError::BackendShutdown)?
    }};
}

impl PersistenceBackend for SqliteBackend {
    // initialize() uses the trait default (no-op) — schema was created in ::new().

    fn store<'a>(
        &'a self,
        record_name: &'a str,
        value: &'a Value,
        timestamp: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        let json = match serde_json::to_string(value) {
            Ok(j) => j,
            Err(e) => return Box::pin(async move { Err(PersistenceError::from(e)) }),
        };
        let record_name = record_name.to_string();
        Box::pin(async move {
            send_cmd!(self.tx, |reply| DbCommand::Store {
                record_name,
                json,
                timestamp,
                reply,
            })
        })
    }

    fn query<'a>(
        &'a self,
        record_pattern: &'a str,
        params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
        let pattern = record_pattern.to_string();
        Box::pin(async move {
            send_cmd!(self.tx, |reply| DbCommand::Query { pattern, params, reply })
        })
    }

    fn cleanup(&self, older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async move {
            send_cmd!(self.tx, |reply| DbCommand::Cleanup { older_than, reply })
        })
    }
}
```

**Key properties of this design:**
- `Connection` is owned by `run_db_thread` — no `Mutex`, no `Arc`, no `spawn_blocking`.
- Schema and WAL mode are configured in `::new()` before the thread is spawned — no `block_on` needed anywhere.
- The async executor is never blocked; it only `.await`s the `oneshot` reply channel.
- WAL mode allows concurrent readers and the single writer without blocking.
- Time-range filtering is pushed into SQL (the `?2`/`?3` params) rather than done in Rust.
- SQL wildcards are escaped via `sanitize_pattern()` — `%` and `_` in names cannot corrupt queries.
- `SqliteBackend` is `Clone` (it just clones the `mpsc::Sender`) — safe to share across per-city `.persist()` calls.
- Graceful shutdown: when all `SyncSender` handles are dropped, `rx.recv()` returns `Err` and the thread exits cleanly.

### `.persist()` Extension

`T` is statically known at the call site on `RecordRegistrar<'a, T, R>`, so the
persistence subscriber can subscribe to the **typed buffer** directly and call
`serde_json::to_value()` itself. This eliminates the dependency on
`.with_remote_access()` entirely.

```rust
// aimdb-persistence/src/ext.rs

pub trait RecordRegistrarPersistExt<'a, T, R>
where
    T: serde::Serialize + Send + Sync + Clone + 'static,
    R: Spawn + 'static,
{
    /// Opt this record into persistence. Spawns a background subscriber that
    /// serializes each value to JSON and writes it to the configured backend.
    /// Retention is managed by the cleanup task AimDB spawns during `build()`
    /// when a retention duration is set on `with_persistence()`.
    ///
    /// Requires `T: Serialize`. Does NOT require `.with_remote_access()`.
    fn persist(
        &'a mut self,
        record_name: String,
    ) -> &'a mut RecordRegistrar<'a, T, R>;
}

impl<'a, T, R> RecordRegistrarPersistExt<'a, T, R> for RecordRegistrar<'a, T, R>
where
    T: serde::Serialize + Send + Sync + Clone + Debug + 'static,
    R: Spawn + 'static,
{
    fn persist(
        &'a mut self,
        record_name: String,
    ) -> &'a mut RecordRegistrar<'a, T, R> {
        // Retrieve the backend from the builder's Extensions TypeMap.
        // with_persistence() stored PersistenceState there; .persist() retrieves it here.
        // No Any downcast dance needed — TypeId lookup returns the typed value directly.
        let backend: Arc<dyn PersistenceBackend> = self
            .extensions()
            .get::<PersistenceState>()
            .map(|s| s.backend.clone())
            .expect(".persist() called but no backend configured via with_persistence()");

        self.tap_raw(move |consumer, _ctx| async move {
            let mut reader = match consumer.subscribe() {
                Ok(r) => r,
                Err(_) => return,
            };
            loop {
                match reader.recv().await {
                    Ok(value) => {
                        // T is known here — no with_remote_access() needed
                        let json = match serde_json::to_value(&*value) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let _ = backend.store(&record_name, &json, now_ms()).await;
                    }
                    Err(_) => break,
                }
            }
        })
    }
}
```

**Consequences:**
- No `tokio::spawn` — uses `runtime.spawn()` via `tap_raw`, respecting the `R: Spawn` abstraction.
- No `subscribe_json()` — bypasses the `json_serializer` gate entirely.
- Timestamp is always `now_ms()` (wall-clock at write time), not extracted from payload fields.

### Retention Cleanup Task

`with_persistence()` is defined in `aimdb-persistence` as an extension trait on
`AimDbBuilder<R>` — `aimdb-core` never sees it. It uses two hooks exposed by
`aimdb-core`: `on_start()` for the cleanup loop, and `extensions_mut()` for
backend storage. A `PersistenceState` newtype gives it a unique `TypeId`:

```rust
// aimdb-persistence/src/builder_ext.rs

/// Wrapper stored in the Extensions TypeMap. Unique TypeId prevents collisions
/// with any other crate that uses the same map.
pub struct PersistenceState {
    pub backend: Arc<dyn PersistenceBackend>,
    /// Retention window in milliseconds (Unix ms). Stored as `u64` so it can
    /// be compared directly with `stored_at` timestamps.
    pub retention_ms: u64,
}

pub trait AimDbBuilderPersistExt<R: Spawn + TimeOps> {
    /// Configure a persistence backend with a retention window.
    ///
    /// Stores the backend in the builder's Extensions TypeMap (accessible to
    /// `.persist()` and `AimDbQueryExt` methods) and registers an `on_start()`
    /// task that runs an initial cleanup sweep then repeats every 24 hours.
    /// No `block_on` is needed — `SqliteBackend::new()` initializes the schema.
    fn with_persistence(
        self,
        backend: Arc<dyn PersistenceBackend>,
        retention: Duration,
    ) -> Self;
}

impl<R: Spawn + TimeOps + 'static> AimDbBuilderPersistExt<R> for AimDbBuilder<R> {
    fn with_persistence(
        mut self,
        backend: Arc<dyn PersistenceBackend>,
        retention: Duration,
    ) -> Self {
        let retention_ms = retention.as_millis() as u64;
        // Store backend + retention_ms as a typed entry in the Extensions TypeMap.
        // Both .persist() (on RecordRegistrar) and AimDbQueryExt (on AimDb<R>)
        // retrieve it via extensions().get::<PersistenceState>().
        self.extensions_mut().insert(PersistenceState {
            backend: backend.clone(),
            retention_ms,
        });

        // Register a startup task for periodic retention cleanup.
        // on_start() is a general-purpose lifecycle hook — any external crate can
        // use it for health checks, metrics reporting, warm-up, etc.
        let backend_task = backend.clone();
        self.on_start(move |runtime| async move {
            loop {
                let cutoff = now_ms().saturating_sub(retention.as_millis() as u64);
                let _ = backend_task.cleanup(cutoff).await;
                runtime.sleep(Duration::from_secs(24 * 3600)).await;
            }
        });
        self
    }
}
```

**Notes:**
- `on_start()` and `extensions_mut()` are the only two hooks `aimdb-core` exposes.
  Both are general-purpose — any external crate can use them, not just persistence.
- `on_start()` signature: `fn on_start<F, Fut>(&mut self, f: F) -> &mut Self`
  where `F: FnOnce(Arc<R>) -> Fut + Send + 'static, Fut: Future<Output = ()> + Send + 'static`.
  Tasks execute in registration order after `build()` completes.
- **`on_start` trait bound:** `on_start` itself is defined on `AimDbBuilder<R>`
  and bounds only `R: Spawn` — the minimum needed to spawn a task. The cleanup
  loop's `runtime.sleep()` call requires `R: TimeOps`, which `AimDbBuilderPersistExt`
  already declares at its own trait level. This layering is correct: the core
  hook stays narrow and callers add the extra bounds they need.
- No `block_on`: schema initialization moved into `SqliteBackend::new()`, so
  `with_persistence()` is safe to call from inside any async context.
- `runtime.sleep()` comes from the `TimeOps` trait — no Tokio import needed.
- The initial cleanup loop iteration on startup prunes rows that exceeded
  retention during any period when the process was not running.
- The cleanup interval (24 h) is an internal detail. At ≤1 value/hour/record it
  is more than sufficient.
- **Shutdown ordering:** Persistence subscriber tasks (spawned via `tap_raw`)
  stop naturally when the buffer closes — the `recv()` loop breaks on `Err` and
  the task exits cleanly. The retention cleanup task (spawned via `on_start`)
  runs an infinite loop with 24 h sleeps; it has no explicit cancellation signal
  and relies on the runtime cancelling it when `AimDb` drops. For Tokio this is
  the standard behaviour and is acceptable for the MVP. If a future runtime
  adapter requires a graceful shutdown signal, `on_start` can be extended to
  provide a `CancellationToken`-style handle — that is a post-MVP concern.

---

## AimX Protocol Extension

For remote clients, AimX exposes \`record.query\`:

**Request:**
```json
{
  "method": "record.query",
  "params": {
    "name": "accuracy::*",
    "limit": 1
  }
}
```

**Response:**
```json
{
  "result": {
    "values": [
      { "record": "accuracy::vienna", "value": { "absolute_error": 0.3, ... } },
      { "record": "accuracy::berlin", "value": { "absolute_error": 0.8, ... } }
    ],
    "count": 2
  }
}
```

**Query Parameters:**

| Parameter | Description |
|-----------|-------------|
| \`name\` | Record pattern (supports \`*\` wildcard) |
| \`limit\` | Max results per matching record (default: 1) |
| \`start\`/\`end\` | Optional time range filter (Unix ms) |

---

## Usage in Weather Demo

### Hub Configuration

```rust
// weather-hub-streaming/src/main.rs

// SqliteBackend::new() opens the file and initializes the schema synchronously.
// with_persistence() stores the backend and registers the 7-day retention cleanup.
let backend = Arc::new(SqliteBackend::new("./data/validations.db")?);

let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_persistence(backend.clone(), Duration::from_secs(7 * 24 * 3600))
    .with_remote_access(aimx_config)
    .with_connector(MqttConnector::new(&mqtt_url));

// Accuracy records: transform + persistence
for (temp_key, accuracy_key, city_name) in cities {
    let trackers = city_forecast_trackers[city_name].clone();
    let tx = ws_tx.clone();

    builder.configure::<ForecastValidation>(accuracy_key, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 500 })
            .tap(move |ctx, consumer| ws_tap(ctx, consumer, tx.clone(), accuracy_key.as_str()))
            .persist(accuracy_key.to_string())
            .transform::<Temperature, _>(temp_key, move |t| {
                t.with_state(ValidationState::new(trackers, city_name, tolerance))
                    .on_value(validate_one)
            });
    });
}

let db = builder.build().await?;
```

### UI Integration

```typescript
// hooks/useWebSocket.ts

// On connect, fetch historical validations (latest 1 per city)
useEffect(() => {
  if (connected) {
    ws.send(JSON.stringify({
      method: 'record.query',
      params: {
        name: 'accuracy::*',
        limit: 1
      }
    }));
  }
}, [connected]);

// Handle response
const handleMessage = (msg) => {
  if (msg.result?.values) {
    // Seed AccuracyPanel with historical data
    // record name "accuracy::vienna" → extract city
    msg.result.values.forEach(v => {
      const city = v.record.split('::')[1];
      validationsMap.set(city, v.value);
    });
  }
  if (msg.type === 'forecast_validation') {
    // Real-time update
    validationsMap.set(msg.node_id, msg);
  }
};
```

**Result:** AccuracyPanel shows data immediately on page load, even after
restarts or for new visitors.

---

## Storage Estimates

| Metric | Value |
|--------|-------|
| Size per validation | ~200 bytes |
| Validations per day (5 cities) | ~120 |
| Daily storage | ~24 KB |
| 7-day retention | ~170 KB |

Negligible for SQLite.

---

## Implementation Plan

| Phase | Tasks | Time |
|-------|-------|------|
| 1 | Create \`aimdb-persistence\` crate with trait + subscriber | 3 days |
| 2 | Create \`aimdb-persistence-sqlite\` with SQLite backend | 2 days |
| 3 | Add `Extensions` TypeMap + `on_start()` to builder; `AimDbQueryExt` extension trait | 2 days |
| 4 | Add \`record.query\` to AimX handler | 1 day |
| 5 | Integrate into weather-hub-streaming | 1 day |

**Phase 3 implementation note — `RecordRegistrar` must carry `Extensions` access.**
The `.persist()` impl calls `self.extensions().get::<PersistenceState>()` on
`RecordRegistrar<'a, T, R>`. `RecordRegistrar` is the closure parameter inside
`builder.configure(|reg| { ... })`, so it must hold a `&'a Extensions` reference
threaded through from the builder during `configure()`. This is the one
core-internal plumbing detail that is not yet reflected in any existing
`RecordRegistrar` field — it must be added as part of Phase 3, before Phase 4
work begins. Suggested approach: add a `extensions: &'a Extensions` field to
`RecordRegistrar` and populate it in `AimDbBuilder::configure()`.

---

## Comparison: \`record.drain\` vs \`db.query_*()\`

| Aspect | doc-019 \`record.drain\` | doc-022 \`db.query_*()\` |
|--------|------------------------|------------------------|
| **Data source** | In-memory ring buffer | SQLite |
| **Survives restart** | ❌ | ✅ |
| **"Latest per city"** | ❌ Manual | ✅ Built-in |
| **Type-safe** | ❌ JSON | ✅ Generic \`<T>\` |
| **Use case** | Session batch reads | Historical queries |

Both coexist — use \`drain\` for in-session batch analysis, \`query_*\` for
historical lookups.

---

## Future Enhancements

- **PostgreSQL backend** — For production multi-instance deployments
- **Compression** — For long retention periods
- **MCP tool** — \`mcp_aimdb_query_history\` for AI-assisted debugging

---

## References

- [019-M8-record-history-api](019-M8-record-history-api.md) — In-memory drain API
- [008-M3-remote-access](008-M3-remote-access.md) — AimX protocol (for `record.query` extension only)
