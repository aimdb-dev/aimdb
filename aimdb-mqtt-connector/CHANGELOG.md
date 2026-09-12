# Changelog - aimdb-mqtt-connector

All notable changes to the `aimdb-mqtt-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking)

- **The backend split is std vs `no_std`, not Tokio vs Embassy.** The embedded
  backend runs on any target whose adapter supplies a `StreamDialer`, so a new
  platform costs one adapter crate and no change here. Features rename
  accordingly: `std` carries the `rumqttc` backend (`tokio-runtime` is a
  deprecated alias), `embedded` carries `mountain-mqtt` with `alloc` only — no
  executor, network stack, adapter or logger in its graph — and `embassy-runtime`
  becomes a convenience bundle over it. TLS splits the same way: `embedded-tls`
  is runtime-neutral, `embassy-tls` adds the SNTP time source a board with no
  RTC needs. Modules follow: `tokio_client` → `native`, `embassy_client` →
  `embedded` (both kept as deprecated re-exports for one release).
- **One constructor.** `MqttConnector::new(url)` is unconditional, and the
  transport — or its absence — picks the backend, so both compile into one
  binary. Previously the two inherent `new`s collided with `E0034` whenever
  both features were on. Broker URL, client id and credentials moved onto
  `MqttConnector` itself, so `with_client_id` / `with_credentials` work on
  either backend; `with_credentials` now reaches `rumqttc` too, taking
  precedence over the URL authority.
- **`.tls(dialer, options)` replaces `.tls(stack, options)`.** The dialer
  resolves the host, so TLS needs no network stack: DNS, the socket buffers and
  the SNTP task all leave the TLS path. The certificate-validity clock comes
  from `RuntimeOps::unix_time()`; SNTP is opt-in via `TlsOptions::with_sntp`
  for a runtime with no wall clock of its own.
- **The `mountain-mqtt-embassy` fork is absorbed and dropped.** Its state,
  event handler and message pump live in `embedded::manager`, with the mutex
  and the clock as this crate's choices rather than the fork's.
- **Session channels use `CriticalSectionRawMutex` in an `Arc`.** They are
  therefore `Sync`, so `MqttSink` and `MqttSource` are plain `Connector` /
  `Source` impls and the `EmbassySink`/`EmbassySource` force-`Send` spine is
  gone from the data plane. std binaries need a `critical-section` impl; the
  `critical-section-std-impl` feature supplies one, mirroring the KNX connector.
  A single documented `unsafe impl Send` remains on the session future:
  `embedded-io-async` puts no `Send` bound on its futures and the loop reaches
  them through a generic transport, which needs return-type notation to express
  — still unstable on the pinned toolchain. It rests on `StreamDialer`'s
  `Stream: Send` guarantee rather than on a single-core executor, so it holds
  under a preemptive scheduler.
- **Time comes from core's `Delay`**, supplied by the dialer, so the session
  loop names no executor. `Settings` is `core::time::Duration` and lost its
  dead `address`/`port` fields.

### Fixed

- **A broker hostname works on every backend, `mqtt://` and `mqtts://` alike.**
  `setup_manager` vetted plain `mqtt://` hosts with `Ipv4Addr::from_str`, a
  rule inherited from the days when this crate built the `embassy_net`
  address itself. Since the host string now goes to a `StreamDialer`, that gate
  described no dialer in particular: `mqtt://broker.local:1883` connected on
  `Native`, and the same URL with `.transport(TokioNet::tcp())` — a dialer that
  resolves names perfectly well — was rejected at `build()`. On Embassy the
  mirror image bit `mqtts://`, which skips the gate: its hostname reached a
  dialer that parsed only IP literals — and a hostname is the configuration
  `build()` steers TLS users toward — so it reconnect-looped. The gate is gone
  and `EmbassyTcpDialer` resolves (see the adapter's changelog: its `net`
  feature now enables `embassy-net/dns` and each stack needs one more
  `StackResources` slot). `backend_parity` dials `localhost` on both backends.
