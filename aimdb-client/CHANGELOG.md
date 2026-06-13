# Changelog - aimdb-client

All notable changes to the `aimdb-client` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Transport-agnostic endpoint resolver — pick the transport at runtime via a `scheme://` URL (Issue #123, follow-up to #39 / #122).** New `endpoint` module: `parse_endpoint` (pure, feature-independent grammar) and `dial(url) -> Box<dyn Dialer>` map an endpoint string to a transport `Dialer`, the way records already pick one for links. Schemes: `unix://PATH` / `uds://PATH`, a bare path (the `unix://` shorthand), and `serial://DEVICE?baud=N`. An unknown scheme — or one whose transport isn't compiled in — is rejected with a clear error. New `AimxConnection::connect_over(dialer)` / `connect_over_with_timeout` dial over an explicit `Dialer`, bypassing resolution. (Rides a new `impl Dialer for Box<dyn Dialer>` in `aimdb-core`.)

### Fixed

- **The `hello` handshake now reports protocol version `"2.0"` (was a stale `"1.0"`).** The client has spoken the AimX-v2 wire since the engine rewrite, but its local `PROTOCOL_VERSION` constant was never bumped; it is now a re-export of `aimdb_core::remote::PROTOCOL_VERSION`, so client and server can no longer drift. (The server does not validate the hello version, so this is cosmetic on the wire.) README updated to match the v2 reality (`AimxConnection`, endpoint URLs, v2 framing).

### Changed (breaking)

- **`AimxConnection::connect` now takes a `&str` endpoint, and transports are feature-gated (Issue #123).** `connect`/`connect_with_timeout` accept an endpoint string (was `impl AsRef<Path>`) — a `scheme://` URL or a bare path — resolved through the new `endpoint` module. The transports are now opt-in Cargo features: `transport-uds` (default; makes `aimdb-uds-connector` optional and gates the `discovery` module — a Unix-socket scan) and `transport-serial` (off by default; pulls `aimdb-serial-connector`, i.e. `tokio-serial` → libudev). `ClientError::ConnectionFailed`'s `socket` field is renamed `endpoint`, and a new `ClientError::UnsupportedEndpoint` covers malformed / not-built-in endpoints. The discovery `InstanceInfo.socket_path` field is likewise renamed `endpoint` — it now also carries a caller-supplied endpoint, not just a discovered socket path.
- **`AimxClient` → `AimxConnection`, rebuilt on the shared session engine (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** The synchronous `connection::AimxClient` is retired; the new `engine::AimxConnection` drives `aimdb-core`'s `run_client` engine over `aimdb-uds-connector`'s `UdsDialer` and speaks the reshaped **AimX-v2** protocol. Both the type and the module are re-exported from the crate root (`aimdb_client::AimxConnection`); the `connection` module is replaced by `engine`. `connect()` performs the `hello` handshake, and the full tool surface (list/get/set/subscribe/drain/graph/query) is available.
  - `subscribe(record_name)` now returns a `Stream` of updates directly — the engine routes events back by request id, so there is **no** server-allocated subscription id to track (the old `(subscription_id, queue_size)` handshake is gone).
  - New `connect_with_timeout(path, timeout)` bounds the whole dial + handshake (used by discovery probing).
  - New dependencies: `aimdb-uds-connector` (the UDS transport), `aimdb-tokio-adapter` (the `TimeOps` clock handed to `run_client`), and `futures`. `aimdb-core` is now pulled in with the `connector-session` feature.

## [0.6.0] - 2026-05-22

### Added

- **`AimxClient::reset_stage_profiling()`** (Issue #58): New method issuing the `profiling.reset` AimX request to clear stage profiling counters for every record on the server. Requires the server to be built with the `profiling` feature and the connection to have write permission.

## [0.5.0] - 2026-02-21

### Added

- **Record Drain API**: New methods for batch history access
  - `drain_record(name)`: Drain all pending values since last drain call
  - `drain_record_with_limit(name, limit)`: Drain with maximum count limit
  - `DrainResponse` struct with `record_name`, `values` (JSON array), and `count`
  - Cold-start semantics: first drain creates reader and returns empty
- **Graph Introspection API**: New methods for dependency graph exploration
  - `graph_nodes()`: Get all nodes with origin, buffer type, tap count, outbound status
  - `graph_edges()`: Get all directed edges showing data flow
  - `graph_topo_order()`: Get record keys in topological (spawn) order

### Changed

- **Re-export**: `DrainResponse` now re-exported from crate root for convenience

## [0.4.0] - 2025-12-25

### Changed

- **Dependency Update**: Updated `aimdb-core` dependency to 0.4.0 for RecordKey trait support

## [0.3.0] - 2025-12-15

### Changed

- **RecordMetadata Updates**: Client now handles new `record_id` and `record_key` fields in `RecordMetadata` from aimdb-core
- Protocol remains backward-compatible with AimX v1 - new fields are additional data

## [0.2.0] - 2025-11-20

### Changed

- **Breaking: RecordMetadata Field Rename (via aimdb-core)**: Re-exported `RecordMetadata` type now has `connector_count` field renamed to `outbound_connector_count`. This change originates from `aimdb-core` and affects code accessing this field through `aimdb-client`.

## [0.1.0] - 2025-11-06

### Added

- Initial release of AimDB client library
- Reusable connection and discovery logic for remote AimDB instances
- Unix domain socket communication
- AimX v1 protocol implementation
- Clean error handling with typed errors
- Instance discovery via socket scanning
- Record querying and value retrieval
- Support for subscription management

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
