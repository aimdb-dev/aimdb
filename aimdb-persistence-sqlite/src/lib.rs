//! # aimdb-persistence-sqlite
//!
//! SQLite persistence backend for AimDB.
//!
//! Owns a dedicated OS thread that holds the `rusqlite::Connection`. All async
//! callers send [`DbCommand`] messages via `std::sync::mpsc::sync_channel` and
//! await a `tokio::sync::oneshot` reply. The async executor is never blocked;
//! the writer thread is never awaited.
//!
//! **Runtime requirement:** This crate requires a Tokio runtime for the
//! `oneshot` reply channel. Do **not** use `SqliteBackend` with the Embassy
//! adapter — it will not compile without a Tokio executor.
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_persistence_sqlite::SqliteBackend;
//! use std::sync::Arc;
//!
//! let backend = Arc::new(SqliteBackend::new("./data/history.db")?);
//! ```

use std::path::Path;

use aimdb_persistence::backend::{BoxFuture, PersistenceBackend, QueryParams, StoredValue};
use aimdb_persistence::error::PersistenceError;
use rusqlite::{params, Connection};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Command enum — sent from async callers to the writer thread
// ---------------------------------------------------------------------------

enum DbCommand {
    Store {
        record_name: String,
        json: String,
        timestamp: u64,
        reply: tokio::sync::oneshot::Sender<Result<(), PersistenceError>>,
    },
    Query {
        pattern: String,
        params: QueryParams,
        reply: tokio::sync::oneshot::Sender<Result<Vec<StoredValue>, PersistenceError>>,
    },
    Cleanup {
        older_than: u64,
        reply: tokio::sync::oneshot::Sender<Result<u64, PersistenceError>>,
    },
}

// ---------------------------------------------------------------------------
// SqliteBackend — the public API
// ---------------------------------------------------------------------------

/// SQLite persistence backend.
///
/// `Clone` is cheap — it only clones the `mpsc::SyncSender` handle.
///
/// The writer thread shuts down automatically when all `SyncSender` handles
/// (i.e. all `SqliteBackend` clones) are dropped.
#[derive(Clone)]
pub struct SqliteBackend {
    tx: std::sync::mpsc::SyncSender<DbCommand>,
}

impl SqliteBackend {
    /// Opens (or creates) a SQLite database at `path` and starts the writer thread.
    ///
    /// Schema and WAL mode are configured **synchronously** here — no `block_on`
    /// needed, no async runtime required at construction time.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, PersistenceError> {
        // Bound of 64 provides backpressure without being too aggressive.
        let (tx, rx) = std::sync::mpsc::sync_channel::<DbCommand>(64);

        let conn = Connection::open(path).map_err(|e| PersistenceError::Backend(e.to_string()))?;

        // Enable WAL mode: readers and the single writer proceed concurrently.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| PersistenceError::Backend(e.to_string()))?;

        // Initialize schema before the writer thread is spawned.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS record_history (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                record_name TEXT    NOT NULL,
                value_json  TEXT    NOT NULL,
                stored_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_record_time
                ON record_history(record_name, stored_at DESC);",
        )
        .map_err(|e| PersistenceError::Backend(e.to_string()))?;

        std::thread::Builder::new()
            .name("aimdb-sqlite".to_string())
            .spawn(move || run_db_thread(conn, rx))
            .map_err(|e| PersistenceError::Backend(e.to_string()))?;

        Ok(Self { tx })
    }
}

// ---------------------------------------------------------------------------
// Writer thread — blocking event loop
// ---------------------------------------------------------------------------

fn run_db_thread(conn: Connection, rx: std::sync::mpsc::Receiver<DbCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            DbCommand::Store {
                record_name,
                json,
                timestamp,
                reply,
            } => {
                let ts = match i64::try_from(timestamp) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = reply.send(Err(PersistenceError::Backend(format!(
                            "timestamp {timestamp} overflows i64"
                        ))));
                        continue;
                    }
                };
                let result = conn
                    .prepare_cached(
                        "INSERT INTO record_history (record_name, value_json, stored_at)
                         VALUES (?1, ?2, ?3)",
                    )
                    .and_then(|mut stmt| stmt.execute(params![record_name, json, ts]))
                    .map(|_| ())
                    .map_err(|e| PersistenceError::Backend(e.to_string()));
                let _ = reply.send(result);
            }

            DbCommand::Query {
                pattern,
                params,
                reply,
            } => {
                let result = query_sync(&conn, &pattern, params);
                let _ = reply.send(result);
            }

            DbCommand::Cleanup { older_than, reply } => {
                let cutoff = match i64::try_from(older_than) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = reply.send(Err(PersistenceError::Backend(format!(
                            "cleanup cutoff {older_than} overflows i64"
                        ))));
                        continue;
                    }
                };
                let result = conn
                    .prepare_cached("DELETE FROM record_history WHERE stored_at < ?1")
                    .and_then(|mut stmt| stmt.execute(params![cutoff]))
                    .map(|n| n as u64)
                    .map_err(|e| PersistenceError::Backend(e.to_string()));
                let _ = reply.send(result);
            }
        }
    }
    // All SyncSender handles dropped → exit cleanly.
}

// ---------------------------------------------------------------------------
// SQL helpers
// ---------------------------------------------------------------------------