- **The embedded session's dead retry path is gone, and a dropped publish now
  says so.** `try_action` parked a failed action in `SessionState` for the next
  loop iteration to retry, but both call sites propagated the error with `?`,
  which ends the session — and `run_sessions` built a *fresh* `SessionState`
  per connection, so the parked action was dropped with the old one.
  `take_pending_action` could only ever return `None` and `is_retry` was never
  `true`. The mechanism is removed rather than repaired: the loss window is
  narrow (a dead link is normally found by the 10 ms poll or the 2 s ping, not
  by a publish), and where a publish *is* the detector — a response timeout —
  the broker has most likely already received the message, so a resend would
  duplicate it. An action that fails now logs its topic and the `ClientError`
  before the session ends, so the drop is visible instead of silent, and
  `handle_messages` documents the at-most-once contract: the action in flight
  is lost, everything still queued survives. Dropping the parking slot also
  makes `SessionState` non-generic and removes the unused type parameter it
  forced onto `ChannelEventHandler`.
- **A second connector in one process no longer steals the first's identity.**
  Client id and credentials were parked in process-global `OnceLock`s, so every
  connector after the first connected as the first.
- **One allocation per inbound message instead of two.** The payload is built
  as a `Payload` on arrival rather than as a `Vec` that is converted again.
- **`defmt` is no longer forced on `mountain-mqtt`**, and is absent from the
  `embedded` graph entirely.

### Added

- **Host coverage for the embedded backend**, which previously had none. A fake
  MQTT broker over real sockets drives the session loop on a multi-thread Tokio
  runtime: reconnect-and-resubscribe, record round-trip both ways, both backends
  against one broker in one process, and — the first test the TLS path has ever
  had — an `mqtts://` handshake against a self-signed certificate pinned as the
  root CA, with no SNTP.
- **`#[diagnostic::on_unimplemented]` for a missing backend.** A `no_std` build
  that forgets `.transport(..)` now gets a message naming the fix instead of an
  unsatisfied `ConnectorBuilder` bound.

### Changed

- **One `MqttConnector<B>` over two protocol backends (breaking on Embassy).**
  `Native` is `rumqttc` (QoS 0–2, rustls); `Embedded<D>` is `mountain-mqtt` over
  a caller-supplied transport. The Tokio path is unchanged; Embassy callers now
  write `MqttConnector::new(url).transport(EmbassyNet::tcp(..))` or
  `.tls(stack, opts)` instead of passing the stack to `new`. The
  `Tokio*`/`Embassy*` aliases and `MqttConnectorBuilder` are gone.
- **`run_with_subscriptions` replaced by an owned session loop.** It binds
  `embassy_net::Stack` and cannot take a transport, so reconnect-and-resubscribe
  is now explicit in `transport::run_sessions` — one loop for both plain and
  TLS, extracted from the TLS path already running it.
- **Reports through the `log_*` facade instead of `tracing::` directly** (design
  050 §10.5), so a `log` destination — an FFI layer's, say — sees this crate's
  events too. Each call site also shed the hand-written
  `#[cfg(feature = "tracing")]` the facade carries itself. The `tracing` feature
  no longer pulls `dep:tracing`; a mirrored `log` feature is added alongside it.
  No change to what is emitted, or to a consumer that enables `tracing`.

### Added

- **`transport` — the broker transport seam.** `BrokerTransport` over
  `mountain-mqtt`'s own `Connection` (the client needs a non-blocking peek that
  a byte stream cannot express and TLS cannot provide), plus `SocketTransport`
  bridging from core's `StreamDialer`. A new runtime supplies MQTT by
  implementing that dialer — no code here.
- **`tests/embassy_broker.rs`** — the connector against a fake broker over two
  crossover-wired `embassy-net` stacks, asserting CONNECT *and* SUBSCRIBE reach
  the wire.
- **Tokio client: the TLS backend for `mqtts://` is now a build-time choice.**
  Two new features — `tokio-native-tls` (system OpenSSL, what this crate linked
  before) and `tokio-rustls` (pure Rust, no `libssl`/`libcrypto`) — plus the
  option of neither. Neither is a supported build rather than an oversight: a
  deployment speaking `mqtt://` on a trusted network links no TLS stack at all,
  which for a shared library is the difference between inheriting a system
  OpenSSL ABI and inheriting nothing. `mqtts://` then fails at connect time with
  a message naming the missing feature, instead of at the linker. `make test`
  and `make clippy` gain a leg for each of the three states.
