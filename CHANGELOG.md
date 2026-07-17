# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note**: This is the global changelog for the AimDB project. For detailed changes to individual crates, see their respective CHANGELOG.md files:
>
> - [aimdb-core/CHANGELOG.md](aimdb-core/CHANGELOG.md)
> - [aimdb-codegen/CHANGELOG.md](aimdb-codegen/CHANGELOG.md)
> - [aimdb-data-contracts/CHANGELOG.md](aimdb-data-contracts/CHANGELOG.md)
> - [aimdb-derive/CHANGELOG.md](aimdb-derive/CHANGELOG.md)
> - [aimdb-tokio-adapter/CHANGELOG.md](aimdb-tokio-adapter/CHANGELOG.md)
> - [aimdb-embassy-adapter/CHANGELOG.md](aimdb-embassy-adapter/CHANGELOG.md)
> - [aimdb-mqtt-connector/CHANGELOG.md](aimdb-mqtt-connector/CHANGELOG.md)
> - [aimdb-knx-connector/CHANGELOG.md](aimdb-knx-connector/CHANGELOG.md)
> - [aimdb-websocket-connector/CHANGELOG.md](aimdb-websocket-connector/CHANGELOG.md)
> - [aimdb-uds-connector/CHANGELOG.md](aimdb-uds-connector/CHANGELOG.md)
> - [aimdb-serial-connector/CHANGELOG.md](aimdb-serial-connector/CHANGELOG.md)
> - [aimdb-tcp-connector/CHANGELOG.md](aimdb-tcp-connector/CHANGELOG.md)
> - [aimdb-ws-protocol/CHANGELOG.md](aimdb-ws-protocol/CHANGELOG.md)
> - [aimdb-wasm-adapter/CHANGELOG.md](aimdb-wasm-adapter/CHANGELOG.md)
> - [aimdb-sync/CHANGELOG.md](aimdb-sync/CHANGELOG.md)
> - [aimdb-client/CHANGELOG.md](aimdb-client/CHANGELOG.md)
> - [aimdb-persistence/CHANGELOG.md](aimdb-persistence/CHANGELOG.md)
> - [aimdb-persistence-sqlite/CHANGELOG.md](aimdb-persistence-sqlite/CHANGELOG.md)
> - [tools/aimdb-cli/CHANGELOG.md](tools/aimdb-cli/CHANGELOG.md)
> - [tools/aimdb-mcp/CHANGELOG.md](tools/aimdb-mcp/CHANGELOG.md)

## [Unreleased]

### Changed — knx-pico submodule

- **Submodule:** bump `_external/knx-pico` to upstream `0.3` (commit 158325bd4)

### Added — per-link record codecs

- **Issue #178:** one `Linkable` record type can now select JSON, bounded
  Postcard, or a custom codec independently on each inbound/outbound connector
  route through `linked_*_with(url, codec)` or builder-level
  `with_link_codec(codec)`. Selection is fused at registration time; bounded
  Postcard routes retain the reusable scratch/owned-overflow behavior from
  issue #177 without a per-message registry, lock, or lookup. See
  [Design 045](docs/design/045-per-link-codec-selection.md). Re-selecting a
  codec replaces both the owned and scratch serializers, so changing a route
  from bounded postcard to owned JSON cannot retain stale postcard output.

### Changed — Design 038 simplification pass (breaking)

Implementation of the accepted items of [design 038](docs/design/038-technical-debt-and-simplification-review.md)
(§3.1, §3.3–§3.8, §3.11, D10, plus the CI drift guards). Net −2,500 lines across the
library crates with no capability loss. Breaking changes and migrations:

- **`aimdb-executor` is retired (§3.1).** `RuntimeOps`, `LogLevel`, `BoxFuture`, and
  `ExecutorError`/`ExecutorResult` now live in `aimdb_core::executor` (re-exported at the
  crate root). The superseded generic trait family (`RuntimeAdapter`, `TimeOps`, `Logger`,
  `Runtime`, `RuntimeInfo`), `aimdb_core::time` (`TimestampProvider`/`SleepCapable`), and
  the pre-runner spawn surface (`TokioAdapter::spawn_task`, `TokioBuffer::spawn_dispatcher`,
  `EmbassyBuffer::dispatcher_task`) are deleted. *Migration:* `use aimdb_executor::X` →
  `use aimdb_core::X`; adapters implement one trait (`RuntimeOps`) instead of four.
- **`DbError` diet (§3.3).** Deleted with zero production consumers: `with_context()`,
  `into_anyhow()`, the ten `is_*()` predicates, the numeric `error_code()`/`error_category()`
  registry, `RESOURCE_TYPE_*`, and the never-constructed `AmbiguousType`,
  `DuplicateRecordKey`, `ResourceUnavailable`, and `HardwareError` variants (with the dead
  `EmbassyErrorSupport`/`TokioErrorSupport` traits). Blocking-facade errors move to the new
  **`aimdb_sync::SyncError`** (`AttachFailed`, `DetachFailed`, `SetTimeout`, `GetTimeout`,
  `RuntimeShutdown`, `Db(DbError)`); all aimdb-sync APIs return `SyncResult<T>`.
- **Dead API pruned (§3.8).** `RecordT` + `register_record(_with_key)` (self-registering
  records; `configure()` is the API), `ConnectorLink::with_config`, `RecordValue::cloned`,
  and the `outbound_connector_count` trait method (use `outbound_connectors().len()`).
- **Internal consolidation (§3.4–§3.6).** The registry collapses to one `Vec<RecordEntry>` +
  key map (`records_of_type` now returns `Vec<RecordId>`); `RecordIntrospect` and
  `RecordMetricsReset` fold into `AnyRecord` (`JsonRecordAccess` stays); `AnyRecord::validate()`
  is gone; writer exclusivity (`.source()`/`.transform()`/`.link_from()`) is validated once,
  in `build()`, reported as `"conflicting writers: …"` with the record key attached.
- **`LinkAddress` replaces `ConnectorUrl` for record links (§3.13/D5).** `.link_to()`/
  `.link_from()` addresses parse as plain `scheme://resource` — no host/port/credential
  parsing; `ConnectorUrl` remains the connector-constructor endpoint URL. Phantom
  `default_port` entries (kafka/http/https) are gone.
- **Tokio log backend (§3.13).** `TokioAdapter` forwards `ctx.log()` to the `log` facade —
  the binary picks the backend (env_logger, tracing-log, …). With no backend configured it
  falls back to plain `[info]`-style stdout so demos work without setup; the emoji
  `println!` backend is gone.
- **Serializer API (§3.7).** `with_serializer_raw`/`with_deserializer_raw` are gone.
  *Migration:* `.with_serializer_raw(|v| …)` → `.with_serializer(|_ctx, v| …)` (deserializer
  likewise).
- **Feature collapse (§3.11).** `observability` = the old `metrics` + `profiling`;
  `remote` = the old `json-serialize` + `remote-access`; `connector-session` stays
  independent. *Migration:* rename the features; there are no aliases.
- **CI drift guards (§2.6/§3.10).** `make readme-check` compiles the README quickstart
  verbatim (`examples/readme-quickstart`) and diffs it against the README;
  `make codegen-drift` compiles aimdb-codegen's generated output (common crate, hub crate,
  flat schema) against the workspace. Both run in CI. The README quickstart itself and three
  already-drifted codegen templates (0-input source tasks, inbound deserializer arity, the
  flat schema's missing registrar-ext import) are fixed.
- **D10 docs cleanup.** ~170 provenance-narration comment sites ("design 036 W1",
  "issue #133", "pre-W8") rewritten to state the invariant directly; history lives in git
  blame and docs/design/.

### Changed — no_std typed migrations

Schema migration is the last capability trait freed from `std`, so a versioned
contract can migrate old payloads on a bare-metal target (e.g. Embassy on an MCU),
not just under `std`. Verified on `thumbv7em-none-eabihf` in CI, with a
multi-step migration round-trip suite and a trybuild compile-fail harness.

- **`migratable` no longer requires `std` (breaking).** The feature is now
  `["alloc", "serde_json", "dep:aimdb-derive"]` (was `["std", "serde_json"]`), and
  the crate's `serde_json` follows the workspace pin (`default-features = false`,
  `features = ["alloc"]`). `migration_chain!` and the `Linkable`-with-migration
  patterns now compile on `no_std + alloc`. *Migration:* a crate that enabled only
  `migratable` and relied on it transitively activating `std` must now enable `std`
  explicitly; in-repo consumers default to `std` and are unaffected.
  ([aimdb-data-contracts](aimdb-data-contracts/CHANGELOG.md))
- **`migration_chain!` moves to an `aimdb-derive` proc-macro.** The 3-arm
  `macro_rules!` is replaced by a variable-arity proc-macro (re-exported unchanged as
  `aimdb_data_contracts::migration_chain!`) with `O(N)`-in-code-size dispatch and a
  tree-free version probe in `migrate_from_bytes` (peak allocation O(concrete struct),
  not O(payload tree)). A below-`MIN_VERSION` source version now reports `VersionTooOld`
  instead of the contradictory `VersionTooNew`.
  ([aimdb-derive](aimdb-derive/CHANGELOG.md))

### Added

