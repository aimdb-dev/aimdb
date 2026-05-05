# Changelog - aimdb-executor

All notable changes to the `aimdb-executor` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
