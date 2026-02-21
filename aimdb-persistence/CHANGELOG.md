# Changelog — aimdb-persistence

All notable changes to this crate are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-02-21

### Added

- **`PersistenceBackend` trait** — pluggable async storage interface with
  `store`, `query`, `cleanup`, and an optional `initialize` hook
  (`backend.rs`)
- **`StoredValue`** — typed record returned by `query`: `record_name`,
  `value: serde_json::Value`, `stored_at: u64` (Unix ms)
- **`QueryParams`** — `limit_per_record: Option<usize>`, `start_time:
  Option<u64>`, `end_time: Option<u64>`; `None` limit means "return all
  matching rows" (no implicit truncation)
- **`AimDbBuilderPersistExt`** trait — adds `.with_persistence(backend,
  retention)` to `AimDbBuilder<R>`; stores `PersistenceState` in the
  builder's `Extensions` TypeMap and registers a 24-hour retention cleanup
  task via `on_start()`
- **`PersistenceState`** — `backend: Arc<dyn PersistenceBackend>` +
  `retention_ms: u64`; stored in `Extensions` and shared between the
  subscriber and query-time code
- **`RecordRegistrarPersistExt`** trait — adds `.persist(record_name)` to
  `RecordRegistrar<T, R>`; accepts `impl Into<String>` for ergonomic call
  sites; spawns a `tap_raw` subscriber that serialises each `T` to JSON and
  writes it to the backend
- **`AimDbQueryExt`** trait — adds `query_latest`, `query_range`, and
  `query_raw` to `AimDb<R>`:
  - `query_latest<T>(&self, pattern, limit) -> Vec<T>` — typed, limit per
    record
  - `query_range<T>(&self, pattern, start_ms, end_ms, limit_per_record) -> Vec<T>` — typed,
    time range with optional per-record limit (`None` = all rows)
  - `query_raw(&self, pattern, params) -> Vec<StoredValue>` — untyped escape
    hatch used by the AimX `record.query` protocol handler
- **`PersistenceError`** — `NotConfigured`, `Backend(String)`,
  `BackendShutdown`, `Serialization(serde_json::Error)`
- **`tracing` feature** — optional structured logging for subscriber events,
  cleanup results, and query-time deserialisation failures
- **Cleanup observability** — when the `tracing` feature is disabled, cleanup
  errors are printed to `stderr` via `eprintln!` so operators are never
  silently left with unbounded database growth

### Design decisions

- **No coupling to `aimdb-core` persistence types** — persistence state flows
  through the `Extensions` TypeMap; `aimdb-core` carries only a type-erased
  `QueryHandlerFn` for the AimX protocol delegation
- **Runtime-agnostic subscriber** — `RecordRegistrarPersistExt` requires only
  `T: Serialize`; `.with_remote_access()` is not needed
- **`StoredValue.value` moved, not cloned** — `filter_map` in `AimDbQueryExt`
  destructures `StoredValue` to move the `Value` into `from_value()` directly

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/aimdb-persistence-v0.1.0
