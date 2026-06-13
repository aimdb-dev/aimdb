# Changelog

All notable changes to `aimdb-websocket-connector` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Internal refactors

- **Adjusted to core's design-036-W1 data-plane de-`Any`.** `WsDispatch`/`WsSession` carry a concrete `RuntimeContext` (was `Option` — it was always `Some`) and the inbound `Router::route` call is synchronous; the inbound route tuples and `pump_sink` routes flow through opaquely. No public API or wire change.

- **WebSocket server + client ported onto the shared session engine (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** Behavior-preserving (wire-identical, gated by a round-trip test): the WS server now runs on `aimdb-core`'s `serve`/`run_session` and the client on `run_client`, so the two hand-rolled WS stacks collapse onto the same engines as AimX. New modules: `codec` (`WsCodec`, the per-connection WS-JSON `EnvelopeCodec` — id↔topic bookkeeping, O(1) fan-out by writing the bus-pre-serialized `Data` frame verbatim, zero-copy `decode_outbound` replacing the old `&'static` topic interner), `transport` (`WsServerConnection`/`WsClientConnection`/`WsDialer` over axum / tokio-tungstenite, including the multi-topic `Subscribe`/`Unsubscribe` split), and `dispatch` (`WsDispatch`/`WsSession` homing the `ClientManager` bus + auth + query/snapshot). The hand-rolled `client/connector.rs` loop is removed; `client_manager`/`session` slim down to a fan-out bus + snapshot/query providers. Public `WebSocketConnectorBuilder` / `WsClientConnectorBuilder` surfaces are unchanged (the client builder now bounds `R: TimeOps` for the engine clock). Added `examples/ws_server.rs`, `tests/ws_roundtrip.rs`, and a dev-dep on `aimdb-tokio-adapter`.
- **WS client connector is now spawn-free (Issue #114, Design 030).** All six `tokio::spawn` call sites in the client connector (initial write/read/keepalive/reconnect-watcher plus the watcher's per-reconnect read/write loops) collapsed into one infrastructure future that owns a `FuturesUnordered<BoxFuture>` driven by `tokio::select! { biased; }`. The reconnect watcher no longer spawns; on a successful reconnect it sends a `NewLoops { write_sink, read_stream, write_rx }` over an mpsc to the outer future, which pushes fresh read- and write-loop futures onto the set.
  - `WsClientConnectorImpl::connect()` return type changed from `Result<Self, String>` to `Result<(Self, BoxFuture), String>` — the second element is the infrastructure future; the builder prepends it to the outbound publisher futures before returning to `AimDbBuilder`.
  - Internal-only API change; no impact on the public `WsClientConnectorBuilder` or `ConnectorBuilder` surfaces.

### Changed (breaking)

- **`ConnectorBuilder::build()` now returns `Vec<BoxFuture<'static, ()>>` instead of `Arc<dyn Connector>` (Issue #88).** Server-side: `start_server()` → `build_server_future()` (the `axum::serve()` accept loop is collected, not spawned). Client-side: outbound publishers converted to `collect_outbound_futures()`.
- `R: Spawn` bounds dropped throughout in favour of `R: RuntimeAdapter`. The no-op `transport::Connector` impl on `WebSocketConnectorImpl` was removed.
- ~~WS *client* internal background tasks (write loop, read loop, keepalive, reconnect watcher) are temporarily bridged to `tokio::spawn` directly (per design 028 §"Out of Scope" / Group 4). They will move to nested `FuturesUnordered` in the AimX portability follow-up.~~ Resolved by the spawn-free refactor above.

## [0.2.0] - 2026-05-22

### Changed

- Updated `Router::route()` calls to pass runtime context via `db.runtime_any()` in both client connector and session handler, enabling context-aware deserializers (Design 026)
- Updated outbound publishers (server and client) to dispatch via `SerializerKind`, enabling context-aware serializers with `db.runtime_any()`

## [0.1.0] - 2026-03-16

### Added

- Initial release of the AimDB WebSocket connector
- **Server mode** (Axum-based): accept incoming WebSocket connections via `link_to("ws://topic")`
  - Configurable bind address, path, and late-join support
  - Client session management with automatic cleanup
  - `AuthHandler` trait for pluggable authentication
- **Client mode** (tokio-tungstenite): connect to remote WebSocket servers via `link_to("ws-client://host/topic")` and `link_from("ws-client://host/topic")`
  - AimDB-to-AimDB sync without intermediary broker
  - Automatic reconnection
- Shared wire protocol via `aimdb-ws-protocol`
- `WebSocketConnector` builder API
- `StreamableRegistry` for extensible type-erased dispatch
  - Register `Streamable` types via `.register::<T>()` on the builder
  - Schema-name collision detection at registration time
  - Monomorphized closures for zero-overhead serialization/deserialization
