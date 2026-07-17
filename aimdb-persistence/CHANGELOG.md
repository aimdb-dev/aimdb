# Changelog — aimdb-persistence

All notable changes to this crate are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking) — Design 045

- **`with_persistence`'s registered `QueryHandlerFn` returns the shared
  `record.query` shape** `{"records": [{topic, payload, ts}, …], "total": N}`
  (rows sorted by `ts` ascending) instead of `{values: [{record, value,
  stored_at}], count}`, so every transport shares one query vocabulary.

### Changed (breaking)

- **Issue #131:** `RecordRegistrarPersistExt`/`AimDbBuilderPersistExt`/`AimDbQueryExt` are non-generic over the runtime (they extend `RecordRegistrar<'a, T>` / `AimDbBuilder` / `AimDb`); the retention-cleanup `on_start` task receives `RuntimeContext` and sleeps via `ctx.time().sleep_secs(...)`.
- **`RecordRegistrarPersistExt::persist` takes a fresh borrow (Issue #130, design 034 Phase 2).** `fn persist(&mut self, …) -> &mut RecordRegistrar<'a, T, R>` (was `&'a mut self -> &'a mut …`), following core's registrar lifetime fix — `persist()` no longer borrows the registrar for its entire remaining lifetime. Call sites compile unchanged.
- All `R: Spawn` bounds replaced with `R: RuntimeAdapter` on public traits (`AimDbBuilderPersistExt`, `RecordRegistrarPersistExt`, `AimDbQueryExt`) and internal helpers (Issue #88). No behavioural change — `aimdb-persistence` never called `runtime.spawn` directly, the bound was just propagated.

## [0.1.1] - 2026-05-22

### Changed

- Bumped `aimdb-core` dependency to `1.1.0` and `aimdb-executor` to `0.2.0`

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/aimdb-persistence-v0.1.1...HEAD
[0.1.1]: https://github.com/aimdb-dev/aimdb/compare/aimdb-persistence-v0.1.0...aimdb-persistence-v0.1.1
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/aimdb-persistence-v0.1.0
