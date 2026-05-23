# Changelog - aimdb-executor

All notable changes to the `aimdb-executor` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Removed (breaking)

- **`Spawn` trait deleted (Issue #88).** Including the `SpawnToken` associated type and the `ExecutorError::SpawnFailed` variant. The `Runtime` bundle trait is now `RuntimeAdapter + TimeOps + Logger` (no `Spawn` supertrait). Task execution moves to `AimDbRunner::run()` in `aimdb-core`, which drives every collected future via a single `FuturesUnordered`. Custom adapters must drop `impl Spawn`.
- `JoinFanInRuntime` supertrait relaxed from `Spawn` to `RuntimeAdapter`.

### Added

- `futures-util` (alloc-only) as a regular dependency — provides `FuturesUnordered` used by `aimdb-core`'s `AimDbRunner`.

## [0.2.0] - 2026-05-22

### Added

- **`TimeOps::duration_as_nanos`** — new required method on the `TimeOps` trait that returns the number of whole nanoseconds in a `Self::Duration`. Introduced for stage profiling (Issue #58) so features can convert elapsed time to a numeric, runtime-agnostic representation without binding to `std::time`. Implementations should saturate rather than overflow for durations larger than `u64::MAX` nanoseconds.
- **Join fan-in traits (Design 027)**: Runtime-agnostic abstraction for multi-input transform fan-in queues, replacing the previous `tokio::sync::mpsc`-only path in `aimdb-core`.
  - `JoinFanInRuntime` trait — runtime capability for creating bounded fan-in queues; implemented per adapter.
  - `JoinQueue<T>` trait — splittable into `Sender` / `Receiver` halves.
  - `JoinSender<T>` / `JoinReceiver<T>` traits — `async fn send` / `async fn recv` returning `ExecutorResult<()>`.
- **`ExecutorError::QueueClosed`** variant — returned by `JoinSender::send` / `JoinReceiver::recv` when the channel is closed (Tokio, WASM). Embassy channels never close, so this variant is unreachable on Embassy.

## [0.1.0] - 2025-11-06

### Added

- Initial release of runtime executor abstraction traits
- `RuntimeAdapter` trait for platform identification
- `Spawn` trait for async task spawning
- `TimeOps` trait for time operations (now, sleep)
- `Logger` trait for logging abstraction
- `no_std` compatibility for embedded targets
- Cross-platform support for Tokio and Embassy runtimes

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