- **Zero-allocation generated Postcard encoding (Issue #177).** `Linkable` gains a source-compatible `encode_into(&mut [u8])` fast path and generated Postcard records implement it with `postcard::to_slice`. Core reuses onebounded 256-byte scratch allocation per generated Postcard outbound route (raw builders choose their capacity) and falls back to the existing owned serializer when a value does not fit. The codec seam is gated by a 10,000-iteration allocation-counting bench (0 allocations, 0 bytes) plus
  host, `no_std` Cortex-M, and codegen-drift tests. The claim intentionally excludes connector-internal payload copies and the existing boxed connector reader future. ([aimdb-core](aimdb-core/CHANGELOG.md),
  [aimdb-data-contracts](aimdb-data-contracts/CHANGELOG.md),
  [aimdb-codegen](aimdb-codegen/CHANGELOG.md))
- **Embassy MQTT client TLS — `mqtts://` for embedded, WP7 ([design 044](docs/design/044-embassy-mqtt-tls.md)).** The Embassy MQTT connector can now dial a broker over TLS 1.3 (`embedded-tls`, pure-Rust `rustpki` certificate verification with `rsa`/`p384` so public CA chains verify out of the box), with SNI/hostname verification, DNS resolution, and a connector-internal SNTP time source that gates the first handshake so certificate validity is always checked against real time. Behind the new `embassy-tls` feature; `MqttConnectorBuilder::new` picks the transport from the URL scheme (`mqtt://` plain, `mqtts://` TLS) and gains `with_tls(TlsOptions)` (entropy, record buffers, SNTP server) and `with_credentials(username, password)` (MQTT CONNECT auth, both transports). The plain `mqtt://` path is untouched. The `embassy-mqtt-connector-demo` example gains a `tls` feature demonstrating the full flow against the repo's `dev/mosquitto` bench broker. ([aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md))
- **Design 034 Phase 3 — sans-io KNX/IP tunneling engine shared by both transports (Issue #135, [review doc §3.7](docs/design/034-technical-debt-review.md)).** The entire tunneling lifecycle — CONNECT_REQUEST/RESPONSE handshake, TUNNELING_REQUEST/ACK sequence + pending-ACK bookkeeping, keepalive (CONNECTIONSTATE_REQUEST) scheduling, ACK-timeout sweeps, and reconnect-with-backoff — now lives **once**, in the new runtime-neutral `aimdb_knx_connector::tunnel` module (`no_std + alloc`, no tokio/embassy imports), driven as a poll-based state machine (events in, `Action`s out, `next_deadline()` for timer arming). `tokio_client.rs` (988 → ~530 lines incl. a new fake-gateway integration test) and `embassy_client.rs` (1,055 → ~450 lines) are reduced to socket shims; the previously untestable handshake/ACK/keepalive/reconnect paths now have 15 host-run unit tests plus a scripted localhost-UDP roundtrip test. Behavioral unifications: Embassy gains the 5 s CONNECT_RESPONSE timeout (previously waited forever), both shims reconnect on fatal socket errors, and the dead tokio-only per-publish ACK oneshot is dropped (it was always `None`); the CONNECT_REQUEST HPAI stays per-transport (`LocalEndpoint`: tokio = real bound address, Embassy = NAT mode). ([aimdb-knx-connector](aimdb-knx-connector/CHANGELOG.md))

- **Design 034 Phase 2 — dyn-safe `RuntimeOps` capability trait (Issue #130, [review doc](docs/design/034-technical-debt-review.md)).** New object-safe trait in `aimdb-executor` (`name` / `now_nanos` / `unix_time` / boxed `sleep` / `log(LogLevel, …)`) so a runtime adapter can travel as `Arc<dyn RuntimeOps>` instead of a generic parameter — the groundwork for removing `R` from the record object graph (#131). Implemented by `TokioAdapter`, `EmbassyAdapter`, and `WasmAdapter`, each covered by a shared behavioral contract test. `BoxFuture`'s canonical definition moves to `aimdb-executor` (re-exported unchanged from `aimdb-core`). ([aimdb-executor](aimdb-executor/CHANGELOG.md), [aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-wasm-adapter](aimdb-wasm-adapter/CHANGELOG.md))

- **M17 — centralized Embassy connector spine: one audited home for the single-core `unsafe` ([Design 033](docs/design/033-M17-unify-connectors-drop-send.md)).** New `aimdb-embassy-adapter::connectors` module (features `connectors` / `connector-io`) collects the force-`Send` plumbing every Embassy connector used to hand-roll: session transports get `EmbassySessionClient`/`EmbassySessionServer`, `OneShotDialer`/`OneShotListener`/`OneShotCell`, and the framed `EmbassyConnection` + `Framer`; data-plane transports get the `EmbassySink`/`EmbassySource` bridges (over `EmbassySinkRaw`/`EmbassySourceRaw`) that ride core's existing `pump_sink`/`pump_source`, plus `into_box_future` for protocol tasks. The serial Embassy half is now thin sugar (just a COBS `Framer`) with **zero `unsafe`** (down from a 407-line hand-roll with 7 `unsafe impl`s); the MQTT and KNX Embassy halves dropped their hand-rolled publisher/router loops and `SendFutureWrapper` use to ride core's pumps (KNX inbound telegrams now flow through `pump_source`). All connector-crate `unsafe`/`SendFutureWrapper` is gone — confined to the adapter. The std/Tokio side, `aimdb-client`, the WebSocket server, examples, and tests are unchanged. (Chosen over Design 033's original "drop `Send` from the contract", which would have pushed `!Send` onto the std side; see the doc's Implementation Decision.) ([aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-serial-connector](aimdb-serial-connector/CHANGELOG.md), [aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md), [aimdb-knx-connector](aimdb-knx-connector/CHANGELOG.md))

- **Remote access via connectors — Phases 0–6: converge four hand-rolled networking stacks onto two shared engines (Issue #39, [design doc](docs/design/remote-access-via-connectors.md)).** AimX remote access (and any future transport) now rides the connector layer instead of a bespoke I/O abstraction. New, runtime-neutral `aimdb-core::session` module (feature `connector-session`, `no_std + alloc`): the three-layer substrate (`Connection`/`Listener`/`Dialer` + `EnvelopeCodec` + `Dispatch`/`Session`), the reactive **server** engine (`serve`/`run_session`) and proactive **client** engine (`run_client`/`pump_client`), the `pump_sink`/`pump_source` data-plane toolkit, and the transport-agnostic `SessionClientConnector`/`SessionServerConnector`. The AimX-v2 NDJSON protocol (`session::aimx`: `AimxCodec` + `AimxDispatch`) and the WebSocket connector are ports onto this substrate, so the AimX server/client and WS server/client stacks collapse onto the two engines. New **`aimdb-uds-connector`** crate carries the UDS transport (`UdsClient`/`UdsServer`). ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-uds-connector](aimdb-uds-connector/CHANGELOG.md), [aimdb-websocket-connector](aimdb-websocket-connector/CHANGELOG.md), [aimdb-client](aimdb-client/CHANGELOG.md), [aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md), [aimdb-knx-connector](aimdb-knx-connector/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md))
- **M16 — JSON codec extracted behind the `json-serialize` feature; `RecordValue::as_json()` now works on `no_std + alloc`, not just `std` ([Design 032](docs/design/032-M16-aimx-json-codec.md)).** New `aimdb-core::codec` module: `RemoteSerialize` (blanket-impl'd for every `serde` `Serialize + DeserializeOwned` type), the object-safe `JsonCodec<T>`, and the zero-sized `SerdeJsonCodec`. `serde_json` runs on `alloc`, so embedded targets can opt in; `std` enables the feature transitively, so std builds are unaffected. ([aimdb-core](aimdb-core/CHANGELOG.md))
- **Embassy buffer + join-queue tests now run in CI (Issue #85).** The join-queue tests previously sat behind `embassy-runtime`, which pulls `embassy-executor`'s cortex-m assembly and can't compile under `cargo test` on x86_64 — so ordering / backpressure / clone-routing regressions were never caught. The `join_queue` module is now gated on `embassy-sync`, and `make test` runs the embassy adapter's unit tests + doctests on the host (no executor). Also adds `EmbassyBuffer::peek()` and fixes a stale `EmbassyBuffer` doc example. ([aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md))
- **`no_std` AimX server — a board can serve a host, not just dial one (Issue #120, follow-up to #39).** Cross-cutting de-std of `aimdb-core`'s central record API behind a new **`remote-access`** feature (`= ["json-serialize", "thiserror"]`, transitively enabled by `std`): the type-erased `AnyRecord` JSON + metadata methods, the `AimDb` JSON read/write/subscribe API, the `remote` module (config / protocol / security / error), and the AimX server dispatch (`AimxDispatch`/`AimxSession`) now all compile on `no_std + alloc` — swapping `std::collections` → `hashbrown`, `std::sync::Arc` → `alloc::sync::Arc`, and `thiserror` to `default-features = false`. Adds a runtime-neutral wall clock, `TimeOps::unix_time()`, implemented from the OS clock on Tokio and from an `EmbassyAdapter::set_unix_time(...)` anchor on Embassy. Verified by a new `thumbv7em-none-eabihf` dispatch cross-check in the Makefile. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-executor](aimdb-executor/CHANGELOG.md), [aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-uds-connector](aimdb-uds-connector/CHANGELOG.md), [tools/aimdb-mcp](tools/aimdb-mcp/CHANGELOG.md))
- **`aimdb-serial-connector` — COBS-framed serial/UART transport, the headline embedded scenario (Issue #122, follow-up to #39 / #120).** New crate: a sensor MCU dials a gateway over UART (and, on the `no_std` `AimxDispatch` from #120, an MCU *serves* a host over UART). Contributes only the `Dialer`/`Listener`/`Connection` triple + `SerialClient`/`SerialServer` sugar over the `serial://` scheme; reuses the AimX codec/dispatch and the session engines from core. Same compact AimX JSON, framed with **COBS** + a `0x00` sentinel (self-synchronizing on a lossy serial line). Dual halves: a std `tokio-runtime` (`tokio-serial`) riding the generic `Session*Connector`, and a `no_std + alloc` `embassy-runtime` generic over `embedded-io-async` UART halves that hand-rolls `ConnectorBuilder` and force-`Send`s the single-core futures via `SendFutureWrapper`. Cross-compiles to `thumbv7em-none-eabihf` (new Makefile `test-embedded` checks). Ships a real end-to-end demo: a host `serial_demo` example and an `embassy-serial-connector-demo` STM32H563ZI firmware (serves a record over USART3 ↔ the ST-LINK Virtual COM Port, queried from the host). The workspace `embedded-io-async` dep is bumped 0.6 → 0.7 to match the Embassy STM32 HAL. ([aimdb-serial-connector](aimdb-serial-connector/CHANGELOG.md))
- **Transport-agnostic host client — pick the transport at runtime with `--connect <scheme://url>` (Issue #123, follow-up to #39 / #122).** `aimdb-client` gains an `endpoint` resolver (`unix://` / `uds://` / bare path / `serial://DEVICE?baud=N`) over a new `impl Dialer for Box<dyn Dialer>` in `aimdb-core`; `AimxConnection::connect` takes an endpoint string, and transports are opt-in features (`transport-uds` default, `transport-serial` off-by-default). The `aimdb` CLI replaces the per-command `--socket` flags with a global `--connect` (+ `AIMDB_CONNECT`), and `aimdb-mcp` tools take an `endpoint` param with `--connect`/`AIMDB_CONNECT` (was `socket_path`/`--socket`/`AIMDB_SOCKET`). See **breaking** below. ([aimdb-client](aimdb-client/CHANGELOG.md), [tools/aimdb-cli](tools/aimdb-cli/CHANGELOG.md), [tools/aimdb-mcp](tools/aimdb-mcp/CHANGELOG.md), [aimdb-core](aimdb-core/CHANGELOG.md))

### Changed (breaking)

- **Design 036 W1 — data-plane de-`Any`: per-message type erasure removed from the connector SPI ([design doc §W1](docs/design/036-followup-refactoring.md)).** #131 removed the control-plane erasure; this removes the per-message kind. The typed pipeline is now fused inside closures at registration time, so no `Box<dyn Any>` is constructed on any per-message path (connectors, session pumps, AimX client mirroring). Inbound: a sync `IngestFn` (deserialize + produce in one typed closure — the per-message boxed future disappears too; `Router::route` becomes a sync fn with a mandatory `RuntimeContext`) replaces `DeserializerKind` + `ProducerTrait::produce_any`. Outbound: a fused `SerializedSource`/`SerializedReader` yielding `SerializedValue { dest, payload }` replaces `ConsumerTrait::subscribe_any` → `AnyReader::recv_any` → `SerializerKind(&dyn Any)`, and absorbs the third erasure crossing (`TopicProviderAny::topic_any` — the typed `TopicProvider<T>` is now stored directly). `SerializeError::TypeMismatch` and the dead `ConnectorClient`/`OutboundConnectorLink` are deleted. The user-facing registrar API is **source-compatible** (serializer/deserializer/topic-provider registration signatures unchanged — examples, codegen output, and aimdb-pro compile untouched); `JoinTrigger` deliberately keeps its erasure (it *is* the multi-type join API, documented on the type). Behavior pinned by the unmodified KNX fake-gateway and session smoke tests. ([aimdb-core](aimdb-core/CHANGELOG.md))

- **Design 034 Phase 3 — runtime type parameter `R` removed from the object graph (Issue #131, [review doc §3.2/§3.3](docs/design/034-technical-debt-review.md)).** The runtime is now a *value* (`Arc<dyn aimdb_executor::RuntimeOps>`, the #130 groundwork) instead of a type parameter; the only generic left on the user-facing object graph is the record type `T`. The full break inventory:
  - **Types lose `R`:** `AimDb<R>` → `AimDb`, `AimDbBuilder<R>` → `AimDbBuilder` (the `NoRuntime` typestate is gone — a missing runtime is a `build()` error per the #133 contract), `TypedRecord<T, R>` → `TypedRecord<T>`, `RecordRegistrar<'a, T, R>` → `RecordRegistrar<'a, T>`, `TransformBuilder<I, O, R>` → `TransformBuilder<I, O>`, `JoinBuilder<O, R>` → `JoinBuilder<O>`, `RecordT<R>` → `RecordT`, and `ConnectorBuilder<R>` → `ConnectorBuilder` (its `build()` takes `&AimDb`). Turbofish updates: `get_typed_record_by_key::<T, R>` → `::<T>`, `as_typed::<T, R>` → `::<T>`.
  - **`RuntimeContext` is a concrete struct** wrapping `Arc<dyn RuntimeOps>`. `ctx.time().now()` returns **`u64` nanoseconds** from an arbitrary monotonic epoch (was `R::Instant`); `sleep` takes a plain `core::time::Duration` (plus `sleep_millis`/`sleep_secs` helpers); `millis()`/`secs()`/`micros()`/`duration_since()`/`duration_as_nanos()` are deleted (durations are concrete, instants are integer math); the panicking `extract_from_any` is deleted.
  - **`source`/`tap`/`transform`/`transform_join` are inherent methods** on `RecordRegistrar` — the per-adapter extension-trait wrappers and `aimdb-core`'s `ext_macros.rs` are gone, as are the `*_raw` variants (the inherent methods *are* the foundational API; closures keep the `(ctx, producer)` arg order, so user code mostly just drops an import). The adapter ext traits (`TokioRecordRegistrarExt`, `EmbassyRecordRegistrarExt`(+`Custom`), `WasmRecordRegistrarExt`) shrink to the one genuinely adapter-specific step: `.buffer(cfg)` construction.
  - **`JoinFanInRuntime` deleted** (with `JoinQueue`/`JoinSender`/`JoinReceiver` and all three per-adapter `join_queue.rs` files): multi-input join fan-in now uses one bounded `async-channel` queue in core — the same primitive the session engine already uses on tokio, Embassy, and WASM. Capacity stays 64 on `std`/wasm32 and rises to 16 on embedded `no_std` (up from Embassy's 8); the queue now closes on every runtime when all forwarders exit (the Embassy queue previously never closed).
  - **Runtime accessors:** `runtime_arc()` → `runtime_ops()` (returns `Arc<dyn RuntimeOps>`), new `runtime_ctx()`; the borrowed `AimDb::runtime()` and the type-erased `runtime_any()` are deleted (zero callers in aimdb/aimdb-pro — `&*db.runtime_ops()` covers the borrowed flavor; [follow-up doc §2.5](docs/design/035-review-followups-deferred.md)). `AimDbBuilder::on_start` closures receive `RuntimeContext` (was `Arc<R>`). Context-aware (de)serializers receive the concrete `RuntimeContext` (was `Arc<dyn Any + Send + Sync>`); `Router::route`'s ctx argument follows. The `RuntimeForProfiling` marker-trait workaround is deleted (profiling clocks ride `RuntimeOps::now_nanos`); the session client engine's clock is `Arc<dyn RuntimeOps>` (was `Arc<R: TimeOps>`).
  - **Embassy network capability moves to construction:** the `EmbassyNetwork` runtime trait is deleted (a `dyn RuntimeOps` can't surface adapter-specific capabilities) along with `EmbassyAdapter::new_with_network` — `EmbassyAdapter` is a stateless unit type with **zero `unsafe`**. The Embassy MQTT/KNX connector builders take the `embassy_net::Stack` at construction (`MqttConnectorBuilder::new(url, stack)` / `KnxConnectorBuilder::new(url, stack)`), wrapped in the new force-`Send + Sync` `aimdb_embassy_adapter::connectors::NetStack` so the single-core `unsafe` stays in the one audited module.
  - **Downstream follows mechanically:** `aimdb-sync` (`AimDb` handles), `aimdb-persistence` (`.persist()`/`.with_persistence()` ext traits de-genericized), `aimdb-data-contracts::log_tap`, `aimdb-codegen` templates (generated `configure_schema(builder: &mut AimDbBuilder)`), and all examples/tools.
  - Acceptance: `grep -rn "extract_from_any|runtime_any|RuntimeForProfiling|JoinFanInRuntime"` over the workspace sources returns nothing; the remaining `dyn Any` in core is data-plane only (`SerializerFn`/`produce_any`/`JoinTrigger`/`TopicProviderAny` — the #131 §6 stretch, tracked as a follow-up). ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-executor](aimdb-executor/CHANGELOG.md), [aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-wasm-adapter](aimdb-wasm-adapter/CHANGELOG.md), [aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md), [aimdb-knx-connector](aimdb-knx-connector/CHANGELOG.md), [aimdb-persistence](aimdb-persistence/CHANGELOG.md), [aimdb-sync](aimdb-sync/CHANGELOG.md))

- **Design 034 Phase 2 — MQTT knobs move out of core; `ConnectorConfig` pruned (Issue #134, [review doc §3.6](docs/design/034-technical-debt-review.md)).** Core's generic link builders drop `with_qos`/`with_retain` (`with_timeout_ms` stays, de-MQTT'd); the knobs now live in `aimdb-mqtt-connector` as the `MqttLinkExt` (qos, outbound + inbound) and `MqttOutboundLinkExt` (retain, publish-side only) extension traits, pushing the **same** `("qos", …)`/`("retain", …)` option keys the MQTT clients have always read — wire behavior unchanged; importing the trait makes the MQTT intent explicit at the call site (generic escape hatch: `with_config(key, value)`). `ConnectorConfig` loses its never-read typed `qos`/`retain` fields and the speculative Kafka/HTTP/shmem interpretation docs — it keeps `timeout_ms` + `protocol_options`, and core now documents no protocol that lacks an in-tree connector. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md))

- **Design 034 Phase 2 — panic-free builder validation: `build()` reports every configuration mistake at once (Issue #133, [review doc §3.4](docs/design/034-technical-debt-review.md)).** Builder methods never panic on user mistakes anymore. Conflicting `.source()`/`.transform()`/`.link_from()` registrations, missing serializers/deserializers, invalid connector URLs, unregistered schemes, and key-reused-with-different-type are *recorded* (the conflicting registration is skipped) and `build()` returns one `DbError::InvalidConfiguration { errors: Vec<ConfigError> }` carrying **all** findings — each with the record key and, where applicable, the connector URL. The worst panic — "requires a buffer" firing at spawn time inside a connector factory closure — is now a build()-time check, which also makes `.buffer()` after `.link_to()`/`.link_from()` legal (order-independent). Duplicate keys and dependency-graph cycles fold into the same collected report (previously distinct `DuplicateRecordKey`/`CyclicDependency` returns from `build()`). Remaining `panic!`/`expect`s in the builder path are internal invariants and say "this is a bug in aimdb-core". ([aimdb-core](aimdb-core/CHANGELOG.md))

- **Design 034 Phase 2 — registrar lifetime fix + de-erased builder internals (Issue #130, [review doc](docs/design/034-technical-debt-review.md)).** `RecordRegistrar`'s fluent methods now take fresh borrows (`&mut self -> &mut Self`) instead of borrowing the registrar for its entire lifetime — a `configure` closure can finally use separate statements (`reg.source_raw(…); reg.tap_raw(…);`) and reuse the registrar after a chain. `configure`'s closure bound drops its HRTB; `OutboundConnectorBuilder`/`InboundConnectorBuilder` gain a second lifetime parameter (`<'r, 'a, T, R>`); `RecordT::register` and the adapter/persistence extension traits follow. Internally, `AimDbBuilder` stores its spawn/start functions typed (`SpawnFnType<R>`/`StartFnType<R>`) instead of `Box<dyn Any>` — the panicking downcasts in `build()` are gone, and `AimDb<R>`'s struct-level bound moved to its impls. Closure-based user code compiles unchanged. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-persistence](aimdb-persistence/CHANGELOG.md))

- **Design 034 Phase 1 — mechanical debt cleanup (Issues #129, #132, [review doc](docs/design/034-technical-debt-review.md)).** `DbError` is unified on `alloc::string::String`: every variant has one shape on all targets, `thiserror` derives `Display`/`Error` unconditionally (now a mandatory no_std dependency of `aimdb-core`), and no_std builds produce the same error messages as std builds instead of `Error 0xNNNN` codes — on no_std the `_field: ()` placeholders become the real `String` fields. The dead `Database<A>` wrapper, the `TokioDatabase`/`EmbassyDatabase` aliases, and the deprecated `RecordRegistrar::link()` are removed. `ConsumerTrait::subscribe_any` is infallible (returns `Box<dyn AnyReader>`), `OutboundRoute` is a struct with named fields, and `aimdb-core`'s `std` feature no longer pulls a `tokio` dependency (tests cover it via dev-deps). Internally: all dual std/alloc import pairs and the duplicated `RuntimeContext` impl blocks are collapsed, and crate-private `log_*!` macros replace the 62 per-call-site `#[cfg(feature = "tracing")]` gates. The `aimdb-wasm-adapter` ring-lag error now carries a `buffer_name` like the other adapters. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md), [aimdb-knx-connector](aimdb-knx-connector/CHANGELOG.md))

- **CLI/MCP endpoint surface reworked to `--connect <url>` (Issue #123).** `aimdb`'s per-command `--socket <path>` is replaced by a global `--connect <endpoint>` (`AIMDB_CONNECT` env; bare paths still work). `aimdb-mcp` renames the tools' `socket_path` param to `endpoint`, the startup `--socket` to `--connect`, and `AIMDB_SOCKET` to `AIMDB_CONNECT`; the pool is keyed by endpoint URL. `aimdb-client`'s `AimxConnection::connect` takes a `&str` endpoint (was a path) and `ClientError::ConnectionFailed.socket` is renamed `endpoint`. Serial endpoints (`serial://…`) require building the CLI/MCP with `--features transport-serial`. ([aimdb-client](aimdb-client/CHANGELOG.md), [tools/aimdb-cli](tools/aimdb-cli/CHANGELOG.md), [tools/aimdb-mcp](tools/aimdb-mcp/CHANGELOG.md))

- **`AimDbBuilder::with_remote_access(config)` removed — remote-access servers are now registered like any other connector (Issue #39).** Replace `.with_remote_access(config)` with `.with_connector(aimdb_uds_connector::UdsServer::from_config(config))`. The AimX wire was reshaped to **v2** (NDJSON tagged frames mapping onto the engine's role-neutral message set) and is **not** backward-compatible with the legacy AimX v1 framing; the bundled `aimdb-client` / CLI / MCP speak v2. The UDS transport types moved out of `aimdb-core` into `aimdb-uds-connector`. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-uds-connector](aimdb-uds-connector/CHANGELOG.md), [aimdb-client](aimdb-client/CHANGELOG.md))
- **M15 — `latest_snapshot` removed; point-in-time reads go through the new buffer-native `DynBuffer::peek()` ([Design 031](docs/design/031-M15-remove-latest-snapshot.md)).** `TypedRecord::latest()` and AimX `record.get` read the buffer directly instead of a per-record snapshot mutex (one lock + clone off the `produce()` hot path). Consequences: a `.with_remote_access()` record with **no buffer** now fails `build()` (was a silent runtime no-op); `record.get` / `latest()` on an `SpmcRing` record returns `not_found` / `None` (rings have no canonical latest — use `record.drain` / `record.subscribe`); `SingleLatest` and `Mailbox` are unaffected. `TypedRecord::produce` is removed — all writes go through `WriteHandle::push`. Adapters implement `peek()` per buffer type. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-wasm-adapter](aimdb-wasm-adapter/CHANGELOG.md))
- **M16 — `with_remote_access()` now requires the `json-serialize` feature (transitively enabled by `std`); `with_read_only_serialization()` removed ([Design 032](docs/design/032-M16-aimx-json-codec.md)).** The stored serializer/deserializer closures are replaced by a type-erased `Arc<dyn JsonCodec<T>>`. A `Serialize`-only record can no longer be exposed read-only over remote access. ([aimdb-core](aimdb-core/CHANGELOG.md))
- **`RecordMetadata` drops `created_at` / `last_update`; `AimxConfig.socket_path` is now `String` (Issue #120).** Core stopped keeping per-record timestamp state for the `no_std` AimX server (the `Mutex`-+-`SystemTime` `RecordMetadataTracker` is deleted), so AimX `record.list` and the MCP `list_records` / schema outputs no longer include the two timestamp fields. `RecordMetadata::new` drops its `created_at` parameter. `AimxConfig::socket_path` / `UdsServer::new` take `impl Into<String>` instead of `impl Into<PathBuf>` (`&Path` / `PathBuf` callers must convert). ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-uds-connector](aimdb-uds-connector/CHANGELOG.md), [tools/aimdb-mcp](tools/aimdb-mcp/CHANGELOG.md))

- **M13 — `Spawn` trait removed across the workspace; `AimDbBuilder::build()` now returns `(AimDb, AimDbRunner)` (Issue #88, [Design 028](docs/design/028-M13-remove-spawn-trait.md)).** Every future the database drives — producer services, taps, transforms, join forwarders, connector loops, the remote-access supervisor, `on_start` tasks — is collected at build time and driven by a single `FuturesUnordered` inside `runner.run().await`. Adapter implementations (`TokioAdapter`, `EmbassyAdapter`, `WasmAdapter`) drop their `impl Spawn`. The `embassy-task-pool-8/16/32` features are deleted and `EmbassyAdapter::new_with_network` no longer takes a `Spawner`. Connector authors must update `ConnectorBuilder::build()` to return `Vec<BoxFuture>` instead of `Arc<dyn Connector>`. See each crate's CHANGELOG for the per-crate impact.

## [1.1.0] - 2026-05-22

### Added

- **Automatic stage profiling (Issue #58, RFC 014)**: New `profiling` feature on `aimdb-core` automatically measures wall-clock time per `.source()` / `.tap()` / `.link()` stage and exposes `call_count` / `total` / `avg` / `min` / `max` counters per stage. Name stages with `RecordRegistrar::with_name("...")`. Works on `no_std + alloc` via the runtime clock (`aimdb_executor::TimeOps`) and `portable-atomic` for 64-bit atomics on `thumbv7em`. New MCP tools `get_stage_profiling` (with bottleneck detection + recommendation) and `reset_stage_profiling`. Feature is off by default and zero-cost when disabled. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-executor](aimdb-executor/CHANGELOG.md), [aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-wasm-adapter](aimdb-wasm-adapter/CHANGELOG.md), [aimdb-client](aimdb-client/CHANGELOG.md), [tools/aimdb-mcp](tools/aimdb-mcp/CHANGELOG.md))
- **`remote-access-demo` exercises stage profiling**: The `Temperature` and `SystemStatus` records now run from in-AimDB `.source()` + `.tap()` tasks (with `.with_name(...)` stage labels) and the demo enables the `profiling` feature. `SystemStatus.slow_status_processor` deliberately sleeps 100 ms per value so `get_stage_profiling` flags it as the bottleneck. README documents how to query and reset profiling via MCP / `socat`.
- **`hello-mailbox` example (Issue #94)**: New minimal example demonstrating the Mailbox buffer (latest-wins) semantics using the synchronous API. First community contribution from [@ggmaldo](https://github.com/ggmaldo) — see [examples/hello-mailbox/](examples/hello-mailbox/).
- **Writer-exclusivity validation (Issue #89)**: Combining `.source()`, `.transform()`, and `.link_from()` on the same record now panics at configuration time with a clear message instead of silently producing a last-writer-wins race on the buffer. Multiple `.link_from()` inbound connectors (fan-in) remain allowed. ([aimdb-core](aimdb-core/CHANGELOG.md))
- **`no_std` Transform API (Design 027)**: `.transform()` and `.transform_join()` are now available on `no_std + alloc` targets — no longer Tokio-only. Multi-input join fan-in moved out of `aimdb-core` into the new `JoinFanInRuntime` traits in `aimdb-executor`, with implementations in the Tokio (`mpsc::channel`, capacity 64), Embassy (`embassy_sync::Channel`, capacity 8), and WASM (`futures_channel::mpsc`, capacity 64) adapters. ([aimdb-core](aimdb-core/CHANGELOG.md), [aimdb-executor](aimdb-executor/CHANGELOG.md), [aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md), [aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md), [aimdb-wasm-adapter](aimdb-wasm-adapter/CHANGELOG.md))
- **Task-model join handler**: New `JoinBuilder::on_triggers(FnOnce(JoinEventRx, Producer) -> impl Future)` API replaces the previous callback model. Eliminates per-event heap allocation and lets handler state borrow across `.await` points. **Breaking change** vs. the old `with_state().on_trigger(...)` form — see [aimdb-core](aimdb-core/CHANGELOG.md).
- **Weather-mesh `DewPoint` demo**: All three weather stations (alpha, beta, gamma) now derive a `DewPoint` record from `Temperature` and `Humidity` via `transform_join`, demonstrating the API end-to-end on Tokio and Embassy.
- Design document: 027 (`no_std` Support for Transform API)
- **MCP public mode**: New `--public` flag restricts the MCP server to read-only tools for safe internet-facing deployments with SSRF protection ([tools/aimdb-mcp](tools/aimdb-mcp/CHANGELOG.md))
- **MCP `--socket` flag**: Default socket path can be set at startup, simplifying single-instance workflows ([tools/aimdb-mcp](tools/aimdb-mcp/CHANGELOG.md))
- **Context-Aware Deserializers (Design 026)**: Inbound connector deserializers can now receive a `RuntimeContext<R>` for platform-independent timestamps and logging
  - New `.with_deserializer(|ctx, bytes| ...)` API on `InboundConnectorBuilder` provides `RuntimeContext<R>` to deserialization closures
  - New `.with_deserializer_raw(|bytes| ...)` for plain bytes-only deserialization when context is unnecessary
  - `DeserializerKind` enum enforces mutual exclusivity between raw and context-aware deserializers
  - `Router::route()` propagates optional runtime context to context-aware routes
- **Context-Aware Serializers**: Outbound connector serializers can now receive a `RuntimeContext<R>`, symmetric with deserializers
  - New `.with_serializer(|ctx, value| ...)` API on `OutboundConnectorBuilder` provides `RuntimeContext<R>` to serialization closures
  - New `.with_serializer_raw(|value| ...)` for plain value-only serialization when context is unnecessary
  - `SerializerKind` enum (`Raw` / `Context`) enforces mutual exclusivity
  - All outbound connector publishers updated to propagate runtime context via `db.runtime_any()`
- Design document: 026 (Context-Aware Deserializers)

### Changed

- **aimdb-executor**: `TimeOps` trait gained a required `duration_as_nanos(duration) -> u64` method (runtime-agnostic numeric representation of elapsed time, used by stage profiling). Implemented in tokio, embassy, and wasm adapters. ([aimdb-executor](aimdb-executor/CHANGELOG.md))
- **aimdb-embassy-adapter**: `SpmcRing` subscriber-slot exhaustion now emits a `defmt::error!` with guidance to increase the `CONSUMERS` const generic. Counting rule: one slot per `.tap()`, `.link_to()`, and `transform_join` input.
- **aimdb-codegen**: Generated join handler stubs updated to the new `on_triggers` task model (`async fn task_handler(JoinEventRx, Producer<...>)`).
- **aimdb-core**: Breaking API changes to `InboundConnectorLink`, `Router`, and `RouterBuilder` to support `DeserializerKind` (see [aimdb-core/CHANGELOG.md](aimdb-core/CHANGELOG.md))
- **aimdb-core**: Breaking API change — `ConnectorLink.serializer` now stores `SerializerKind` instead of `SerializerFn`
- **aimdb-core**: `.with_serializer()` renamed to `.with_serializer_raw()` for the old single-argument pattern
- **aimdb-mqtt-connector**: Updated router dispatch for new `route()` signature; outbound publishers dispatch via `SerializerKind`
- **aimdb-knx-connector**: Updated router dispatch for new `route()` signature; outbound publishers dispatch via `SerializerKind`
- **aimdb-websocket-connector**: Updated router dispatch for new `route()` signature; outbound publishers dispatch via `SerializerKind`
- All connector examples updated to use new `.with_deserializer(|_ctx, bytes| ...)` and `.with_serializer_raw(|value| ...)` signatures
- **`rand` 0.8 → 0.10.1**: Upgraded across workspace (`aimdb-data-contracts`, `aimdb-embassy-adapter`, examples). Migrated API: `gen` → `random`, `SmallRng` seed size 16 → 32, added `RngExt` imports.
- **README**: Reordered quickstart — remote MCP exploration (zero install) is now step 2, local Docker setup moved to step 3.

## [1.0.0] - 2026-03-16

### Added

- **aimdb-core** promoted to **1.0.0** — stable public API milestone
- **aimdb-websocket-connector**: New crate for real-time bidirectional WebSocket streaming
  - Server mode (Axum-based) with `link_to("ws://topic")` for pushing data to browser clients
  - Client mode (tokio-tungstenite) with `link_to("ws-client://host/topic")` for AimDB-to-AimDB sync
  - `AuthHandler` trait for pluggable authentication
  - Client session management with automatic cleanup and late-join support
  - `StreamableRegistry` for extensible type-erased dispatch — register types via `.register::<T>()`
- **aimdb-ws-protocol**: New crate with shared wire protocol types for the WebSocket ecosystem
  - `ServerMessage` and `ClientMessage` enums with JSON-encoded `"type"` discriminant
  - MQTT-style wildcard topic matching (`#` multi-level, `*` single-level)
- **aimdb-wasm-adapter**: New crate enabling AimDB to run in the browser via WebAssembly
  - Full `aimdb-executor` trait implementations (`RuntimeAdapter`, `Spawn`, `TimeOps`, `Logger`)
  - `Rc<RefCell<…>>` buffers — zero-overhead for single-threaded WASM
  - `WasmDb` facade via `#[wasm_bindgen]` with `configureRecord`, `get`, `set`, `subscribe`
  - `WsBridge` — WebSocket bridge connecting browser to remote AimDB server
  - `SchemaRegistry` for type-erased record dispatch with extensible `.register::<T>()` API
  - React hooks: `useRecord<T>`, `useSetRecord<T>`, `useBridge`
- **aimdb-codegen**: New crate for architecture-to-code generation
  - `ArchitectureState` type for reading `.aimdb/state.toml` decision records
  - Mermaid diagram generation (`generate_mermaid`)
  - Rust source generation (`generate_rust`) — value structs, key enums, `SchemaType`/`Linkable` impls, `configure_schema()` scaffolding
  - Common crate, hub crate, and binary crate scaffolding
  - State validation module for architecture integrity checks
- **aimdb-data-contracts**: Refocused as a pure trait-definition crate (`SchemaType`, `Streamable`, `Linkable`, `Simulatable`, `Observable`, `Migratable`)
  - `Streamable` trait as capability marker for types crossing serialization boundaries
  - `Migratable` trait with `MigrationChain` and `MigrationStep` for schema evolution
  - Concrete contracts (Temperature, Humidity, GpsLocation) moved to application-level crates
  - Removed closed `StreamableVisitor` dispatcher in favor of extensible registry pattern
  - Removed `ts` feature (`ts-rs` dependency)
- **aimdb-core**: Added `ws://` and `wss://` URL scheme support in `ConnectorUrl` for WebSocket connectors
- **tools/aimdb-cli**: New `aimdb generate` subcommand for Mermaid diagrams, Rust schema generation, and crate scaffolding via `aimdb-codegen`
- **tools/aimdb-cli**: New `aimdb watch` subcommand for live record monitoring
- **tools/aimdb-mcp**: Architecture agent module with session state machine (`Idle → Gathering → Proposing → Resolve`)
- **tools/aimdb-mcp**: 16+ new MCP tools: `propose_add_record`, `propose_add_connector`, `propose_modify_buffer`, `propose_modify_fields`, `propose_modify_key_variants`, `remove_record`, `rename_record`, `reset_session`, `resolve_proposal`, `save_memory`, `validate_against_instance`, `get_architecture`, `get_buffer_metrics`, and more
- **tools/aimdb-mcp**: Architecture MCP resources — Mermaid diagram and validation results
- **tools/aimdb-mcp**: `CONVENTIONS.md` asset for architecture agent prompts
- Design documents: 023 (Architecture Agent), 024 (Codegen Common Crate), 025 (WASM Adapter)

### Changed

- **aimdb-data-contracts**: Restructured from schema+contract crate to pure trait-definition crate (version reset to 0.1.0)
- **aimdb-websocket-connector**: Replaced closed dispatcher with extensible `StreamableRegistry` — users register types via builder `.register::<T>()`
- **aimdb-wasm-adapter**: `SchemaRegistry` updated to use extensible `.register::<T>()` pattern
- **aimdb-knx-connector**: Updated Embassy dependency versions (executor 0.10.0, time 0.5.1, sync 0.8.0, net 0.9.0)
- **aimdb-mqtt-connector**: Updated Embassy dependency versions (executor 0.10.0, time 0.5.1, sync 0.8.0, net 0.9.0)
- Embassy submodules updated to latest upstream commits

### Published Crates

| Crate | Previous | New |
|-------|----------|-----|
| `aimdb-core` | 0.5.0 | **1.0.0** |
| `aimdb-data-contracts` | 0.5.0 | **0.1.0** |
| `aimdb-cli` | 0.5.0 | **0.6.0** |
| `aimdb-mcp` | 0.5.0 | **0.6.0** |
| `aimdb-codegen` | — | **0.1.0** (new) |
| `aimdb-websocket-connector` | — | **0.1.0** (new) |
| `aimdb-ws-protocol` | — | **0.1.0** (new) |
| `aimdb-wasm-adapter` | — | **0.1.1** (new) |
| `aimdb-tokio-adapter` | 0.5.0 | unchanged |
| `aimdb-embassy-adapter` | 0.5.0 | unchanged |
| `aimdb-client` | 0.5.0 | unchanged |
| `aimdb-sync` | 0.5.0 | unchanged |
| `aimdb-mqtt-connector` | 0.5.0 | **0.5.1** |
| `aimdb-knx-connector` | 0.3.0 | **0.3.1** |
| `aimdb-persistence` | 0.1.0 | unchanged |
| `aimdb-persistence-sqlite` | 0.1.0 | unchanged |
| `aimdb-derive` | 0.1.0 | unchanged |
| `aimdb-executor` | 0.1.0 | unchanged |

## [0.5.0] - 2026-02-21

### Added

- **aimdb-persistence**: New crate providing a pluggable persistence layer for long-term record history
  - `PersistenceBackend` trait with `store`, `query`, `cleanup`, and `initialize` hooks
  - `AimDbBuilderPersistExt` trait adding `.with_persistence(backend, retention)` to `AimDbBuilder<R>`
  - `RecordRegistrarPersistExt` trait adding `.persist(record_name)` to `RecordRegistrar<T, R>`
  - `AimDbQueryExt` trait with `query_latest`, `query_range`, and `query_raw` methods
  - Automatic 24-hour retention cleanup task registration
- **aimdb-persistence-sqlite**: New crate providing a SQLite backend for `aimdb-persistence`
  - `SqliteBackend` with WAL journal mode, actor-model command dispatch, and window-function queries
  - Pattern-based queries with `*` wildcard support and optional time/row-count limits
  - Dedicated OS writer thread; `Clone` = `O(1)` handle copy
- **aimdb-data-contracts**: New crate for portable, versioned AimDB data schemas
  - Optional features: `observable`, `simulatable`, `migratable`, `ts` (TypeScript bindings via `ts-rs`)
  - Schema versioning and migration support
- **Transform API (Design 020)** in `aimdb-core`: Reactive data transformations between records
  - `transform_raw()` for single-input derivations and `transform_join_raw()` for multi-input joins
  - `TransformBuilder` and `JoinBuilder` fluent APIs with optional stateful handlers
  - Transforms spawned as tasks during `AimDb::build()`; mutual exclusion with `.source()` enforced
- **Graph Introspection API (Design 021)** in `aimdb-core` and `aimdb-client`:
  - `RecordOrigin` enum classifying record sources (`Source`, `Link`, `Transform`, `TransformJoin`, `Passive`)
  - `GraphNode`, `GraphEdge`, `DependencyGraph` types for full graph representation
  - `graph_nodes()`, `graph_edges()`, `graph_topo_order()` methods on `AimDb` and `AimDbClient`
- **Record Drain API (Design 019)** in `aimdb-core`, `aimdb-tokio-adapter`, and `aimdb-client`:
  - `try_recv()` on `BufferReader` for non-blocking batch pulls
  - `record.drain` AimX protocol method with cold-start semantics and optional limit
  - `DrainResponse` type in `aimdb-client`
- **Dynamic Topic/Address Routing (Design 018)** in `aimdb-core`, `aimdb-mqtt-connector`, `aimdb-knx-connector`:
  - `TopicProvider<T>` trait for per-message outbound topic/address determination
  - `TopicResolverFn` for late-binding inbound topic resolution at connector startup
  - `with_topic_provider()` and `with_topic_resolver()` builder methods on connector links
- **Graph Tools** in `tools/aimdb-mcp`:
  - `graph_nodes`, `graph_edges`, `graph_topo_order` MCP tools for LLM-powered graph exploration
  - `drain_record` MCP tool for batch history access with persistent cold-start reader
- **Graph Commands** in `tools/aimdb-cli`:
  - `aimdb graph nodes`, `graph edges`, `graph order`, `graph dot` subcommands
  - Color-coded output and DOT format export for Graphviz visualization
- **Extension Macro** in `aimdb-core`: `impl_record_registrar_ext!` for generating runtime adapter extension traits

### Changed

- **aimdb-core**: Renamed `.with_serialization()` to `.with_remote_access()` for clearer semantics (breaking)
- **aimdb-core**: `RecordId::new()` now requires a `RecordOrigin` parameter for dependency graph support (breaking)
- **aimdb-core**: `set_from_json` now also rejects writes on records with active transforms
- **aimdb-core**: `collect_outbound_routes()` returns `OutboundRoute` tuples with optional `TopicProviderFn`
- **aimdb-core**: `collect_inbound_routes()` calls `link.resolve_topic()` for dynamic topic resolution
- **tools/aimdb-mcp**: Replaced real-time subscription system with simpler drain-based polling
  - Removed `subscribe_record`, `unsubscribe_record`, `list_subscriptions`, `get_notification_directory` tools
  - Simplified architecture reduces infrastructure complexity for LLM clients

### Published Crates

| Crate | Previous | New |
|-------|----------|-----|
| `aimdb-core` | 0.4.0 | **0.5.0** |
| `aimdb-tokio-adapter` | 0.4.0 | **0.5.0** |
| `aimdb-embassy-adapter` | 0.4.0 | **0.5.0** |
| `aimdb-client` | 0.4.0 | **0.5.0** |
| `aimdb-sync` | 0.4.0 | **0.5.0** |
| `aimdb-mqtt-connector` | 0.4.0 | **0.5.0** |
| `aimdb-knx-connector` | 0.2.0 | **0.3.0** |
| `aimdb-cli` | 0.4.0 | **0.5.0** |
| `aimdb-mcp` | 0.4.0 | **0.5.0** |
| `aimdb-persistence` | — | **0.1.0** (new) |
| `aimdb-persistence-sqlite` | — | **0.1.0** (new) |
| `aimdb-derive` | 0.1.0 | unchanged |
| `aimdb-executor` | 0.1.0 | unchanged |

## [0.4.0] - 2025-12-25

### Added

- **aimdb-derive**: New crate providing `#[derive(RecordKey)]` macro for compile-time checked record keys
  - `#[key = "..."]` attribute for string representation
  - `#[key_prefix = "..."]` attribute for namespace prefixing
  - `#[link_address = "..."]` attribute for connector metadata (MQTT topics, KNX addresses)
  - Auto-generates `Hash` implementation to satisfy `Borrow<str>` contract
- **aimdb-core**: `RecordKey` is now a **trait** instead of a struct, enabling user-defined enum keys
  - `RecordKey` trait with `as_str()` and optional `link_address()` methods
  - `StringKey` type replaces old `RecordKey` struct (static/interned string keys)
  - `derive` feature flag to enable `#[derive(RecordKey)]` macro
  - Blanket implementation for `&'static str`
- **examples**: New `mqtt-connector-demo-common` and `knx-connector-demo-common` crates for shared cross-platform types and monitors
- **docs**: Added design documents:
  - `016-M6-record-key-trait.md` - RecordKey trait design
  - `017-M6-record-key-hash-impl.md` - Hash implementation for Borrow compliance

### Fixed

- **aimdb-mqtt-connector**: Fixed initialization deadlock when subscribing to >10 MQTT topics (Issue #63)
  - Event loop now spawned before subscribing (spawn-before-subscribe pattern)
  - Dynamic channel capacity scales with topic count (`topics + 10`)

### Changed

- **aimdb-mqtt-connector**: Upgraded `rumqttc` dependency from 0.24 to 0.25
- **deny.toml**: Added `OpenSSL` license allowance (transitive via rumqttc/rustls)
- **deny.toml**: Ignored `RUSTSEC-2025-0134` advisory (rustls-pemfile unmaintained but not vulnerable)

## [0.3.0] - 2025-12-15

### Added

- **aimdb-core**: New `RecordId` type for stable O(1) record indexing (Issue #60)
- **aimdb-core**: New `RecordKey` type for string-based record identification with zero-alloc static keys
- **aimdb-core**: Key-based producer/consumer API: `produce_by_key()`, `subscribe_by_key()`, `producer_by_key()`, `consumer_by_key()`
- **aimdb-core**: `ProducerByKey<T, R>` and `ConsumerByKey<T, R>` types for key-bound access
- **aimdb-core**: `records_of_type::<T>()` introspection method to find all records of a given type
- **aimdb-core**: `resolve_key()` method for O(1) key-to-RecordId lookups
- **aimdb-core**: New error variants: `RecordKeyNotFound`, `InvalidRecordId`, `TypeMismatch`, `AmbiguousType`, `DuplicateRecordKey`
- **aimdb-core**: `RecordMetadata` now includes `record_id` and `record_key` fields
- **aimdb-tokio-adapter**: Multi-instance tests validating same-type different-key scenarios
- **aimdb-core**: New `BufferMetrics` trait and `BufferMetricsSnapshot` struct for buffer introspection (feature-gated behind `metrics`)
  - `produced_count`: Total items pushed to the buffer
  - `consumed_count`: Total items consumed across all readers
  - `dropped_count`: Total items dropped due to lag (with per-reader semantics documented)
  - `occupancy`: Current buffer fill level as `(current, capacity)` tuple
- **aimdb-core**: `RecordMetadata` now includes optional buffer metrics fields when `metrics` feature is enabled
- **aimdb-tokio-adapter**: Full `BufferMetrics` implementation for all buffer types (SPMC Ring, SingleLatest, Mailbox)
- **aimdb-tokio-adapter**: Comprehensive test suite for metrics (`metrics_tests` module)
- **Makefile**: Added test targets for `metrics` feature combinations
- **docs**: Added design document `015-M6-record-id-architecture.md` for future RecordId-based storage

### Changed

- **aimdb-core**: `configure<T>()` now requires a key parameter: `configure::<T>("key", |reg| ...)`
- **aimdb-core**: Internal storage changed from `BTreeMap<TypeId>` to `Vec` + `HashMap` indexes for O(1) hot-path access
- **aimdb-core**: `SecurityPolicy::ReadWrite` now uses `HashSet<String>` for writable record keys
- **aimdb-core**: Added `hashbrown` dependency for no_std-compatible HashMap
- **deny.toml**: Added `Zlib` license allowance (used by `foldhash`, hashbrown's default hasher)
- **aimdb-core**: `DynBuffer` trait no longer has a blanket implementation. Each adapter now provides its own explicit implementation. This enables adapters to provide `metrics_snapshot()` when the `metrics` feature is enabled.

### Migration Guide

**Breaking: `configure<T>()` now requires a key parameter**

The record registration API now requires an explicit key for each record:

```rust
// Before (v0.2.x)
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SingleLatest);
});

// After (v0.3.0)
builder.configure::<Temperature>("sensor.temperature", |reg| {
    reg.buffer(BufferCfg::SingleLatest);
});
```

**Key naming conventions:**

- Use dot-separated hierarchical names: `"sensors.indoor"`, `"config.app"`
- Keys must be unique across all records (duplicate keys panic at registration)
- For single-instance records, any descriptive string works (e.g., `"sensor.temperature"`, `"app.config"`)

**Breaking: Type-based lookup may return `AmbiguousType` error**

If you register multiple records of the same type, type-based methods (`produce()`, `subscribe()`, `producer()`, `consumer()`) will return `AmbiguousType` error:

```rust
// With multiple Temperature records registered...
db.produce(temp).await  // Returns Err(AmbiguousType { count: 2, ... })

// Use key-based methods instead:
db.produce_by_key("sensors.indoor", temp).await  // Works correctly
```

**New key-based API (recommended for multi-instance scenarios):**

```rust
// Key-bound producer/consumer
let indoor_producer = db.producer_by_key::<Temperature>("sensors.indoor");
let outdoor_producer = db.producer_by_key::<Temperature>("sensors.outdoor");

// Each produces to its own record
indoor_producer.produce(temp).await?;
outdoor_producer.produce(temp).await?;

// Introspection
let temp_ids = db.records_of_type::<Temperature>();  // Returns &[RecordId]
let id = db.resolve_key("sensors.indoor");  // Returns Option<RecordId>
```

**Breaking: DynBuffer no longer has blanket impl**

If you have custom `Buffer<T>` implementations, you now need to also implement `DynBuffer<T>`:

```rust
// Before (v0.2.x) - automatic via blanket impl
impl<T: Clone + Send> Buffer<T> for MyBuffer<T> { ... }
// DynBuffer was automatically implemented

// After (v0.3.0) - explicit implementation required
impl<T: Clone + Send> Buffer<T> for MyBuffer<T> { ... }

impl<T: Clone + Send + 'static> DynBuffer<T> for MyBuffer<T> {
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }
    
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }
    
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    
    // Optional: implement metrics_snapshot() if you support metrics
    #[cfg(feature = "metrics")]
    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        None // or Some(...) if you track metrics
    }
}
```

## [0.2.0] - 2025-11-20

### Summary

This release introduces **bidirectional connector support**, enabling true two-way data synchronization between AimDB and external systems. The new architecture supports simultaneous publishing and subscribing with automatic message routing, working seamlessly across both Tokio (std) and Embassy (no_std) runtimes.

### Highlights

- 🔄 **Bidirectional Connectors**: New `.link_to()` and `.link_from()` APIs for clear directional data flows
- 🎯 **Type-Erased Router**: Automatic routing of incoming messages to correct typed producers
- 🏗️ **ConnectorBuilder Pattern**: Simplified connector registration with automatic initialization
- 📡 **Enhanced MQTT Connector**: Complete rewrite supporting simultaneous pub/sub with automatic reconnection
- 🌐 **Embassy Network Integration**: Connectors can now access Embassy's network stack for network operations
- 📚 **Comprehensive Guide**: New 1000+ line connector development guide with real-world examples

### Breaking Changes

- **aimdb-core**: `.build()` is now async; connector registration changed from `.with_connector(scheme, instance)` to `.with_connector(builder)`
- **aimdb-core**: `.link()` deprecated in favor of `.link_to()` (outbound) and `.link_from()` (inbound)
- **aimdb-core**: Outbound connector architecture refactored to trait-based system:
  - Removed: `TypedRecord::spawn_outbound_consumers()` method (was called automatically)
  - Added: `ConsumerTrait`, `AnyReader` traits for type-erased outbound routing
  - Added: `AimDb::collect_outbound_routes()` method to gather configured routes
  - **Required**: Connectors must implement `spawn_outbound_publishers()` and call it in `ConnectorBuilder::build()`
- **aimdb-mqtt-connector**: API changed from `MqttConnector::new()` to `MqttConnectorBuilder::new()`; automatic task spawning removes need for manual background task management
- **aimdb-mqtt-connector**: Added `spawn_outbound_publishers()` method; must be called in `build()` for outbound publishing to work

### Modified Crates

See individual changelogs for detailed changes:

- **[aimdb-core](aimdb-core/CHANGELOG.md)**: Core connector architecture, router system, bidirectional APIs
- **[aimdb-tokio-adapter](aimdb-tokio-adapter/CHANGELOG.md)**: Connector builder integration
- **[aimdb-embassy-adapter](aimdb-embassy-adapter/CHANGELOG.md)**: Network stack access, connector support
- **[aimdb-mqtt-connector](aimdb-mqtt-connector/CHANGELOG.md)**: Complete bidirectional rewrite for both runtimes
- **[aimdb-sync](aimdb-sync/CHANGELOG.md)**: Compatibility with async build

### Migration Guide

**1. Update connector registration:**

```rust
// Old (v0.1.0)
let mqtt = MqttConnector::new("mqtt://broker:1883").await?;
builder.with_connector("mqtt", Arc::new(mqtt))

// New (v0.2.0)
builder.with_connector(MqttConnectorBuilder::new("mqtt://broker:1883"))
```

**2. Make build() async:**

```rust
// Old (v0.1.0)
let db = builder.build()?;

// New (v0.2.0)
let db = builder.build().await?;
```

**3. Use directional link methods:**

```rust
// Old (v0.1.0)
.link("mqtt://sensors/temp")

// New (v0.2.0)
.link_to("mqtt://sensors/temp")    // For publishing (AimDB → External)
.link_from("mqtt://commands/temp")  // For subscribing (External → AimDB)
```

**4. Remove manual MQTT task spawning (Embassy):**

```rust
// Old (v0.1.0) - Manual task spawning required
let mqtt_result = MqttConnector::create(...).await?;
spawner.spawn(mqtt_task(mqtt_result.task))?;
builder.with_connector("mqtt", Arc::new(mqtt_result.connector))

// New (v0.2.0) - Automatic task spawning
builder.with_connector(MqttConnectorBuilder::new("mqtt://broker:1883"))
// Tasks spawn automatically during build()
```

**5. Update custom connectors to spawn outbound publishers:**

If you've implemented a custom connector, you **must** add `spawn_outbound_publishers()` support:

```rust
// Old (v0.1.0) - Outbound consumers spawned automatically
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        // ... setup code ...
        Ok(Arc::new(MyConnector { /* fields */ }))
    }
    // Outbound publishing happened automatically via TypedRecord::spawn_outbound_consumers()
}

// New (v0.2.0) - Must explicitly spawn outbound publishers
impl ConnectorBuilder for MyConnectorBuilder {
    fn build<R>(&self, db: &AimDb<R>) -> DbResult<Arc<dyn Connector>> {
        // ... setup code ...
        let connector = MyConnector { /* fields */ };
        
        // REQUIRED: Collect and spawn outbound publishers
        let outbound_routes = db.collect_outbound_routes(self.protocol_name());
        connector.spawn_outbound_publishers(db, outbound_routes)?;
        
        Ok(Arc::new(connector))
    }
}

// REQUIRED: Implement spawn_outbound_publishers method
impl MyConnector {
    fn spawn_outbound_publishers<R: RuntimeAdapter + 'static>(
        &self,
        db: &AimDb<R>,
        routes: Vec<(String, Box<dyn ConsumerTrait>, SerializerFn, Vec<(String, String)>)>,
    ) -> DbResult<()> {
        for (topic, consumer, serializer, _config) in routes {
            let client = self.client.clone();
            let topic_clone = topic.clone();
            
            db.runtime().spawn(async move {
                // Subscribe to record updates using ConsumerTrait
                match consumer.subscribe_any().await {
                    Ok(mut reader) => {
                        loop {
                            match reader.recv_any().await {
                                Ok(value) => {
                                    // Serialize and publish
                                    if let Ok(bytes) = serializer(&*value) {
                                        let _ = client.publish(&topic_clone, bytes).await;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    Err(_) => { /* Log error */ }
                }
            })?;
        }
        Ok(())
    }
}
```

**Why this change?** The new trait-based architecture provides:

- ✅ Symmetry with inbound routing (`ProducerTrait` ↔ `ConsumerTrait`)
- ✅ Testability (can mock `ConsumerTrait` without real records)
- ✅ Type safety via factory pattern (type capture at configuration time)
- ✅ Maintainability (connector logic stays in connector crate)

**Migration checklist for custom connectors:**

- [ ] Add `spawn_outbound_publishers()` method to connector implementation
- [ ] Call `db.collect_outbound_routes(protocol_name)` in `ConnectorBuilder::build()`
- [ ] Call `connector.spawn_outbound_publishers(db, routes)?` before returning
- [ ] Use `ConsumerTrait::subscribe_any()` to get type-erased readers
- [ ] Handle serialization with provided `SerializerFn`
- [ ] Test both inbound (`.link_from()`) and outbound (`.link_to()`) data flows

See [Connector Development Guide](docs/design/012-M5-connector-development-guide.md) for complete examples.

### Documentation

- Added comprehensive [Connector Development Guide](docs/design/012-M5-connector-development-guide.md)
- Updated MQTT connector examples for both Tokio and Embassy
- Enhanced API documentation with bidirectional patterns

## [0.1.0] - 2025-11-06

### Added

#### Core Database (`aimdb-core`)

- Initial release of AimDB async in-memory database engine
- Type-safe record system using `TypeId`-based routing
- Three buffer types for different data flow patterns:
  - **SPMC Ring Buffer**: High-frequency data streams with bounded memory
  - **SingleLatest**: State synchronization and configuration updates
  - **Mailbox**: Commands and one-shot events
- Producer-consumer model with async task spawning
- Runtime adapter abstraction for cross-platform support
- `no_std` compatibility for embedded targets
- Error handling with comprehensive `DbResult<T>` and `DbError` types
- Remote access protocol (AimX v1) for cross-process introspection

#### Runtime Adapters

- **Tokio Adapter** (`aimdb-tokio-adapter`): Full-featured std runtime support
  - Lock-free buffer implementations
  - Configurable buffer capacities
  - Comprehensive async task spawning
- **Embassy Adapter** (`aimdb-embassy-adapter`): Embedded `no_std` runtime support
  - Configurable task pool sizes (8/16/32 concurrent tasks)
  - Optimized for resource-constrained devices
  - Compatible with ARM Cortex-M targets

#### MQTT Connector (`aimdb-mqtt-connector`)

- Dual runtime support for both Tokio and Embassy
- Automatic consumer registration via builder pattern
- Topic mapping with QoS and retain configuration
- Pluggable serializers (JSON, MessagePack, Postcard, custom)
- Automatic reconnection handling
- Uses `rumqttc` for std environments
- Uses `mountain-mqtt` for embedded environments

#### Developer Tools

- **MCP Server** (`aimdb-mcp`): LLM-powered introspection and debugging
  - Discover running AimDB instances
  - Query record values and schemas
  - Subscribe to live updates
  - Set writable record values
  - JSON Schema inference from record values
  - Notification persistence to JSONL files
- **CLI Tool** (`aimdb-cli`): Command-line interface (skeleton)
  - Instance discovery and management commands
  - Record inspection capabilities
  - Live watch functionality
- **Client Library** (`aimdb-client`): Reusable connection and discovery logic
  - Unix domain socket communication
  - AimX v1 protocol implementation
  - Clean error handling

#### Synchronous API (`aimdb-sync`)

- Blocking wrapper around async AimDB core
- Thread-safe synchronous record access
- Automatic Tokio runtime management
- Ideal for gradual migration from sync to async

#### Documentation & Examples

- Comprehensive README with architecture overview
- Individual crate documentation with examples
- 12 detailed design documents in `/docs/design`
- Working examples:
  - `tokio-mqtt-connector-demo`: Full Tokio MQTT integration
  - `embassy-mqtt-connector-demo`: Embedded RP2040 with WiFi MQTT
  - `sync-api-demo`: Synchronous API usage patterns
  - `remote-access-demo`: Cross-process introspection server

#### Build & CI Infrastructure

- Comprehensive Makefile with color-coded output
- GitHub Actions workflows:
  - Continuous integration (format, lint, test)
  - Security audits (weekly schedule + on-demand)
  - Documentation generation
  - Release automation
- Cross-compilation testing for `thumbv7em-none-eabihf` target
- `cargo-deny` configuration for license and dependency auditing
- Dev container setup for consistent development environment

### Design Goals Achieved

- ✅ Sub-50ms latency for data synchronization
- ✅ Lock-free buffer operations
- ✅ Cross-platform support (MCU → edge → cloud)
- ✅ Type safety with zero-cost abstractions
- ✅ Protocol-agnostic connector architecture

### Known Limitations

- Kafka and DDS connectors planned for future releases
- CLI tool is currently skeleton implementation
- Performance benchmarks not yet included
- Limited to Unix domain sockets for remote access (no TCP yet)

### Dependencies

- Rust 1.75+ required
- Tokio 1.47+ for std environments
- Embassy 0.9+ for embedded environments
- See `deny.toml` for approved dependency licenses

### Breaking Changes

None (initial release)

### Migration Guide

Not applicable (initial release)

---

## Release Notes

### v0.1.0 - "Foundation Release"

AimDB v0.1.0 establishes the foundational architecture for async, in-memory data synchronization across MCU → edge → cloud environments. This release focuses on core functionality, dual runtime support, and developer tooling.

**Highlights:**

- 🚀 Dual runtime support: Works on both standard library (Tokio) and embedded (Embassy)
- 🔒 Type-safe record system eliminates runtime string lookups
- 📦 Three buffer types cover most data patterns
- 🔌 MQTT connector works in both std and `no_std` environments
- 🤖 MCP server enables LLM-powered introspection
- ✅ 27+ core tests, comprehensive CI/CD, security auditing

**Get Started:**

```bash
cargo add aimdb-core aimdb-tokio-adapter
```

See [README.md](README.md) for quickstart guide and examples.

**Feedback Welcome:**
This is an early release. Please report issues, suggest features, or contribute at:
<https://github.com/aimdb-dev/aimdb>

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/aimdb-dev/aimdb/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v1.0.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