- **Embassy client: TLS (`mqtts://`) and broker authentication ([design 044](../docs/design/044-embassy-mqtt-tls.md), WP7).** New `embassy-tls` feature (`embassy-runtime` + `embedded-tls`/`embedded-io-async`/`rand_core`, `embassy-net/dns`, `embassy-net/udp`) adds an `embedded-tls` 1.3 session over the Embassy TCP socket, with pure-Rust (`rustpki`) certificate verification (`rsa` + `p384`, so public CA chains verify out of the box) and SNI/hostname verification taken from the broker URL. `MqttConnectorBuilder::new` now accepts `mqtts://host[:port]` (default port 8883) alongside plain `mqtt://` (1883); the scheme selects the transport at `build()`. New `MqttConnectorBuilder::with_tls(TlsOptions)` supplies the TLS materials — entropy (`&'static mut dyn CryptoRngCore`, app-owned TRNG), app-provided static record buffers, and the SNTP server address; `build()` errors if `mqtts://` is used without `.with_tls(...)`, if `.with_tls(...)` is used with a plain `mqtt://` URL, or if the `embassy-tls` feature is off. IPv6 broker literals are rejected at `build()` (can never pass certificate verification); IPv4 literals are allowed with a `defmt` warning (only a private CA that pins the dotted quad in its CN will verify). A connector-internal SNTP (UDP) task backs the TLS clock and gates the first handshake on a successful time sync, so certificate validity is always checked. The plain `mqtt://` path is unchanged.
- **`MqttConnectorBuilder::with_credentials(username, password)` (Embassy, design 044 D8).** Feeds the MQTT CONNECT username/password on both the plain and TLS transports. The `aimdb-dev/mountain-mqtt` fork submodule is bumped to pick up upstream 0.4's `ConnectionSettings::with_auth`/`authenticated` (`aimdb-dev/mountain-mqtt@89a7129`).
- `make check` gains an `embassy-runtime,embassy-tls,defmt` clippy leg on `thumbv7em-none-eabihf`.

### Fixed

- **The rustls path no longer builds its configuration through
  `TlsConfiguration::default()`**, which `expect`s on `load_native_certs()` and
  `unwrap`s each `add()`. Two panics on the connect path, in a crate reachable
  through an FFI boundary where a panic is undefined behaviour rather than an
  error. The configuration is now built explicitly, and a machine with no usable
  trust roots gets a message saying so.

### Changed

- **Embassy connector reuses the upstream session loop instead of copying it.** The TLS path no longer duplicates mountain-mqtt-embassy's `handle_messages`/`State`/`ChannelEventHandler`/`try_action` (~195 lines): the `aimdb-dev/mountain-mqtt` fork now exposes them publicly (plus a `run_with_subscriptions`), so the plain and TLS transports share one keep-alive/action-dispatch/event loop and can no longer drift. The plain `mqtt://` path also switches to `run_with_subscriptions`, which **re-subscribes inbound topics on every connection** — previously it queued subscribe actions once at startup, so subscriptions were silently lost after a reconnect. The submodule is bumped to the matching change; no public API change.
- **Embassy broker URL parsing now validates the scheme.** `MqttConnectorBuilder::new`'s URL must be `mqtt://` or `mqtts://` (previously any scheme's host/port were used as-is); this is what selects the transport for the `embassy-tls` change above.

### Changed (breaking)

- **`TlsOptions::new` requires a `Send` RNG** —
  `&'static mut (dyn CryptoRngCore + Send)`. Every concrete CSPRNG already
  satisfies it (`embassy_stm32::rng::Rng` included), so callers are unchanged
  textually. With it, `TlsSlot` becomes core's `OneShot<TlsOptions>` and this
  crate carries **zero `unsafe impl`s** (was two).
- **Issue #131:** the Embassy `MqttConnectorBuilder::new` takes the network stack — `MqttConnectorBuilder::new(broker_url, stack)` — since the deleted `EmbassyNetwork` runtime trait can no longer supply it; both `ConnectorBuilder` impls and the `MqttLinkExt`/`MqttOutboundLinkExt` link-builder ext traits are non-generic over the runtime.

### Added