/// Escape SQL LIKE special characters, then replace `*` with `%`.
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
    // `None` means "no limit" — the SQL uses `(?4 IS NULL OR rn <= ?4)`.
    let limit: Option<i64> = params
        .limit_per_record
        .map(|l| {
            i64::try_from(l).map_err(|_| {
                PersistenceError::Backend("limit_per_record overflows i64".to_string())
            })
        })
        .transpose()?;
    let sql_pattern = sanitize_pattern(pattern);

    // Checked conversion: timestamps must fit in SQLite's signed i64.
    let start_time: Option<i64> = params
        .start_time
        .map(i64::try_from)
        .transpose()
        .map_err(|_| PersistenceError::Backend("start_time overflows i64".to_string()))?;
    let end_time: Option<i64> = params
        .end_time
        .map(i64::try_from)
        .transpose()
        .map_err(|_| PersistenceError::Backend("end_time overflows i64".to_string()))?;

    let mut stmt = conn
        .prepare_cached(
            "WITH ranked AS (
                SELECT record_name, value_json, stored_at,
                       ROW_NUMBER() OVER (
                           PARTITION BY record_name
                           ORDER BY stored_at DESC, id DESC
                       ) AS rn
                FROM record_history
                WHERE record_name LIKE ?1 ESCAPE '\\'
                  AND (?2 IS NULL OR stored_at >= ?2)
                  AND (?3 IS NULL OR stored_at <= ?3)
            )
            SELECT record_name, value_json, stored_at
            FROM ranked WHERE (?4 IS NULL OR rn <= ?4)
            ORDER BY record_name, stored_at DESC",
        )
        .map_err(|e| PersistenceError::Backend(e.to_string()))?;

    let rows = stmt
        .query_map(
            rusqlite::params![sql_pattern, start_time, end_time, limit],
            |row| {
                let value_str: String = row.get(1)?;
                Ok(StoredValue {
                    record_name: row.get(0)?,
                    value: serde_json::from_str(&value_str).unwrap_or_else(|e| {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            "SQLite: corrupted JSON in record_history row, \
                             substituting null: {e}"
                        );
                        #[cfg(not(feature = "tracing"))]
                        let _ = e;
                        Value::Null
                    }),
                    stored_at: row.get::<_, i64>(2).map(|v| v.max(0) as u64)?,
                })
            },
        )
        .map_err(|e| PersistenceError::Backend(e.to_string()))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| PersistenceError::Backend(e.to_string()))
}

// ---------------------------------------------------------------------------
// send_cmd! macro — enqueue + await oneshot
// ---------------------------------------------------------------------------

macro_rules! send_cmd {
    ($tx:expr, $cmd:expr) => {{
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        $tx.send($cmd(reply_tx))
            .map_err(|_| PersistenceError::BackendShutdown)?;
        reply_rx
            .await
            .map_err(|_| PersistenceError::BackendShutdown)?
    }};
}

// ---------------------------------------------------------------------------
// PersistenceBackend impl
// ---------------------------------------------------------------------------

impl PersistenceBackend for SqliteBackend {
    // initialize() — uses trait default (no-op); schema was created in ::new().

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
        let tx = self.tx.clone();
        Box::pin(async move {
            send_cmd!(tx, |reply| DbCommand::Store {
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
        let tx = self.tx.clone();
        Box::pin(async move {
            send_cmd!(tx, |reply| DbCommand::Query {
                pattern,
                params,
                reply,
            })
        })
    }

    fn cleanup(&self, older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let tx = self.tx.clone();
        Box::pin(async move { send_cmd!(tx, |reply| DbCommand::Cleanup { older_than, reply }) })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_store_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let backend = SqliteBackend::new(&db_path).unwrap();

        // Store a value
        let value = serde_json::json!({"celsius": 21.5, "city": "vienna"});
        backend.store("temp::vienna", &value, 1000).await.unwrap();
        backend.store("temp::vienna", &value, 2000).await.unwrap();
        backend.store("temp::berlin", &value, 1500).await.unwrap();

        // Query latest 1 per record with wildcard
        let results = backend
            .query(
                "temp::*",
                QueryParams {
                    limit_per_record: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 2); // 1 per city
        assert!(results.iter().any(|r| r.record_name == "temp::vienna"));
        assert!(results.iter().any(|r| r.record_name == "temp::berlin"));

        // The vienna result should be the latest (timestamp 2000)
        let vienna = results
            .iter()
            .find(|r| r.record_name == "temp::vienna")
            .unwrap();
        assert_eq!(vienna.stored_at, 2000);
    }

    #[tokio::test]
    async fn test_time_range_query() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_range.db");
        let backend = SqliteBackend::new(&db_path).unwrap();

        let value = serde_json::json!({"celsius": 20.0});
        for ts in [1000u64, 2000, 3000, 4000, 5000] {
            backend.store("temp::vienna", &value, ts).await.unwrap();
        }

        let results = backend
            .query(
                "temp::vienna",
                QueryParams {
                    start_time: Some(2000),
                    end_time: Some(4000),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 3); // timestamps 2000, 3000, 4000
    }

    #[tokio::test]
    async fn test_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_cleanup.db");
        let backend = SqliteBackend::new(&db_path).unwrap();

        let value = serde_json::json!({"celsius": 20.0});
        backend.store("temp::a", &value, 1000).await.unwrap();
        backend.store("temp::b", &value, 2000).await.unwrap();
        backend.store("temp::c", &value, 3000).await.unwrap();

        // Delete rows older than 2500
        let deleted = backend.cleanup(2500).await.unwrap();
        assert_eq!(deleted, 2); // 1000 and 2000

        // Only the 3000 row remains
        let results = backend
            .query(
                "temp::*",
                QueryParams {
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].stored_at, 3000);
    }

    #[tokio::test]
    async fn test_pattern_escaping() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_escape.db");
        let backend = SqliteBackend::new(&db_path).unwrap();

        let value = serde_json::json!({"ok": true});
        backend.store("test_record", &value, 1000).await.unwrap();
        backend.store("testXrecord", &value, 1000).await.unwrap();

        // Exact match — the `_` in the record name should NOT match `X`
        let results = backend
            .query(
                "test_record",
                QueryParams {
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record_name, "test_record");
    }
}
