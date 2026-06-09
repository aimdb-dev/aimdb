# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Connector-build errors carry their message on `no_std` too (Issue #129).** With `DbError` unified on `alloc::String`, the dual `#[cfg]` error-construction branches in both clients collapse to one `DbError::runtime_error(...)` expression; the Embassy client's "Failed to build KNX connector" detail is no longer dropped on embedded targets. No API change.
- **Tokio client rebuilt on the shared data-plane toolkit (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** The hand-rolled consume-serialize-publish and telegram read-route loops are replaced by `aimdb-core`'s `pump_sink` / `pump_source` helpers: the connector now writes only a `KnxSink` (`Connector`, parses the destination group address and forwards a fire-and-forget `GroupValueWrite`) and a `KnxSource` (`Source`, yields each inbound `(group_address, payload)`) and composes the pumps in `build()`. The routing `Router` is (re)built inside `pump_source`. `std` enables `aimdb-core/connector-session` (where the pump helpers live; `std` implies it transitively). No public API change.
- **Outbound publishers survive a consumer lag (Tokio + Embassy).** A `BufferLagged` (SPMC-ring overflow) on the outbound reader now skips the gap and keeps publishing instead of terminating the publisher; only a closed buffer stops it.
- **M17 — Embassy client rebuilt on core's pumps via the adapter spine ([Design 033](../docs/design/033-M17-unify-connectors-drop-send.md)).** The hand-rolled outbound publisher loops are gone; this crate now contributes only the KNX/IP **protocol** (UDP socket + tunnelling state machine), force-`Send`ed once via `aimdb_embassy_adapter::connectors::into_box_future` — **no `unsafe`, no `SendFutureWrapper`** remain in this crate. Data-flow changes:
  - **Outbound** rides core's `pump_sink` through the existing `Connector` impl (commands onto the `CriticalSectionRawMutex` command channel, which is already `Send` — no force-`Send` bridge needed). Behavior change: when a topic provider returns an **invalid dynamic group address**, the value is now dropped (`PublishError::InvalidDestination`, logged by the pump) instead of falling back to the URL's default address — silently writing to a different group address than the provider asked for was misrouting, and this matches the Tokio half.
  - **Inbound** rides core's `pump_source` via a new static 32-deep telegram channel (`KnxSource` drains it). The protocol loop forwards each parsed telegram with `try_send` — **drop + log when the channel is full** rather than awaiting, so a slow consumer can never stall the loop that answers `TUNNELING_ACK`s and heartbeats (a stalled loop would time out the gateway connection, losing far more than the dropped telegram).

### Changed (breaking)

- **`ConnectorBuilder::build()` now returns `Vec<BoxFuture<'static, ()>>` instead of `Arc<dyn Connector>` (Issue #88).** Both Tokio and Embassy implementations updated.
- `spawn_connection_task()` → `build_connection_future()`; the `mpsc::channel` for outbound commands is created up front, the receiver captured by the connection future, and the sender cloned into each outbound publisher future. `spawn_outbound_publishers()` → `collect_outbound_futures()`.
- `R: Spawn` bounds dropped in favour of `R: RuntimeAdapter`.
- The `transport::Connector` impl on `KnxConnectorImpl` was removed alongside the discarded `Arc<dyn Connector>` return path (Issue #88) — then reinstated as the pure outbound I/O adapter that `pump_sink` drives (Issue #39 / M17): it is no longer a programmatic-publish surface but the route through which every outbound record reaches the command channel.

## [0.4.0] - 2026-05-22

### Changed

- Updated `Router::route()` calls to pass runtime context via `db.runtime_any()` in both Tokio and Embassy clients, enabling context-aware deserializers (Design 026)
- Updated outbound publishers (Tokio and Embassy) to dispatch via `SerializerKind`, enabling context-aware serializers with `db.runtime_any()`

## [0.3.1] - 2026-03-16

### Changed

- Updated Embassy dependency versions: executor 0.10.0, time 0.5.1, sync 0.8.0, futures 0.1.2, net 0.9.0

## [0.3.0] - 2026-02-21

### Added

- **Dynamic Group Address Routing (Design 018)**: Full support for dynamic KNX group address resolution
  - **Outbound**: Uses `TopicProvider` to dynamically determine group addresses based on data values. Configure via `.with_topic_provider()` on outbound connectors.
  - **Inbound**: Uses `TopicResolverFn` for late-binding subscription addresses at connector startup. Configure via `.with_topic_resolver()` on inbound connectors.
  - Addresses resolved at connector startup via `collect_inbound_routes()` and per-telegram via `TopicProviderFn` for outbound

## [0.2.0] - 2025-12-15

### Changed

- **Breaking: Record Registration**: Updated demo examples to use new key-based `configure<T>(key, |reg| ...)` API
- All demo records now have explicit keys (e.g., `"lights.state"`, `"temp.livingroom"`, `"lights.control"`)
- Demos refactored to use shared `knx-connector-demo-common` crate for cross-platform types, keys, and monitors

## [0.1.0] - 2025-11-20

### Added
- Initial implementation of KNX/IP connector
- Dual runtime support (Tokio and Embassy)
- KNXnet/IP Tunneling protocol support
- Inbound monitoring (KNX bus → AimDB records)
- Outbound control (AimDB records → KNX bus)
- Group address parsing (3-level format)
- DPT type support via knx-pico integration
- Automatic reconnection on connection loss (5s interval)
- **ACK timeout handling** with 3-second timeout for outbound telegrams
- **Heartbeat/keepalive** (CONNECTIONSTATE_REQUEST every 55s)
- **Comprehensive unit tests** (group addresses, frames, connection state)
- **Production deployment guide** in README.md
- `tokio-knx-connector-demo` example with bidirectional control
- `embassy-knx-connector-demo` example for embedded systems

### Fixed
- Proper sequence number tracking for ACK validation
- TUNNELING_ACK detection and processing
- Pending ACK cleanup on timeout

### Known Limitations
- No KNX Secure support (plaintext only)
- No group address discovery
- Fire-and-forget publishing (no bus-level confirmation)
- Single connection per gateway instance
- No routing mode support

## [0.1.0] - 2025-11-20

Initial beta release for production evaluation.

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