- **`MqttLinkExt` / `MqttOutboundLinkExt` — the MQTT knobs, now where the protocol lives (Issue #134, design 034 §3.6).** New `link_ext` module (compiled on every feature leg, `alloc`-only) with extension traits over core's generic link builders: `MqttLinkExt::with_qos(u8)` on outbound *and* inbound links (publish / subscribe QoS), and `MqttOutboundLinkExt::with_retain(bool)` on outbound links only (retain is a publish-side flag). They push the exact `("qos", …)` / `("retain", …)` option keys both clients have always read from `protocol_options` — wire behavior identical to the deleted core methods; only an extra `use aimdb_mqtt_connector::{MqttLinkExt, MqttOutboundLinkExt};` is needed. The crate now declares `extern crate alloc` unconditionally.

### Changed

- **Connector-build errors carry their message on `no_std` too (Issue #129).** With `DbError` unified on `alloc::String`, the dual `#[cfg]` error-construction branches in both clients collapse to one `DbError::runtime_error(...)` expression; the Embassy client's "Failed to build MQTT connector" detail is no longer dropped on embedded targets. No API change.
- **Tokio client rebuilt on the shared data-plane toolkit (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** The hand-rolled consume-serialize-publish and read-route loops are replaced by `aimdb-core`'s `pump_sink` / `pump_source` helpers (the connector now writes only its `Connector`/`Source` I/O adapters and composes the pumps in `build()`). Per-route configuration (`qos` / `retain` / `timeout_ms` / …) is threaded from each link URL's query via `ConnectorConfig::from_query`. `std` now enables `aimdb-core/connector-session` (where the pump helpers live; `std` implies it transitively). No public API change.
- **Outbound publisher survives a consumer lag (Embassy client, Issue #39).** A `BufferLagged` (SPMC-ring overflow) on the outbound reader now skips the gap and keeps publishing instead of terminating the publisher; only a closed buffer stops it.
- **M17 — Embassy client rebuilt on core's pumps via the adapter spine ([Design 033](../docs/design/033-M17-unify-connectors-drop-send.md)).** The hand-rolled outbound publisher and inbound event-router loops are gone: the Embassy half now rides core's `pump_sink` / `pump_source` through the force-`Send` `EmbassySink` / `EmbassySource` bridges in `aimdb-embassy-adapter::connectors`, exactly like the Tokio half rides them — this crate contributes only the broker **manager task** (mountain-mqtt's `run`, force-`Send`ed once via `into_box_future`) and the `MqttSink` / `MqttSource` over its action/event channels. **No `unsafe`, no `SendFutureWrapper`** remain in this crate. Per-route `qos` / `retain` still arrive from each link URL's query (now via `ConnectorConfig::protocol_options`, parsed per publish). Note: per-message inbound routing logs moved from this crate's `defmt` calls into core's `pump_source` (`tracing` feature), so defmt-only MCU builds no longer log per-message routing failures.

### Changed (breaking)

- **`ConnectorBuilder::build()` now returns `Vec<BoxFuture<'static, ()>>` instead of `Arc<dyn Connector>` (Issue #88).** Both Tokio and Embassy implementations updated. The MQTT event-loop, the Embassy event-router, and every outbound publisher are returned as futures that the `AimDbRunner` drives — no more `runtime.spawn` / `tokio::spawn` inside the connector. `R: Spawn` bounds dropped throughout in favour of `R: RuntimeAdapter`.
- `spawn_event_loop()` → `build_event_loop_future()` (Tokio side). `spawn_outbound_publishers()` → `collect_outbound_futures()` on both Tokio and Embassy.
- The `transport::Connector` impl on `MqttConnectorImpl` was removed alongside the discarded `Arc<dyn Connector>` return path; direct programmatic publish was already unreachable through the `AimDbBuilder` public API.
- **`MqttConnectorImpl` (Embassy) removed entirely (M17).** It was a build-time aggregation holder; its logic collapsed into the private `setup_manager` + the pump composition in `build()`. Register via `MqttConnectorBuilder` as before — the builder's public API is unchanged.

## [0.6.0] - 2026-05-22

### Changed

- Updated `Router::route()` calls to pass runtime context via `db.runtime_any()`, enabling context-aware deserializers (Design 026)
- Updated outbound publishers (Tokio and Embassy) to dispatch via `SerializerKind`, enabling context-aware serializers with `db.runtime_any()`

## [0.5.1] - 2026-03-16

### Changed

- Updated Embassy dependency versions: executor 0.10.0, time 0.5.1, sync 0.8.0, net 0.9.0

## [0.5.0] - 2026-02-21

### Added

- **Dynamic Topic Routing (Design 018)**: Full support for dynamic MQTT topic resolution
  - **Outbound**: Uses `TopicProvider` to dynamically determine publish topics based on data values. Configure via `.with_topic_provider()` on outbound connectors.
  - **Inbound**: Uses `TopicResolverFn` for late-binding subscription topics at connector startup. Configure via `.with_topic_resolver()` on inbound connectors.
  - Topics resolved at connector startup via `collect_inbound_routes()` and per-message via `TopicProviderFn` for outbound

## [0.4.0] - 2025-12-25

### Fixed

- **MQTT Connector Deadlock with >10 Topics (Issue #63)**: Fixed initialization deadlock when subscribing to more than 10 MQTT topics. The fix has two parts:
  1. **Spawn-before-subscribe**: Event loop is now spawned before subscribing to topics, allowing continuous channel draining
  2. **Dynamic channel capacity**: Channel capacity now scales with topic count (`topics + 10`) instead of hard-coded `10`
  3. Added `tokio::task::yield_now().await` to ensure proper task scheduling before subscriptions

### Changed

- **Dependency Update**: Upgraded `rumqttc` from 0.24 to 0.25

## [0.3.0] - 2025-12-15

### Changed

- **Breaking: Record Registration**: Updated demo examples to use new key-based `configure<T>(key, |reg| ...)` API
- Demo records now have explicit keys (e.g., `"sensor.temp.indoor"`, `"sensor.temp.outdoor"`, `"command.temp.indoor"`)
- Demos refactored to use shared `mqtt-connector-demo-common` crate for cross-platform types and monitors

## [0.2.0] - 2025-11-20

### Added

- **Bidirectional MQTT Support**: Complete rewrite supporting simultaneous publishing and subscribing with automatic message routing
- **Inbound Message Routing**: Automatic routing of incoming MQTT messages to appropriate AimDB producers based on topic patterns
- **ConnectorBuilder Pattern**: New `MqttConnectorBuilder` for both Tokio and Embassy runtimes
- **Automatic Task Spawning**: Background tasks (connection management, message routing) now spawn automatically during `build()`
- **Router Integration**: Uses new `Router` system for type-safe message dispatch to correct record producers
- **Outbound Publisher Support**: Added `spawn_outbound_publishers()` method for both Tokio and Embassy implementations to handle AimDB → MQTT publishing via `ConsumerTrait`

### Changed

- **Breaking: Builder API**: Changed from `MqttConnector::new()` to `MqttConnectorBuilder::new()` with automatic initialization
- **Breaking: Task Management**: Removed manual `mqtt_background_task` spawning - tasks spawn automatically during database `build()`
- **Breaking: Configuration**: Simplified Embassy configuration with automatic network stack access
- **Breaking: Outbound Architecture**: Refactored to use `ConsumerTrait`-based outbound routing:
  - Added: `spawn_outbound_publishers()` method in both Tokio and Embassy implementations
  - Required: Must call `spawn_outbound_publishers()` in `ConnectorBuilder::build()` for outbound publishing to work
  - Changed: Outbound publishing now uses type-erased `ConsumerTrait` instead of automatic spawning
- **Tokio Client**: Refactored for bidirectional support with unified client and automatic reconnection
- **Embassy Client**: Simplified API with integrated task spawning and network stack management
- **Client ID**: Now properly passes client ID from user configuration instead of generating random IDs

### Fixed

- Session persistence with user-configured client IDs
- Reconnection handling in background tasks
- Topic subscription management for inbound routes

### Removed

- Manual background task spawning requirement in Embassy implementation
- Separate consumer registration API (now integrated into builder)

## [0.1.0] - 2025-11-06

### Added

- Initial release of MQTT connector for AimDB
- Dual runtime support for both Tokio and Embassy
- Automatic consumer registration via builder pattern
- Topic mapping with QoS and retain configuration
- Pluggable serializers (JSON, MessagePack, Postcard, custom)
- Automatic reconnection handling
- Uses `rumqttc` for std environments (Tokio)
- Uses `mountain-mqtt` for embedded environments (Embassy)
- Support for MQTT v3.1.1 protocol
- Configurable keep-alive and connection timeouts

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
