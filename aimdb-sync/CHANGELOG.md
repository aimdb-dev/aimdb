# Changelog - aimdb-sync

All notable changes to the `aimdb-sync` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **`AimDbSyncExt::attach` no longer spins, and can no longer hang.** The
  constructor polled a `Mutex<Option<Handle>>` on a 1 ms sleep while the runtime
  thread filled it — and that thread returns early if `Runtime::new()` fails, so
  the wait never ended. It now takes the same channel-and-`blocking_recv` shape
  `AimDbBuilderSyncExt::attach` has always used: a runtime thread that dies drops
  its sender, which closes the channel, which ends the wait with
  `SyncError::AttachFailed`.
- **A startup failure now carries its cause.** Both constructors' channels carry
  a `Result`, so a failed `Runtime::new()` or a failed `build()` reports *why*
  instead of only that it happened — previously the reason reached the log sink
  and nothing else, which for an FFI consumer meant a status code and an empty
  explanation.
- **`detach_timeout` parks instead of polling.** The wait used a 10 ms sleep
  loop, so every shutdown paid up to 10 ms it did not need and the timeout was
  rounded up to the next tick. It now blocks on `recv_timeout`.
- **Both `Mutex::lock().unwrap()` sites are gone**, with the mutex they guarded.
  A poisoned lock could panic out of the blocking surface, which across an FFI
  boundary is undefined behaviour rather than an error.

### Added

- **`fork()` safety.** A child of `fork` inherits every handle, producer and
  consumer the parent held, and none of the runtime thread that makes them work
  — so its `set()` used to return `Ok` into a buffer nobody drains. Handles,
  producers and consumers now record a fork generation and refuse with the new
  `SyncError::ForkedChild` once the process has forked since they were made.
  `detach` and `Drop` release the runtime thread's `JoinHandle` rather than
  joining a thread this process does not have, which panicked inside `std`.
  Detection is a lazily registered `pthread_atfork` handler, so the check on the
  publish path is one relaxed atomic load, and a program that never attaches
  never installs a handler. `fork::generation` and `fork::forked_since` are
  public because a layer built on this crate needs the same answer without
  taking a lock the runtime thread may hold; `generation` arms the handler
  itself, so a caller that stamps its own state before any database exists —
  an FFI door opens before it is used — is not handed a number that can never
  change. A database the child attaches *itself* after forking is unaffected —
  the guard is a generation counter, not a poison flag.
- **A panic-freedom contract on the blocking surface.** The crate is compiled
  under `deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)` outside
  its own tests, so "a panic here is a bug, not an error channel" is checked
  rather than remembered. There were no violations left to fix — the last two
  went with the mutex removed in #226 — so this is a ratchet, not a cleanup.
  It matters most across an FFI boundary, where unwinding is undefined
  behaviour and a consumer's `panic = "abort"` turns any panic into the whole
  process dying. Documented with its two limits: `block_on` still panics if
  called from inside a Tokio runtime, and the guarantee stops at this crate's
  edge.
- **`SyncError::kind()`.** Returns `aimdb_core::DbErrorKind` rather than a kind
  of its own, so a caller — an FFI layer above all — has one set of actions for
  the whole stack instead of one per crate. The `Db` arm delegates, so a buffer
  that is merely empty classifies identically whether it is reached through this
  facade or through `aimdb-core` directly.
- **`SyncProducer<T: Settable>::set_value`/`try_set_value`/`set_value_at`** (design 041 §3.4, feature `data-contracts`). `set()` took a fully constructed `T`, so every outside-the-thread caller hand-assembled the struct; `set_value(value)` constructs via `T::set(value, timestamp)` and sends in one call — blocking (`set_value`), non-blocking (`try_set_value`), or with an explicit timestamp for replay/testing (`set_value_at`). `set_value`/`try_set_value` stamp the caller's `SystemTime` (crate is std-only). New optional dependency: `aimdb-data-contracts` (feature `settable`), behind the new `data-contracts` feature — the contracts crate gains no `sync` feature (dependency direction unchanged).

### Changed (breaking)

- **`SyncError` is `#[non_exhaustive]`.** Downstream exhaustive matches now need
  a wildcard arm; match on `kind()` instead where you only need to know what to
  do. This is what makes every future `SyncError` variant additive rather than
  breaking.
- **Issue #131:** `AimDbSyncExt` extends the non-generic `aimdb_core::AimDb`; internal handles drop the `TokioAdapter` type parameter.
- **Issue #200:** the internal channel bridge to the `tokio` thread is gone — blocking calls now call the runtime directly using the `block_on` seam. API implications:
  - `SyncProducer::set_with_timeout` removed
  - Capacity-related API removed: `AimDbBuilderSyncExt::producer_with_capacity`/`consumer_with_capacity` and the `DEFAULT_SYNC_CHANNEL_CAPACITY` constant are gone.
  - `SyncConsumer`: `get`, `try_get`, `get_with_timeout`, `get_latest`, and `get_latest_with_timeout` now take `&mut self` (was `&self`)
  - `SyncConsumer` no longer implements `Clone` or `Sync` (still `Send`).
  - **Migration.** The replacement for a cloned consumer is a second
    `handle.consumer()` — but note that it is not a like-for-like swap.
    Cloning in 0.5.0 shared one stream, so each clone got *a share* of the
    values (split). Calling `handle.consumer()` twice gives two independent
    cursors, so each one sees *every* value (fan-out). Code that mechanically
    replaces `clone()` with `consumer()` therefore changes behaviour with no
    compile error. To keep split semantics, share one consumer as
    `Arc<Mutex<SyncConsumer<T>>>`.

### Changed

- `attach()` updated to destructure the `(AimDb<TokioAdapter>, AimDbRunner)` tuple returned by `AimDbBuilder::build()` after issue #88, and to drive the runner inside the runtime thread via `tokio::select!` against the shutdown signal. No public API change.

## [0.5.0] - 2026-02-21

### Changed

- **Dependency Update**: Updated `aimdb-core` and `aimdb-tokio-adapter` dependencies to 0.5.0

## [0.4.0] - 2025-12-25

### Changed

- **Dependency Update**: Updated `aimdb-core` and `aimdb-tokio-adapter` dependencies to 0.4.0

## [0.3.0] - 2025-12-15

### Changed

- **Breaking: Producer/Consumer API**: All methods now require a record key parameter:
  - `producer::<T>(key)` instead of `producer::<T>()`
  - `consumer::<T>(key)` instead of `consumer::<T>()`
  - `producer_with_capacity::<T>(key, capacity)` instead of `producer_with_capacity::<T>(capacity)`
  - `consumer_with_capacity::<T>(key, capacity)` instead of `consumer_with_capacity::<T>(capacity)`
- **Breaking: Record Registration API**: Updated all test code to use new key-based `configure<T>(key, |reg| ...)` API
- All integration tests now specify explicit record keys (e.g., `"test.data"`) per new RecordId/RecordKey architecture

## [0.2.0] - 2025-11-20

### Changed

- Updated to support async `build()` method in `aimdb-core`
- Compatible with new connector builder pattern

## [0.1.0] - 2025-11-06

### Added

- Initial release of synchronous API wrapper for AimDB
- Blocking wrapper around async AimDB core
- Thread-safe synchronous record access
- Automatic Tokio runtime management
- Ideal for gradual migration from sync to async
- Type-safe synchronous record operations
- Compatible with existing synchronous codebases

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
