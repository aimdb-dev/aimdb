# Changelog — aimdb-persistence-sqlite

All notable changes to this crate are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- **Upgrade `rusqlite` 0.32 → 0.41 (bundled SQLite 3.54.1)** to fix
  RUSTSEC-2026-0459 / CVE-2026-119375: SQLite before 3.54.0 could return
  uninitialized page-cache memory when opening a database alongside a crafted
  `-wal` file with a mismatched page size.

## [0.3.0] - 2026-11-03

### Changed (breaking)

- **Requires `aimdb-core` 3.0 and `aimdb-persistence` 0.3 (#326).**

### Added

- **`log` feature (#365)**, forwarding operational events through the `log`
  facade. It can be enabled together with `tracing`.

### Changed

- **Operational failures are reported through core's `log_*!` macros (#365)**,
  for example a stored row whose JSON no longer parses. `tracing` now enables
  `aimdb-core/tracing` instead of depending on `tracing` directly, and the crate
  depends on `aimdb-core` (`alloc`) for those macros. The log target is
  `aimdb_persistence_sqlite::events`.

## [0.2.0] - 2026-09-18

### Changed (breaking) — Design 047

- **`query` honors MQTT pattern semantics.** The `*` → SQL `%` rewrite is gone:
  `%` crosses `.`, so `sensors.*` also returned `sensors.secret.deep` — outside
  the pattern, and on the WebSocket path outside the grant that authorized it.
  Queries now scan a `record_name` range derived from the pattern's literal
  prefix and filter rows with `topic_matches`. Three consequences: `#` works (it
  reached SQLite as a literal and matched nothing), matching is case-sensitive
  (`LIKE` is ASCII-case-insensitive, so `sensors.*` matched `Sensors.temp`), and
  the scan can use `idx_record_time`, which case-insensitive `LIKE` never could.
  Patterns over keys that are not dot-separated (`"temp::*"`) no longer wildcard.

## [0.1.1] - 2026-05-22

### Changed

- Bumped `aimdb-persistence` dependency to `0.1.1`

## [0.1.0] — 2026-02-21

### Added

- **`SqliteBackend`** — `Clone`-able handle to a dedicated SQLite writer thread
  - Construction via `SqliteBackend::new(path)`: opens or creates the database,
    enables WAL journal mode, applies schema, then spawns the `"aimdb-sqlite"`
    OS thread — all synchronously, so no Tokio runtime is needed at build time
  - `Clone` is `O(1)`: clones only the `SyncSender<DbCommand>` handle; the
    writer thread shuts down when all clones are dropped
- **Schema** — single `record_history` table with `(id, record_name,
  value_json, stored_at)`; composite index on `(record_name, stored_at DESC)`
  for efficient range and top-N queries
- **WAL mode** — `PRAGMA journal_mode = WAL` allows concurrent readers while
  a single writer proceeds
- **`DbCommand` actor model** — `Store`, `Query`, `Cleanup` commands sent via
  `mpsc::sync_channel(64)`; each carries a `tokio::sync::oneshot` reply sender
  so the async caller `await`s without blocking the executor
- **`store(record_name, value, timestamp)`** — serialises `serde_json::Value`
  to text and INSERTs into `record_history`
- **`query(pattern, params)`** — `WITH ranked AS (… ROW_NUMBER() OVER
  (PARTITION BY record_name …))` window-function query supporting:
  - `*` wildcard patterns (mapped to SQL `LIKE … ESCAPE '\\'`)
  - Optional time-range bounds (`start_time`, `end_time`)
  - Optional per-record limit (`limit_per_record: None` → no truncation)
- **`cleanup(older_than)`** — `DELETE … WHERE stored_at < ?` with row-count
  return; called by the retention task registered via `with_persistence()`
- **`sanitize_pattern`** helper — escapes SQL `LIKE` metacharacters (`%`, `_`,
  `\`) before mapping the AimDB `*` wildcard to `%`
- **`tracing` feature** — optional `tracing::warn!` for corrupted JSON rows
  encountered during query

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v3.0.0...HEAD
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v2.0.0...v3.0.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v1.1.0...v2.0.0
[0.1.1]: https://github.com/aimdb-dev/aimdb/compare/aimdb-persistence-sqlite-v0.1.0...aimdb-persistence-sqlite-v0.1.1
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/aimdb-persistence-sqlite-v0.1.0
