# Changelog

All notable changes to `aimdb-websocket-connector` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking, API)

- **Read grants are record-key patterns, not topic patterns.**
  `Permissions::subscribe_patterns` is renamed to `read_patterns` and
  `Permissions::can_subscribe(topic)` is renamed `can_read(key)`. The rename
  carries a sublte change: the read patterns are now matched against **record
  keys** — the string passed to `configure::<T>(key, ...)` — rather than against
  the ws topic registered at `link_to("ws://...")`. `write_patterns` and `can_write` are
  unchanged and remain **topic**-based. An operator migrating a config
  replaces each topic grant with the key(s) of the records publishing there;
  where the two namespaces coincide (`link_to("ws://cfg")` on record `cfg`) no
  change is needed.

- **`AuthHandler::authorize_subscribe`, `authorize_query` and `authorize_list`
  are removed.** Authorization is resolved once, at the HTTP upgrade, into a
  per-client record bitset; the three read paths consult that bitset instead of
  calling back into the handler. A handler that overrode any of them must move
  the logic into `authenticate`, which now returns the grants that become the
  bitset. `authorize_write` is unaffected.

  **Read grants are now fixed for the lifetime of a connection.** Previously a
  handler overriding these hooks could consult external or mutable state (an ACL
  service, a revocation list) on every `sub`, `record.query` and `record.list`,
  and so refuse *new* read requests on an open connection once a grant changed.
  That per-request re-check is gone: nothing on the read path calls back into
  the handler after `authenticate`. With the default handler nothing changes —
  grants were already fixed at the upgrade. Live subscriptions were never
  re-checked per event, before or after this change, so revoking a client's
  reads still means closing its connection and letting it re-authenticate.

- **`AuthHandler::authorize_query_record` added**, a `record.query` post-filter
  receiving the handler's rows and returning the subset the client may read.
  It defaults to `Permissions::can_read` on each row's key, so implementors need
  it only to narrow further. This is deliberately pattern-based rather than
  bitset-based: history outlives configuration, so a query may legitimately name
  a record the running server no longer registers.

- **`ClientInfo` carries `record_perms: Arc<RecordsBits>`**, the resolved bitset,
  alongside the `Permissions` it was built from. `RecordsBits` is public
  (`new`, `set`, `is_allowed`, `len`, `is_empty`,
  `resolve_permissions`); one bit per registered record, indexed by record id —
  registration order in the builder, which is also the index
  `AimDb::list_records` reports.

- **`ClientManager::subscribe` and `broadcast` signatures changed.**
  `subscribe(pattern)` becomes `subscribe(pattern, Arc<RecordsBits>)` — a
  subscription now carries the grants it delivers under — and
  `broadcast(topic, payload)` becomes `broadcast(topic, record_index, payload)`,
  since the topic alone no longer identifies which record a message came from.

- **`SnapshotProvider::snapshots` returns `Vec<(usize, String, Vec<u8>)>`**
  instead of `Vec<(String, Vec<u8>)>`; the added `usize` is the record id the
  cached value belongs to. Two records publishing on one topic now yield two
  entries rather than one overwriting the other.

- **`ConnectorConfig` carry an additional attr `record_index: Option<usize>`
  to `WsBusSink`**, so record index could join topic at outbound routes.

### Changed (breaking, wire)

- **A late-join burst may carry several `snap` frames for the same topic** — one
  per granted record publishing there — where previously a topic produced at
  most one. Each rides its own `seq`, and `last` still closes the burst, so a
  client tracking sequence numbers is unaffected; a client indexing snapshots by
  topic must expect collisions.

- **`record.query`'s `total` is the number of rows returned**, after
  authorization, rather than the handler's own match count. The trait doc for
  `QueryHandler::handle_query` is updated to say so: a client is never told how
  many rows it was not allowed to see. The built-in persistence handler already
  returned `records.len()`, so only custom handlers change behaviour.

  However, there is a limit to this design: in case `total` < `limit`, it does not mean
  that there is no more record, but records returned from `QueryHandlerFn` are filtered by grants.
  A better design is to resolve search name against grants then dedicate the filtering
  to `QueryHandlerFn` instead of let it stay in `WsSession`.

- **`record.query` distinguishes "not permitted" from "nothing matched".**
  `denied` is returned only when the client holds no read grants at all;
  a client with grants that match no stored rows gets `{"records": [], "total": 0}`.
  Querying a record the server no longer registers is allowed if the grant
  covers its key, so history survives a record's retirement from the config.

- **Subscribing to a topic no records publish on now succeeds and stays silent**
  rather than being refused. Topics are not part of the authorization model any
  more, so the server cannot tell an unregistered topic from one whose records
  the client may not read. Only a client holding no read grants at all is
  refused with `denied`; a client whose grants match no registered record
  subscribes and stays silent, as does any client on a server with no records.

- **`record.list` returns only the records the client may read.** Previously the
  full database was enumerable by any authenticated client.

### Fixed

- **Late-join snapshots no longer lose a record when two share a topic.** The
  snapshot cache was keyed by topic, so the most recent publisher overwrote the
  other's cached value and a late-joining client received only one of them —
  and, once grants became per record, sometimes neither. It is now keyed by
  `(record id, topic)`.

### Security

- **A read grant no longer leaks records it does not name.** Grants lived in
  topic space while records are keyed independently, so a client granted one
  record's topic received every record publishing there — including records it
  held no grant for, over `event` frames, late-join snapshots, `record.list` and
  `record.query` alike. Records whose topic comes from a `TopicProvider` made
  this unbounded, since the topic is chosen per value at runtime. Grants are now
  resolved against record keys at the upgrade and enforced at every delivery
  point, so a record's data reaches only clients granted that record.

## [0.3.0] - 2026-09-18

### Added

- **`GET /version` returns the AimX version this server speaks** as
  `{"aimx": "3.0"}`, beside `/health` and under the same permissive CORS layer.
  The upgrade gate refuses an incompatible client with HTTP 426, which a browser
  cannot read — the WebSocket API surfaces a failed upgrade as an opaque `error`
  event with no status or body, making a version refusal indistinguishable from
  an unreachable host. A browser can now fetch this before dialing and name both
  versions in its error. The gate itself is unchanged. `/version` is now a
  reserved path: an application already serving its own `GET /version` through
  `with_additional_routes` panics on startup, since axum refuses overlapping
  method routes on a merge — drop or rename that route.

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
