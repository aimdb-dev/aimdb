# Changelog

All notable changes to `aimdb-websocket-connector` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking, wire)

- **`snap` frames now carry `seq`, and the burst's last one carries `last`.**
  Late-join snapshots are numbered in the subscription's sequence space
  (`1..=N`), and the first `event` continues at `N + 1` rather than restarting
  at `1` — so a snapshot dropped by a slow client is visible as a gap instead of
  vanishing. The final `snap` of a burst adds `"last":true`, which the client
  engine reserves a sink slot for; it surfaces as `SubUpdate::snapshot_end` and
  closes out the initial state without needing a live event (see `aimdb-core`).
  Clients reading the golden frame shape must expect `"seq"` on every `snap`,
  `"last"` on the final one, and an event sequence offset by the snapshot count.

### Fixed

- **Fan-out broadcast drops are now observable as `seq` gaps.** When a
  subscription's bounded channel is full, `ClientManager::broadcast` still drops
  the update (slow-client protection) but now records it and folds the count
  into the next delivered update's `skipped`, so a broadcast-stage drop surfaces
  as a `seq` gap downstream — the same loss signal buffer lag and the connection
  funnel already emit. Previously this drop happened upstream of where the pump
  assigns `seq`, so a slow fan-out consumer silently under-reported its loss.

### Security

- **`record.list` and `record.query` now consult the `AuthHandler`.** Both
  consulted nothing: `record.list` returned core's whole database and
  `record.query` fell through to the `QueryHandlerFn` that
  `aimdb-persistence::with_persistence` registers, with `name` defaulting to
  `"*"` — so a client authenticated with empty subscribe/write grants could
  enumerate and historically read records it could not subscribe to. Two new
  `AuthHandler` methods gate them, `authorize_query(client, pattern)` and
  `authorize_list(client, record_key)`, both defaulting to
  `authorize_subscribe`; an existing handler that overrides `authorize_subscribe`
  (async ACL included) therefore governs all three read paths unchanged. A denied
  query answers `denied` whether or not a handler is configured; denied
  `record.list` rows are dropped from an otherwise successful reply. `NoAuth`
  allows everything, as before.

  Two caveats: grants live in ws-topic space while `record.list` rows are keyed
  by `record_key`, so a record whose topic comes from a `TopicProvider` needs a
  grant covering its *key*; and a `record.query` omitting `name` asks for `"#"`,
  which a narrower grant does not contain — it fails closed.

- **`record.query` results stay inside the pattern that was authorized.** A
  `sensors.*` query returned `sensors.secret.deep` too: the persistence backend
  rewrote `*` to SQL `%`, which crosses `.`, so rows outside the grant reached
  the client even though `can_subscribe("sensors.secret.deep")` is false. Fixed
  in `aimdb-persistence-sqlite` by matching with `topic_matches`; authorization
  needs no per-row hook, since `pattern_contains` already guarantees every topic
  matching an authorized pattern is covered by the grant. An omitted `name` now
  defaults to `"#"` rather than `"*"` — under MQTT semantics `*` is a single
  segment, so the old default silently excluded every dotted record key.

- **A grant with a non-terminal `#` no longer covers its whole subtree.** Both
  `Permissions::can_subscribe` (via `pattern_contains`) and
  `Permissions::can_write` (via `topic_matches`) stopped matching at the first
  `#`, so every segment after it was ignored: a grant of `tenant.#.secret`
  admitted `tenant.public`. `#` now absorbs zero or more segments with the
  suffix still applying, so that grant covers `tenant.secret` and
  `tenant.a.b.secret` only. Grants using a trailing `#` (`sensors.#`, `#`) or
  `*` are unaffected — the change only tightens what an interior `#` admits.
- **Subscribe ACL now checks pattern *containment*, not topic matching.** A
  granted subscribe pattern is honored only if it covers the *whole* pattern the
  client requests. Previously the check matched the requested pattern as if it
  were a concrete topic, so a one-level grant (`sensors.*`) admitted an
  all-levels request (`sensors.#`) — the grant's `*` swallowed the request's
  `#` — silently widening the grant. Concrete (wildcard-free) subscribes are
  unaffected. (Latent before Design 047's `/`→`.` separator fix, which is what
  made dot-keyed grant patterns match at all.)

### Changed (breaking) — Design 047: the WS wire is now AimX

- **The wire protocol is AimX** (`aimdb-core::session::aimx`), one tagged JSON
  frame per WS text message — the same envelope as UDS/serial/TCP. The
  `aimdb-ws-protocol` crate, the 507-line `WsCodec` (and its per-connection
  id↔topic maps), the multi-topic `Subscribe` split, and the `Data`-frame
  pre-serialization in `ClientManager` are deleted. Subscribing to N patterns
  is N `sub` frames; events carry `sub`/`seq`/`topic`; snapshots carry the
  routing `sub`; errors collapse to the 3-code AimX vocabulary
  (`not_found`/`denied`/`internal` — auth stays out-of-band at the HTTP 401).
- **`record.query` / `record.list` replace `Query`/`ListTopics`.**
  `QueryHandler` returns the shared `aimdb_core::remote::QueryRecord` rows;
  without a plugged-in handler the dispatch now falls back to the
  `QueryHandlerFn` registered by `aimdb-persistence::with_persistence`
  (`NoQuery` is gone). `record.list` replies with core's shared
  `aimdb_core::remote::RecordMetadata` rows — the same shape every transport
  serves, keyed by `record_key` — with the data-contract `schema_type` the
  connector resolves stamped in. The connector-only `TopicInfo` row type and
  its topic-scoped `{name, schema_type, entity}` shape are gone; `record.list`
  now enumerates every record the client is granted, not only WS-outbound topics
  (see the `AuthHandler` gating under **Security**).
- **`with_raw_payload` removed** — its purpose was bypassing the ws `Data`
  envelope; under AimX the envelope is the protocol.
- **`SnapshotProvider::snapshot(topic)` became `snapshots(pattern)`**,
  returning every cached `(topic, value)` under the pattern, so wildcard
  subscriptions late-join every covered record (previously wildcard patterns
  never hit the exact-key cache).
- **Auto-subscribe ids are server-chosen** (counting down from `u64::MAX`);
  engine-demuxed clients should subscribe explicitly (design 047 §3.6).
- **Protocol-version gate at the WS upgrade.** The client declares its AimX
  version as `?v=<PROTOCOL_VERSION>` on the upgrade URL (browsers cannot set
  handshake headers, and the server runs `reads_hello:false`); an
  incompatible/absent version is refused with **HTTP 426 Upgrade Required**
  before the socket opens, so a stale client fails at the handshake instead of
  on its first frame. The bundled `WsClientConnector` appends it automatically.

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
