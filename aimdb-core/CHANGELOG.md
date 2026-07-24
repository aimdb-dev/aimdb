# Changelog - aimdb-core

All notable changes to the `aimdb-core` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking) — Design 045: one protocol (AimX) for every transport

- **Wildcard / multi-record subscribe.** `Inbound::Subscribe` topics may carry
  MQTT-style wildcards (`#`, `*`): `AimxDispatch` matches the pattern against
  the registry once at subscribe time (the record set is builder-frozen),
  merges the matched records' update streams under the one subscription id,
  and emits one late-join `Snapshot` per matched record. The matcher moved in
  from the retired `aimdb-ws-protocol` crate as
  `session::topic_match::{topic_matches, is_wildcard}` (re-exported at the
  crate root).
- **Subscription streams carry the firing record.** `Session::subscribe` and
  `ClientHandle::subscribe` now yield `SubUpdate { topic: Option<Arc<str>>,
  data: Payload }` instead of bare `Payload`; `Outbound::Event` gains an
  optional `topic` and `Outbound::Snapshot` gains the routing `sub` (frames
  without them are unchanged on the wire). `Session::snapshot` became
  `Session::snapshots(topic) -> Vec<(String, Payload)>` (one per covered
  record).
- **`AimxCodec` learned the `subscribed` ack frame** (`{"t":"subscribed",
  "sub":S}`) for servers running `acks_subscribe:true` (the WebSocket
  connector); UDS/serial/TCP keep the implicit ack. A dedicated `AimxCodec`
  roundtrip suite now locks the frame set.
- **`ClientConfig::topic_routed_subs` removed** — it existed solely for the
  retired ws wire; all subscriptions are id-routed.
- **Shared query/list vocabulary.** New `remote::QueryRecord { topic, payload,
  ts }` is the canonical `record.query` result row (result shape
  `{records, total}`); `RecordMetadata` gains optional `schema_type` /
  `entity` fields (`entity` derived from the record key's final `.` segment).

### Added

- **Issue #196 — direct JSON bytes for type-erased remote reads.** The public
  `RemoteSerialize`, `JsonCodec<T>`, `JsonRecordAccess` and
  `JsonBufferReader` traits gain defaulted byte methods, preserving existing
  manual implementations; the blanket Serde codec overrides them with direct
  `to_vec`/`from_slice`. State `record.get` and subscription events now pass
  owned JSON bytes into the existing opaque `Payload`/`RawValue` AimX path
  without materializing an intermediate record `serde_json::Value`.
  `RecordValue::as_json`, value-based APIs, custom codecs, AimX v2, ring-drain
  fallback, write safety, lag/cancellation/backpressure behavior and
  `no_std + alloc` remain unchanged. Focused production-boundary benchmarks
  reduce allocation calls from 16 to 6 per reply/event; Criterion slope point
  estimates show a 1.55x–1.61x in-memory record-to-envelope speedup on the
  measured host.
- **Issue #177 — bounded into-slice serialization for outbound links.** New
  `OutboundConnectorBuilder::with_serializer_into` and the additive
  `SerializedReader::recv_into`/`SerializedPayload` SPI let a fused typed reader
  encode into caller-owned storage using the existing `SerializeError` contract.
  `pump_sink` allocates one bounded scratch buffer per route and reuses it across
  messages; an undersized buffer falls back to the existing owned serializer for
  that value. Existing serializers, third-party `SerializedReader`
  implementations, and `pump_client` keep their owned-`Vec` behavior. This
  removes the codec allocation when an into-slice encoder is installed;
  connector adapters may still copy the borrowed payload.
- **Scratch serializer reset hook.**
  `OutboundConnectorBuilder::clear_serializer_into` lets higher-level builder
  extensions replace a bounded serialization strategy with an owned-only one
  without retaining the previous route-local scratch callback.
- **`RecordRegistrar::signal_gauge(name, unit) -> SignalGaugeHandle`** (design 041 §3.2) — the core hook behind `aimdb-data-contracts`'s `Observable::observe()`. Feeding values via `SignalGaugeHandle::update(f64)` folds them into per-record `SignalStats` (last/min/max/mean), surfaced through `RecordMetadata::signal_stats` on `record.list`/`record.get`. Mirrors the `with_name` precedent: always callable, an inert no-op handle when the `observability` feature is off, so callers never `#[cfg]` on core's features. New public types: `SignalGaugeHandle` (always available), `SignalStats`/`SignalGauge`/`SignalStatsInfo` (feature `observability`).

### Fixed

- **AimX protocol doc rot cleaned up; `remote::PROTOCOL_VERSION` corrected to `"2.0"` and exported.** The `remote` module docs claimed "AimX v1" and linked a spec file that no longer exists; they now describe the v2 NDJSON tagged-frame wire and point at `crate::session::aimx` / `docs/design/remote-access-via-connectors.md`. The AimX dispatch's Welcome uses the constant instead of a hardcoded `"2.0"` (same bytes on the wire). The dead, never-exported v1 `Message` untagged envelope and its helpers were removed from `remote::protocol`. Also de-advertised Kafka/HTTP connector semantics from `ConnectorUrl` docs (the parser is scheme-agnostic; those connectors never existed) and updated the `connector` module docs from the removed `.link()` API to `.link_to()`/`.link_from()`.
- **`build()` reports a missing runtime alongside every other configuration error (issue #133 contract).** The missing-runtime check no longer short-circuits: it is collected as a `ConfigError` and returned in the one `DbError::InvalidConfiguration` with all other findings (previously the collected errors were silently dropped and only a `RuntimeError` surfaced). The error type for a runtime-less build changes accordingly from `DbError::RuntimeError` to `DbError::InvalidConfiguration`.

### Changed (breaking)

- **Design 037 W8 — zero-allocation consume path: `BufferReader` is now poll-based ([design doc](../docs/design/037-zero-alloc-consume-path.md)).** The object-erased async `recv` returned a `Pin<Box<dyn Future>>`, heap-allocating on every call — the last AimDB-added per-message allocation on the consume path. It is replaced by an object-safe poll method, restoring async ergonomics through a new handle:
  - **SPI break (adapter authors only):** `BufferReader<T>::recv(&mut self) -> Pin<Box<dyn Future>>` → `BufferReader<T>::poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>>`. `try_recv` is unchanged. Same break for the `remote-access` `JsonBufferReader`: `recv_json` → `poll_recv_json`. Object safety is preserved (poll, unlike `async fn`, is object-safe).
  - **New consumer handles:** `buffer::Reader<T>` (and `buffer::JsonReader` under `remote-access`) wrap the erased reader and expose an `async fn recv()` implemented once via `core::future::poll_fn` — `core`-only, `no_std`-clean, zero-allocation, no `unsafe`. `Consumer::subscribe`, `TypedRecord::subscribe`, and `AimDb::subscribe` now return `Reader<T>` instead of `Box<dyn BufferReader<T> + Send>`.
  - **Source-compatible for consumers:** `subscribe().recv().await` is unchanged at every call site; examples and aimdb-pro compile without edits. Holders of a concrete adapter reader wrap it once: `Reader::new(Box::new(reader))`.
  - **Connector SPI unchanged (BYOC-stable, design 039 §2):** `SerializedReader::recv` keeps its boxed `RecvSerializedFuture`; only the *inner* per-message box is eliminated. The remote-access JSON path drops both its boxes (`poll_recv_json` + `JsonReader`).
  - **Result:** **0 AimDB-added heap allocations per message** on the in-process consume path, enforced by the `aimdb-bench` B0 suite (1 → 0 allocs/msg across all three tokio buffer profiles). Tokio `broadcast`/`watch` expose no public poll API, so the reader round-trips its receiver through a single reused `tokio_util::sync::ReusableBoxFuture` — one allocation per subscriber lifetime, not per message — and the Mailbox replaces `Notify` with an explicit waker list beside the slot. Embassy drives embassy-sync's public poll methods directly (`Subscriber::poll_next_message`, `watch::Receiver::poll_changed`, `Channel::poll_receive`) — no future box and **no new `unsafe`**; `poll_next_message`/`poll_changed` were added to the vendored `embassy-sync` as small additive wrappers (upstream PR pending).

- **Design 036 W1 — data-plane de-`Any`: the per-message `Box<dyn Any>` is gone from the connector SPI ([design doc §W1](../docs/design/036-followup-refactoring.md)).** Both ends of every erased hop were typed — `T` is known in the registrar where routes are wired, and the connector spine only wants bytes — so the typed pipeline is now built inside closures at registration time (`finish()`) and the SPI exposes only the wire level. The full break inventory:
  - **Inbound:** new `IngestFn = Arc<dyn Fn(&RuntimeContext, &[u8]) -> Result<(), String>>` + `IngestFactoryFn` replace deserializer + producer: deserialize + produce in one typed closure, **synchronous** (`Producer::produce` is sync + infallible per design 029 — the per-message `Box::pin` disappears along with the `Box<dyn Any>`). Deleted: `ProducerTrait`/`produce_any`, `ProducerFactoryFn`, `DeserializerFn`/`ContextDeserializerFn`/`DeserializerKind`, `TypedRecord::create_producer_trait`. `InboundConnectorLink` is `{ url, config, ingest_factory, topic_resolver }` (factory non-optional — `finish()` validates the deserializer before registering, error strings unchanged); `collect_inbound_routes` returns `Vec<(String, IngestFn)>`; `Route` is `{ resource_id, ingest }`.
  - **`Router::route` is a sync fn and its context is mandatory:** `route(&self, resource_id, payload, ctx: &RuntimeContext) -> Result<(), String>` (was `async` with `Option<&RuntimeContext>`). Every production caller already passed `Some(&ctx)`; the old "skip context-deserializers when no ctx" branch is unrepresentable now that raw-vs-context is invisible inside the fused closure.
  - **Outbound:** new `SerializedSource` (object-safe `subscribe()`, sync — the buffer handle is pre-resolved) → `SerializedReader` whose `recv(&mut self, ctx: &RuntimeContext)` yields `SerializedValue { dest, payload }` — subscribe → recv → resolve topic → serialize all typed inside, including the third erasure crossing the old path had: `topic_any(&dyn Any)` per value. Deleted: `ConsumerTrait`/`subscribe_any`, `AnyReader`/`recv_any`, `ConsumerFactoryFn`, `SerializerFn`/`ContextSerializerFn`/`SerializerKind`, `TopicProviderAny`/`TopicProviderWrapper`/`TopicProviderFn` (the typed `TopicProvider<T>` is stored as `Arc<dyn TopicProvider<T>>` directly). `ConnectorLink` is `{ url, config, source_factory }` (non-optional, same validation contract); `OutboundRoute` is `{ topic, source, config }` and the "skip links without serializer" branch in `collect_outbound_routes` is gone.
  - **Error contract of the fused reader:** buffer errors propagate unchanged (`BufferLagged` → pumps skip the gap; anything else → publisher stops, identical control flow). Serialization failures are logged and skipped *inside* the reader — observably the same as the old pump-side `continue`, but the log line moves (now `outbound link: failed to serialize <type> (dest …)` instead of `pump_sink: failed to serialize …`).
  - **Unrepresentable error surface deleted:** `SerializeError::TypeMismatch` (both constructors died with the downcasts; `DbError::TypeMismatch` is unrelated and stays). Dead code deleted: `ConnectorClient` (held `Arc<dyn Any>`, zero users) and `OutboundConnectorLink`.
  - **Source-compatible:** the registrar API (`with_serializer`/`with_serializer_raw`/`with_deserializer`/`with_deserializer_raw`/`with_topic_provider`/`with_topic_resolver`, `link_to`/`link_from`) keeps identical signatures — examples, codegen output, and aimdb-pro compile unchanged. The `RuntimeContext` is threaded into `recv`/ingest per call (not captured) for the design-026 context (de)serializers; raw variants skip the per-message ctx clone.
  - **Deliberate exception:** `JoinTrigger` keeps its `Box<dyn Any + Send>` — a join fans N differently-typed inputs into one channel and user closures branch via `as_input::<T>()`; the erasure is the public API there (documented on the type). Remaining `dyn Any` in core after W1: `ExtensionMap` (TypeId-keyed), `AnyRecord::as_any`/`as_any_mut` + `DynBuffer::as_any` (setup-time), and join.

- **Phase 3 — `R` removed from the object graph (Issue #131, [design doc §3.2/§3.3](../docs/design/034-technical-debt-review.md)).** The runtime travels as `Arc<dyn aimdb_executor::RuntimeOps>`; records (`T`) are the only generic surface left. `AimDb`, `AimDbBuilder` (no `NoRuntime` typestate), `TypedRecord<T>`, `RecordRegistrar<'a, T>`, `TransformBuilder<I, O>`, `JoinBuilder<O>`, `RecordT`, and `ConnectorBuilder` are all non-generic over the runtime; `RuntimeContext` is a concrete struct (`time().now()` → `u64` nanos, `sleep(core::time::Duration)` + `sleep_millis`/`sleep_secs`; `millis`/`secs`/`micros`/`duration_since`/`duration_as_nanos`/`extract_from_any` deleted). `source`/`tap`/`transform`/`transform_join` are inherent registrar methods (the `*_raw` variants and `ext_macros.rs` are deleted); connector consumer/producer factories take `&AimDb` (the `Arc<dyn Any>` downcast-or-panic dance is gone); context (de)serializers and `Router::route` receive the concrete `RuntimeContext`; `runtime_arc()` → `runtime_ops()` (+ new `runtime_ctx()`), `runtime_any()` and the borrowed `runtime()` accessor deleted (zero callers; `&*db.runtime_ops()` covers the borrowed flavor — [follow-up doc §2.5](../docs/design/035-review-followups-deferred.md)); `on_start` closures receive `RuntimeContext`; the `RuntimeForProfiling` marker is deleted (profiling clocks ride `RuntimeOps::now_nanos`); the session client engine clock is `Arc<dyn RuntimeOps>`. Multi-input join fan-in is one bounded `async-channel` queue in core (capacity 64 on `std` and wasm32 — matching the old tokio/WASM queues — and 16 on embedded `no_std`, up from Embassy's 8). **Close semantics changed on Embassy:** the queue now closes on *all* runtimes once every input forwarder exits, so a `no_std` join handler's `while let Ok(_) = rx.recv().await` loop ends instead of parking forever — treat `Err(QueueClosed)` as end-of-inputs. Input forwarders skip `BufferLagged` (SPMC-ring overflow) and keep forwarding, the same recoverable-lag policy as every other recv loop in core. The `JoinFanInRuntime` GAT family is gone from `aimdb-executor`.

- **Generic runtime trait re-exports removed from the crate root (Issue #131 follow-up).** `aimdb_core::{RuntimeAdapter, Runtime, TimeOps, Logger, RuntimeInfo}` are gone — core no longer consumes the generic family (the runtime travels as `Arc<dyn aimdb_executor::RuntimeOps>`), and keeping the re-exports invited `R:`-bounds back into downstream signatures. Import them from `aimdb_executor` directly where an adapter still implements them; `ExecutorError`/`ExecutorResult` stay re-exported.

### Changed

- **Session client keepalive is deadline-based.** Activity records a timestamp (one dyn clock read) and the boxed `RuntimeOps::sleep` future stays armed for a full idle window — re-created about once per keepalive interval instead of once per processed frame/command (each re-arm previously heap-allocated through the `dyn RuntimeOps` boundary). Wire behavior unchanged: a Ping still goes out once the link has been idle for `keepalive_interval` ms.
- **`RecordRegistrar::source` closure bound relaxed: `Send + Sync` → `Send`.** The `FnOnce` is taken out of its slot exactly once, so `Sync` bought nothing (it was an artifact of the deleted `ext_macros.rs`); source closures may now capture `!Sync` state (e.g. `Cell`-based sensor state). `tap` already required only `Send`.

### Added

- **`connector-session` feature + `session` module — the shared, runtime-neutral session substrate (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** A new `crate::session` module (gated `connector-session`, `no_std + alloc`, also enabled transitively by `std`) carrying the connector-convergence machinery:
  - **Substrate traits** — `Connection`/`Listener`/`Dialer` (Layer 1 transport, framing-in-transport), `EnvelopeCodec` (Layer 2, symmetric: `decode`/`encode` + the client-direction `encode_inbound`/`decode_outbound`), and `Dispatch` (shared, `Send + Sync`) + `Session` (per-connection, `&mut`-threaded) for Layer 3. Plus the role-neutral `Inbound`/`Outbound` message set, `Payload = Arc<[u8]>` (raw bytes — one serde pass on hot paths), `PeerInfo`/`SessionCtx` (type-erased auth `ext` slots), `Source`, and the `SessionLimits`/`Transport`/`Codec`/`Rpc`/`Auth` error enums. All `dyn`-safe on `std` and `no_std + alloc`.
  - **Server engine** — `serve` (accept loop, honors `SessionLimits::max_connections`) + `run_session` (per-connection biased `select_biased!` loop: RPC + streaming subscriptions funneled through a bounded per-connection event channel + fire-and-forget writes). Spawn-free (one `FuturesUnordered` per engine future); honors `SessionLimits::max_subs_per_connection`, with subscriptions reaped — and their cap slot freed — when a stream ends or on Unsubscribe. `SessionConfig` knobs: `reads_hello`, `acks_subscribe`.
  - **Client engine** — `run_client` returns a cheap-clone `ClientHandle` (`call`/`subscribe`/`write`) + the engine future; demuxes replies by `id`, supports id- or topic-routed subscriptions, exponential reconnect backoff, idle keepalive, and a bounded offline command queue (`ClientConfig`). The only runtime dependency is the adapter's `TimeOps` clock; everything else is `futures` channels + `async-channel`.
  - **Data-plane toolkit** — `pump_sink` (outbound: consume-serialize-publish via `Connector`) and `pump_source` (inbound: one multiplexed `Source` reader → `Router` fan-out), extracting the boilerplate every data-plane connector hand-rolled.
  - **Generic connectors** — `SessionClientConnector<D, C>` and `SessionServerConnector<C, LF, DF>` wrap the engines onto the `ConnectorBuilder` spine so a transport crate contributes only its `Dialer`/`Listener`/`Connection` triple under a configurable scheme (default `"remote"`).
- **`session::aimx` — the AimX-v2 protocol substrate (gated `connector-session` + `json-serialize`).** `AimxCodec`, the symmetric NDJSON `EnvelopeCodec` (`no_std + alloc`; splices an already-serialized record-value `Payload` into the JSON envelope verbatim via `serde_json`'s `RawValue`, no re-escaping). `AimxDispatch`/`AimxSession` (`no_std + alloc` as of Issue #120 — gated `connector-session` + `remote-access`; it reaches into core's `record.list`/JSON API, de-std'd to match), porting the method semantics (`hello`, `record.{list,get,set,drain,query}`, `graph.*`, `*.reset`) off the deleted hand-rolled handler onto `Session`. The drain cursors live in the per-connection session; the AimX subscribe ack stays implicit (events carry the request id back).
- **`remote-access` feature — the AimX server data model, now `no_std + alloc` (Issue #120, follow-up to #39).** New feature `remote-access = ["json-serialize", "thiserror"]` that lifts the AimX *server* path off the `std` gate so a board can **serve** a host (e.g. over serial), not just dial one. It re-gates, from `std` to `remote-access`: the `crate::remote` module (record metadata + introspection, the wire protocol/config/security/error types), the type-erased `AnyRecord` JSON + metadata methods (`collect_metadata` / `latest_json` / `subscribe_json` / `set_from_json`), the `JsonBufferReader` trait, and the `AimDb`/`AimDbInner` JSON read/write/subscribe API (`list_records` / `try_latest_as_json` / `set_record_from_json`). The server dispatch swaps `std::collections::HashMap`/`HashSet` → `hashbrown` and `std::sync::Arc` → `alloc::sync::Arc`; `thiserror` is pulled with `default-features = false` so `RemoteError` builds on `no_std`, and `From<std::io::Error> for RemoteError` is kept behind a `std` cfg. `std` enables `remote-access` transitively, so std builds are unaffected. Verified by `cargo check -p aimdb-core --no-default-features --features "alloc,connector-session,remote-access" --target thumbv7em-none-eabihf` (added to the Makefile `test-embedded` target alongside `alloc,remote-access` test/clippy lines).
- **`AimDb::runtime_arc(&self) -> Arc<R>`.** An owned runtime-adapter handle for connectors that hand the runtime to a `'static` engine future (the session client engine clones it for its `TimeOps` clock).
- **`ConnectorConfig::from_query(&[(String, String)])`.** Builds a per-route `ConnectorConfig` from a link URL's query pairs (`timeout_ms` lifted to the typed field, everything else passed through in `protocol_options`); the seam `pump_sink` uses to thread per-route config to `Connector::publish`.
- **`json-serialize` feature + `codec` module (M16, Design 032).** New `crate::codec` module with `RemoteSerialize` (capability trait, blanket-impl'd for every `serde` `Serialize + DeserializeOwned` type), the object-safe `JsonCodec<T>` storage trait, and the zero-sized `SerdeJsonCodec`. All three are re-exported from the crate root. The feature is `no_std + alloc` compatible (`serde_json` runs on `alloc`), so `RecordValue::as_json()` now works on embedded targets, not just `std`. `std` enables `json-serialize` transitively, so existing std builds are unaffected.
- **`DynBuffer::peek(&self) -> Option<T>` (M15, Design 031).** Non-destructive, buffer-native point-in-time read; the default impl returns `None` (correct for buffers with no canonical latest, e.g. broadcast/SPMC rings). AimX `record.get` and `TypedRecord::latest()` now route through it. Adapters implement it per buffer type — see the tokio/embassy adapter changelogs.
- **`impl Dialer for Box<dyn Dialer>` (Issue #123).** A boxed dialer is itself a `Dialer`, so a runtime-selected `Box<dyn Dialer>` (e.g. from a `scheme://` URL resolver) can be handed straight to `run_client<D: Dialer>` without a generic transport at the call site.

### Internal refactors

- **Phase 2 — builder internals de-erased (Issue #130, [design doc §3.2](../docs/design/034-technical-debt-review.md)).** No behavior change: `AimDbBuilder` stores its per-record future collectors and `on_start` tasks as their typed forms (`Vec<(StringKey, SpawnFnType<R>)>` / `Vec<StartFnType<R>>`) instead of `Box<dyn Any + Send>` — the builder is already generic over `R`, so the erasure bought nothing. The panicking downcasts in `build()` (`"spawn function type mismatch"`, `"on_start fn type mismatch"`) are deleted. To keep the field types well-formed on the `NoRuntime` typestate, `AimDb<R>`'s struct-level `R: RuntimeAdapter + 'static` bound moved to its impl blocks (strictly more permissive). `BoxFuture` is now a re-export of the canonical alias in `aimdb-executor` (same type).

- **Phase 1 mechanical cleanup (Issue #132, [design doc](../docs/design/034-technical-debt-review.md)).** No behavior change:
  - All dual `#[cfg(feature = "std")] use std::… / #[cfg(not(…))] use alloc::…` import pairs replaced by single unconditional `use alloc::…` imports; redundant per-module `extern crate alloc;` declarations dropped (the crate root has one). The duplicated std/no_std `RuntimeContext` impl blocks in `context.rs` (character-identical except for the `Arc` path) are merged into one.
  - New crate-private `log_debug!`/`log_info!`/`log_warn!`/`log_error!` macros (`src/log.rs`) forward to `tracing` when the feature is on and expand to an argument-borrowing no-op otherwise — deleting all 62 per-call-site `#[cfg(feature = "tracing")]` gates. `defmt` is deliberately not folded in (most sites use `{:?}` with non-`defmt::Format` types); router.rs keeps its paired explicit defmt gates.
  - `build()` resolves each topo-order key with one O(1) `by_key.get_key_value(&str)` lookup (via `StringKey: Borrow<str>`) instead of an O(n) `keys().find()` scan plus a redundant second resolve.

- **`RecordMetadataTracker` deleted; `TypedRecord` keeps only a bare `writable` flag (Issue #120).** The per-record tracker (an `Arc<Mutex<Option<timestamp>>>` + `Arc<AtomicBool>` + `SystemTime::now()` on every `produce()` via `RecordWriter::push`) is gone; the surviving `writable` bit is now a single `portable_atomic::AtomicBool` field on `TypedRecord`, and `collect_metadata` reads the type name + `writable` directly with no shared state. New `pub(crate)` `DbError::{runtime_error, permission_denied, record_key_not_found}` constructors let the JSON/remote paths write one error expression across `std` (message carried) and `no_std` (unit placeholder) — replacing the inline `#[cfg]` splits at each call site. The `graph` types' `serde` derives (`RecordOrigin` / `GraphNode` / `GraphEdge` / `EdgeType`) move from the `std` gate to the `serde` feature (always on with `alloc`), so `RecordMetadata` can serialize on `no_std`.

- **AimX server/client ported onto the shared session engine; the hand-rolled loops deleted (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** Building on the spawn-free work below, `remote/handler.rs` (the per-connection `select!` loop) and `remote/supervisor.rs` (the accept loop) are **removed** — their behavior is now `run_session` + `serve` in `session`, driven by the AimX-v2 `AimxDispatch`/`AimxCodec`. `remote/stream.rs`'s `stream_record_updates` survives and is reused by `AimxSession::subscribe`. The UDS transport (socket bind/connect, NDJSON framing) relocated out of core into the new `aimdb-uds-connector` crate; core keeps only the protocol (codec + dispatch) and the generic session connectors. The query handler type-erasure moved to `remote/query.rs` (`QueryHandlerFn`/`QueryHandlerParams`). New dependencies: `async-channel` (runtime-neutral mpsc), `futures-channel` (oneshot), `futures-util`'s `async-await-macro` (`select_biased!`), and `serde_json`'s `raw_value` feature — all `no_std + alloc`-compatible, none entering the no_std contracts build.
- **AimX remote-access path is now spawn-free (Issue #114, Design 030).** Every remaining `tokio::spawn` in `aimdb-core/src/remote/` was removed; the supervisor's accept loop and each connection handler now own their own `FuturesUnordered<BoxFuture>` driven by `tokio::select! { biased; }`. Cancellation collapsed to one mechanism — dropping the future.
  - New `aimdb-core/src/remote/stream.rs` exports a `pub(crate) stream_record_updates` helper that adapts a record's `JsonBufferReader` into a `Stream<Item = serde_json::Value>` via `futures_util::stream::unfold`. No task, no channel — drop the stream to cancel.
  - `AimDb::subscribe_record_updates` **deleted**. The method had no out-of-tree callers (the only caller was the AimX handler); replaced by `stream_record_updates` above.
  - Per-subscription `oneshot::Sender<()>` cancel channels and the `SubscriptionHandle` struct **deleted**. `ConnectionState::subscriptions` is now `HashMap<String, Arc<tokio::sync::Notify>>`; `record.unsubscribe` calls `notify_one()`, waking the per-sub future immediately (even when parked on `stream.next()`).
  - The two-task chain per subscription (buffer-reader task + JSON-event forwarder task) **collapsed** into one `run_subscription` future per subscription, held in the connection's `FuturesUnordered`.

### Changed (breaking)

- **Phase 2 — MQTT knobs deleted from the generic link builders; `ConnectorConfig` pruned (Issue #134, [design doc §3.6](../docs/design/034-technical-debt-review.md)).** `OutboundConnectorBuilder::with_qos`/`with_retain` and `InboundConnectorBuilder::with_qos` are gone — use `aimdb-mqtt-connector`'s `MqttLinkExt`/`MqttOutboundLinkExt` (same option keys, wire-identical) or the generic `with_config(key, value)`. `with_timeout_ms` survives with protocol-neutral docs. `ConnectorConfig` drops its typed `qos: u8`/`retain: bool` fields (verified unread by every connector — MQTT reads `protocol_options`) and the Kafka/HTTP/shmem interpretation docs; it keeps `timeout_ms` + `protocol_options`. `OutboundConnectorBuilder`/`InboundConnectorBuilder` are now re-exported from the crate root (for the extension-trait impls).

- **Phase 2 — panic-free builder validation; `build()` collects every configuration mistake (Issue #133, [design doc §3.4](../docs/design/034-technical-debt-review.md)).** One failure model instead of two: builder methods stay infallible and never panic on user mistakes; `build()` performs all validation and returns one `DbError::InvalidConfiguration { errors: Vec<ConfigError> }` (new variant + new public `ConfigError { record_key, url, message }` type, error code `0x4003`) carrying **every** finding from the run. Specifics:
  - `TypedRecord::set_producer`/`set_transform`/`add_inbound_connector` mutual-exclusion violations and duplicate producers/transforms: recorded, conflicting registration skipped (observable: `has_producer()` stays `false` after a conflicting `.source()`).
  - `OutboundConnectorBuilder::finish()`/`InboundConnectorBuilder::finish()`: invalid URL, missing serializer/deserializer, unregistered scheme, and the transform/source conflicts are recorded with record key + URL; the link is not registered and the registrar is returned as usual.
  - The spawn-time factory panics (missing buffer, record lookup) are now build()-time checks: any record with an outbound or inbound link and no buffer fails `build()` with key + link URL. Consequence: `.buffer()` may be called after `.link_to()`/`.link_from()` — the finish()-time buffer check is gone.
  - `configure()` with a key already registered under a different type: recorded, the closure is skipped (previously `assert!`).
  - Duplicate record keys and `DependencyGraph` findings (cycles, unregistered transform inputs) fold into the same `InvalidConfiguration` report — callers matching on `DuplicateRecordKey`/`CyclicDependency` from `build()` must update.
  - Surviving `panic!`/`expect`s on the builder path are internal invariants only, worded "this is a bug in aimdb-core". `AnyRecord` gains `has_buffer()` and `drain_config_errors()` (breaking for external implementors).

- **Phase 2 — `RecordRegistrar` fluent methods take fresh borrows (Issue #130, [design doc §3.5](../docs/design/034-technical-debt-review.md)).** The methods previously took `&'a mut self` and returned `&'a mut Self` with `'a` = the struct's own lifetime parameter, so the first call borrowed the registrar for its entire remaining lifetime and only one unbroken chain per `configure` closure was possible. Now:
  - All fluent methods are `&mut self -> &mut Self`; separate statements (`reg.source_raw(…); reg.tap_raw(…);`) and registrar reuse after a chain work.
  - `configure`'s closure bound is `FnOnce(&mut RecordRegistrar<'_, T, R>)` (HRTB dropped) — existing closures compile unchanged.
  - `OutboundConnectorBuilder` / `InboundConnectorBuilder` are now `<'r, 'a, T, R>` (`'r` borrows the registrar, `'a` is the registrar's record borrow) instead of the double-`'a`; `finish()` returns `&'r mut RecordRegistrar<'a, T, R>`, so `.finish().link_from(…)` chains keep working.
  - `RecordT::register` is `fn register(reg: &mut RecordRegistrar<'_, Self, R>, cfg: &Self::Config)` (was `fn register<'a>(reg: &'a mut RecordRegistrar<'a, …>)`); the `impl_record_registrar_ext!` macro and the adapter/persistence extension traits follow the same shape. Code that *names* the old signatures (custom `RecordT` impls, custom extension traits) must update; closure-based configuration is source-compatible.

- **`DbError` unified on `alloc::string::String` — one shape per variant on every target (Issue #129, [design doc §3.1.1](../docs/design/034-technical-debt-review.md)).** The std/no_std dual-field design (`key: String` under `std`, `_key: ()` otherwise) is gone: every context field is a `String` unconditionally (the crate already requires `alloc` everywhere), and `Display`/`Error` derive from `thiserror` on every target (thiserror 2.x is no_std-capable, now a mandatory `default-features = false` dependency; the `remote-access` feature no longer lists it). Consequences:
  - **no_std only:** field renames (`_endpoint`/`_reason`/`_buffer_name`/`_key`/… `: ()` → the real `String` fields); `Display` now prints the same full messages as std builds instead of the `Error 0xNNNN` numeric-code table (which is deleted); `DbError` now implements `core::error::Error`. `error_code()` / `error_category()` are unchanged.
  - **std:** no change to variant shapes or messages.
  - The helper constructors `DbError::runtime_error` / `permission_denied` / `record_key_not_found` are now **public**, un-gated from `remote-access`, single-bodied (the message is carried on every target), and joined by a new `DbError::missing_configuration`. Every dual `#[cfg]` construction branch in core, the adapters, and the MQTT/KNX connectors is collapsed.
- **`ConsumerTrait::subscribe_any` is infallible (Issue #132).** The future now resolves to `Box<dyn AnyReader>` instead of `DbResult<Box<dyn AnyReader>>` — subscription has been infallible since M14 (pre-resolved buffer handle); the `Result` was kept only for caller compatibility. Implementors drop the `Ok(...)` wrap; callers drop the dead `Err` arm.
- **`OutboundRoute` is a struct (Issue #132).** Was a 5-tuple type alias destructured positionally; now named fields: `topic`, `consumer`, `serializer`, `config`, `topic_provider`.
- **The `std` feature no longer enables a `tokio` dependency (Issue #132).** Since the session-engine refactor (030/033) the only `tokio::` references in `aimdb-core/src` are in `#[cfg(test)]` modules and doc comments — covered by the dev-dependency. Core no longer pulls tokio (`net`, `io-util`, `sync`, `time`) into every std build; depend on tokio directly if you relied on the transitive dependency.

- **`RecordMetadata` drops `created_at` / `last_update`; `AimxConfig.socket_path` is now `String` (Issue #120).** Part of de-std'ing the remote data model for the `no_std` AimX server. Core no longer keeps per-record timestamp state — the `std::sync::Mutex`-+-`SystemTime`-backed `RecordMetadataTracker` is deleted — so `RecordMetadata` loses the `created_at` and `last_update` fields, `RecordMetadata::new` loses its `created_at` parameter, and `with_last_update` / `with_last_update_opt` are removed. AimX `record.list` (and downstream MCP / CLI output derived from it) therefore no longer carry those two fields; every other metadata field is unchanged. `AimxConfig.socket_path` becomes `String` (was `PathBuf`) and `AimxConfig::socket_path(impl Into<String>)` replaces `impl Into<PathBuf>` — `&str` / `String` callers are unaffected, `&Path` / `PathBuf` callers must convert (the UDS transport in `aimdb-uds-connector` does the `Path`-based bind). `SecurityPolicy`'s writable set and the `remote` module's collections move from `std::collections` to `hashbrown`.

- **`AimDbBuilder::with_remote_access(config)` removed (Issue #39).** Register a session connector instead: `.with_connector(aimdb_uds_connector::UdsServer::from_config(config))`. The connector binds its transport at `build` time (bind errors surface synchronously, as before), applies the security policy's writable-record marking, and drives the shared `serve` engine. The builder's private `remote_config` field is gone; the per-record `TypedRecord::with_remote_access()` is unrelated and unchanged. The reshaped **AimX-v2** wire is not backward-compatible with the legacy v1 framing.
- **`latest_snapshot` removed from `TypedRecord`; `latest()` / AimX `record.get` read the buffer via `peek()` (M15, Design 031).** Eliminates one snapshot-mutex lock + `Option<T>` clone per `produce()` on the hot path. Behavioural consequences:
  - A record configured with `.with_remote_access()` but **no buffer** now fails `build()` with a clear error (previously a silent runtime no-op — reads returned `not_found`, writes were discarded). Add a buffer, e.g. `.buffer(BufferCfg::SingleLatest)`.
  - `record.get` / `latest()` on an `SpmcRing` record now returns `not_found` / `None` — a ring keeps per-consumer history with no canonical latest. Use `record.drain` (history) or `record.subscribe` (live). `SingleLatest` and `Mailbox` are unaffected.
  - On `no_std`/embedded, `latest()` now depends on the adapter implementing `peek()` (the Embassy adapter does — see its changelog).
- **`with_remote_access()` is now gated on `json-serialize` and bounded on `T: codec::RemoteSerialize` (M16, Design 032).** Same effective bound as before (`Serialize + DeserializeOwned`, blanket-impl'd), but the stored serializer/deserializer closures are replaced by a single type-erased `Arc<dyn JsonCodec<T>>`. `std` enables `json-serialize`, so std callers see no change; `no_std + alloc` callers must enable the `json-serialize` feature to call it.
- **`producer_service` renamed to `producer` (M15).** `TypedRecord::set_producer_service` → `set_producer`, and `has_producer_service` → `has_producer` (the latter also on the `AnyRecord` trait). Affects code that called these methods directly; the public `.source()` registrar API is unchanged. Also collapses the std/no_std `cfg` split on `AnyRecord::buffer_info` / `transform_input_keys` into single signatures.
- **`AimxConfig` lost `subscription_queue_size` (Issue #114, Design 030).** The field bounded a per-subscription mpsc channel that no longer exists — subscriptions are now one future in a `FuturesUnordered`. The builder method `.subscription_queue_size(n)` is removed; replace it with `.max_subs_per_connection(n)` if you were using the value as a soft cap on subscription count, or just delete the call.
- **AimX `Welcome.max_subscriptions` now reports the real per-connection cap.** Previously it returned `subscription_queue_size` (default 100) while the actual cap was implicit; it now returns `max_subs_per_connection` (default 32). Clients that displayed this value will see the change.
- **AimX `record.subscribe` response no longer carries `queue_size`.** Result object is now `{ "subscription_id": "..." }` — the previous `"queue_size"` reported a number that no longer corresponded to anything in the implementation.
- **`AimxConfig` gains `max_subs_per_connection: usize` (default 32)** — the dedicated per-connection subscription cap. The existing `max_connections: usize` (previously declared but unread) is now actually enforced by the supervisor; over-cap connections are refused by closing the accepted `UnixStream` pre-handshake.

- **`Producer::produce` is now sync + infallible; `Consumer::subscribe` is now infallible (Design 029 follow-up, M14).** The pre-resolved `WriteHandle::push` cannot fail and the pre-resolved buffer Arc makes `subscribe()` infallible. Call sites collapse: `producer.produce(x).await?` → `producer.produce(x);` and `let Ok(reader) = consumer.subscribe() else { ... }` → `let reader = consumer.subscribe();`. The `ProducerTrait::produce_any` / `ConsumerTrait::subscribe_any` trait surfaces stay `Result`/`async` because the type-erasure downcast remains fallible.
  - `AimDb::produce<T>(key, value) -> DbResult<()>` is now sync; `.await` on the call site goes away. Only the key lookup can fail.
  - `Database::produce` likewise sync.
  - `TypedRecord::produce` was made sync here (was `pub async fn produce`), then **removed entirely in M15** — see _Removed (breaking)_ below.
  - `aimdb-wasm-adapter`: `bindings::poll_sync` helper deleted — no remaining callers now that `TypedRecord::produce` is sync.
  - Dead `consumer.subscribe()` error arms in `transform/single.rs` and `transform/join.rs` removed (the `Err` branch was unreachable after M14).

- **`Producer<T>` / `Consumer<T>` drop the runtime parameter `R` and pre-resolve the record at build time (Design 029, M14).** Producer/Consumer become handles to a buffer rather than tickets to look one up: `produce()` is one virtual call (no `HashMap<key>` probe, no `TypeId` check, no downcast), and `subscribe()` collapses to `buffer.subscribe_boxed()`. The internal mechanic is a new crate-private `WriteHandle<T>` trait backed by `RecordWriter<T>` (in `aimdb-core/src/buffer/writer.rs`), pre-bound to the record's `Arc<dyn DynBuffer<T>>` + snapshot mutex + metadata tracker.
  - `Producer<T, R>` → `Producer<T>`; `Consumer<T, R>` → `Consumer<T>`. User code that names the two-parameter form must drop the trailing adapter arg.
  - `Producer::key(&self) -> &str` is **removed**. Capture the record key at the registration site instead.
  - `Producer::produce(value) -> ()` and `Consumer::subscribe() -> Box<dyn BufferReader<T> + Send>` (v0.4 revision — see the sync/infallible bullet above for the rationale and migration). The `ProducerTrait::produce_any` / `ConsumerTrait::subscribe_any` trait surfaces retain `async`/`Result` for the type-erased downcast that can still fail.
  - `AimDb::producer<T>(key)` / `AimDb::consumer<T>(key)` now return `DbResult<…>` (was infallible). They resolve the typed record up front, so callers that previously assumed inference must add `?`.
  - `Consumer<T>` cannot exist without a buffer: `.tap()` on a record with no `.buffer(...)` now surfaces as `MissingConfiguration` at build time (was a deferred subscribe-time error).
  - `TypedRecord::buffer` field is `Option<Arc<dyn DynBuffer<T>>>` (was `Box`); `TypedRecord::set_buffer(Box<…>)` keeps its public signature and converts via `Arc::from(box_)` internally.
  - `TypedRecord::create_producer_trait(&self)` no longer takes `db` / `record_key` — it uses the new `writer_handle()`.
  - `ConnectorBuilder<R>` cascade is zero-LOC: no connector struct carried `R` after M13. The outbound `consumer_factory` / inbound `producer_factory` callbacks now resolve the record once at link-startup time (via `db.inner().get_typed_record_by_key`) and construct the new handles.
  - Codegen-emitted task scaffolds use `Producer<T>` / `Consumer<T>` (no `, TokioAdapter`).
  - `data-contracts` `log_tap` parameter is `Consumer<T>`.

- **`Spawn` trait removed; `AimDbBuilder::build()` now returns `(AimDb<R>, AimDbRunner)` (Issue #88, Design 028).** Every future the database needs — `.source()`/`.tap()`/`.transform()` tasks, on_start hooks, connector loops, the remote-access supervisor — is collected at build time into the new `AimDbRunner`, then driven by a single `FuturesUnordered` from `runner.run().await`. No background work runs until the runner is polled.
  - `AimDb::spawn_task` is **deleted**. Migrate to `on_start()` (collected at build) or to a private `FuturesUnordered` inside your own future.
  - The `Runtime` bundle no longer supertrait-requires `Spawn`. Custom adapters drop `impl Spawn`.
  - `R: Spawn` bounds are gone everywhere in `aimdb-core` (`Producer`, `Consumer`, `TypedRecord`, `TransformDescriptor`, `RecordRegistrar`, `RecordT`, `AnyRecordExt::as_typed`, remote handler/supervisor, `Database<A>`) — replaced by `R: RuntimeAdapter`.
  - `RecordSpawner<T>` renamed to `RecordFutureCollector<T>`; its `spawn_all_tasks` → `collect_all_futures`. Internal `spawn_consumer_tasks`/`spawn_producer_service`/`spawn_transform_task` on `TypedRecord` become `collect_consumer_futures`/`collect_producer_future`/`collect_transform_futures`.
  - Join transforms now hoist their per-input forwarder construction to build time — `JoinPipeline::into_descriptor()` returns a `CollectedTransform { task_future, fanin_futures }` and the lazy `runtime.spawn(forwarder)` inside `run_join_transform` is gone.
  - `ConnectorBuilder::build()` now returns `Vec<BoxFuture<'static, ()>>` instead of `Arc<dyn Connector>` (which `AimDbBuilder` already discarded).
  - Unsafe `impl Send/Sync` blocks on `Producer<T, R>` / `Consumer<T, R>` deleted — they auto-derive now.
  - On the AimX remote-access path, three `runtime.spawn(...)` call sites were temporarily bridged to bare `tokio::spawn` under `#[cfg(feature = "std")]`. These have since been removed by the AimX spawn-free follow-up — see the "AimX remote-access path is now spawn-free" entry above.
- `on_start` no_std bifurcation collapsed: a single `StartFnType<R>` alias replaces the byte-identical std/no_std pair.

### Removed (breaking)

- **`Database<A>` wrapper removed (Issue #132, design doc §3.8).** The thin façade over `AimDb` (and its `pub use database::Database` re-export) had no users beyond the `TokioDatabase`/`EmbassyDatabase` adapter aliases, themselves used nowhere — both aliases are removed from the adapters in the same change. Use `AimDb<R>` via `AimDbBuilder` directly.
- **Deprecated `RecordRegistrar::link()` removed (Issue #132).** Deprecated since 0.2.0; use `.link_to()` / `.link_from()`.

- **`TypedRecord::produce` removed (M15, Design 031).** The M14 step (above) made it sync; M15 removes it entirely. All writes now go through `WriteHandle::push` via `TypedRecord::writer_handle()`. `AimDb::produce` and AimX `set_from_json` route through it; as a side effect `set_from_json` now marks record metadata as updated (previously skipped on that path). `WriteHandle` / `RecordWriter` no longer carry the snapshot mutex.
- **`with_read_only_serialization()` removed (M16, Design 032).** A `Serialize`-only record can no longer be exposed read-only over remote access. Use `with_remote_access()`, which additionally requires `DeserializeOwned`. No in-tree callers existed.

## [1.1.0] - 2026-05-22

### Added

- **Automatic stage profiling (Issue #58, RFC 014, feature `profiling`)**: AimDB now measures wall-clock time per `.source()`, `.tap()`, and `.link()` stage with no user instrumentation. Feature is off by default and adds zero overhead when disabled; `alloc` + a runtime clock is enough, so it works on `no_std + alloc` targets too.
  - New `profiling` module exporting `StageMetrics` (atomic `call_count` / `total_time_ns` / `avg_time_ns` / `min_time_ns` / `max_time_ns` counters), `RecordProfilingMetrics` per-record container, and serializable `StageProfilingInfo` snapshot.
  - Source-stage timing measures the interval between successive `Producer::produce()` calls via a new `ProducerProfilingState`. Tap- and link-stage timing wraps the `BufferReader` returned by `Consumer::subscribe()` in a new `ProfilingBufferReader` that times the interval between successive `recv()` yields. The whole-task closure shape of `.source()` / `.tap()` is preserved — no per-value handler changes.
  - `RecordRegistrar::with_name("...")` assigns a human-readable name to the most recently registered source/tap/link; surfaces in MCP output. Always callable — a no-op when the feature is disabled.
  - New `StageKind` enum (`Source` / `Tap` / `Link` / `Transform`); `.transform()` is stubbed for future instrumentation.
  - `RecordMetadata` gains an optional `stage_profiling: Vec<StageProfilingInfo>` field (feature-gated) attached automatically in `TypedRecord::collect_metadata`. New helper `RecordMetadata::with_stage_profiling`.
  - `AimDb::reset_stage_profiling()` clears every record's counters. New `profiling.reset` AimX RPC method (write-permission gated) wired through `remote::handler`.
  - New `RuntimeForProfiling` marker trait — blanket-implemented for every `R` when the feature is off, requires `aimdb_executor::TimeOps` when on. Surfaces on `AimDbBuilder::run` / `build` and `AimDb::build_with`. Public API is unchanged when the feature is disabled.
  - New `Time::duration_as_nanos` accessor on the context (delegates to `TimeOps`).
  - Dependency: `portable-atomic` (with the `fallback` + `critical-section` features enabled by the `profiling` feature) for 64-bit-atomic emulation on targets without native `AtomicU64` (e.g. `thumbv7em-none-eabihf`).
- **Writer-exclusivity validation for `.link_from()` (Issue #89)**: `.source()`, `.transform()`, and `.link_from()` are now mutually exclusive on a single record — combining any two now panics at configuration time instead of silently racing on the buffer (last-writer-wins). The check fires from `LinkFromBuilder::finish()` (panic message includes the offending URL), with symmetric defense-in-depth checks added to `TypedRecord::set_producer_service`, `set_transform`, and `add_inbound_connector`. Multiple `.link_from()` calls on the same record (fan-in) remain permitted.
- **`no_std` support for the full Transform API (Design 027)**: `.transform()` and `.transform_join()` are now available on `no_std + alloc` targets. Multi-input join fan-in is no longer hardcoded to `tokio::sync::mpsc`; it uses the runtime-agnostic `JoinFanInRuntime` traits from `aimdb-executor`, implemented by Tokio, Embassy, and WASM adapters.
- **`JoinEventRx`** — type-erased trigger receiver passed to the `on_triggers` handler. Call `.recv().await` in a loop to consume `JoinTrigger` events from all input forwarders.
- **`transform_join` as an inherent method on `RecordRegistrar`** (gated `feature = "alloc"`, `R: JoinFanInRuntime`). Previously only exposed via the `impl_record_registrar_ext!` macro under `feature = "std"`.
- **Context-Aware Deserializers (Design 026)**: Inbound connector deserializers can now receive a `RuntimeContext<R>` for platform-independent timestamps and logging during deserialization
  - New `ContextDeserializerFn` type alias for context-aware type-erased deserializer callbacks
  - New `DeserializerKind` enum (`Raw` / `Context`) to enforce mutual exclusivity between plain and context-aware deserializers
  - `.with_deserializer(|ctx, bytes| ...)` now accepts a context-aware closure receiving `RuntimeContext<R>`
  - `.with_deserializer_raw(|bytes| ...)` added for plain bytes-only deserialization (no context needed)
  - `Router::route()` now accepts an optional type-erased runtime context (`Option<&Arc<dyn Any + Send + Sync>>`)
  - Context deserializer routes are gracefully skipped when no context is provided
- **Context-Aware Serializers**: Outbound connector serializers can now receive a `RuntimeContext<R>`, symmetric with deserializers
  - New `ContextSerializerFn` type alias for context-aware type-erased serializer callbacks
  - New `SerializerKind` enum (`Raw` / `Context`) to enforce mutual exclusivity between plain and context-aware serializers
  - `.with_serializer(|ctx, value| ...)` now accepts a context-aware closure receiving `RuntimeContext<R>`
  - `.with_serializer_raw(|value| ...)` added for plain value-only serialization (no context needed)

### Changed

- **Breaking — Join handler API redesign (Design 027 §Q4)**: `JoinBuilder::with_state(...).on_trigger(Fn(...) -> Pin<Box<dyn Future>>)` replaced with task-model `JoinBuilder::on_triggers(FnOnce(JoinEventRx, Producer) -> impl Future)`. The handler now owns the event loop, eliminating per-event heap allocation and allowing state to be borrowed across `.await` points.
- **`transform.rs` split into `transform/{mod,single,join}.rs`** — internal reorganization to keep the `alloc`-only join path separate from the runtime-agnostic single-input path. `JoinBuilder`, `JoinPipeline`, `JoinTrigger`, `JoinEventRx` are now re-exported from `transform::join`.
- `transform_join_raw` now requires `R: JoinFanInRuntime` (was `feature = "std"`).
- `ExecutorError::QueueClosed` mapped to `DbError::RuntimeError` in `From<ExecutorError>`.
- **Breaking**: `InboundConnectorLink::deserializer` field type changed from `DeserializerFn` to `DeserializerKind`
- **Breaking**: `InboundConnectorLink::new()` now takes `DeserializerKind` instead of `DeserializerFn`
- **Breaking**: `Router::route()` signature changed to accept an additional `ctx` parameter
- **Breaking**: `RouterBuilder::from_routes()` and `RouterBuilder::add_route()` now take `DeserializerKind` instead of `DeserializerFn`
- **Breaking**: `ConnectorLink::serializer` field type changed from `Option<SerializerFn>` to `Option<SerializerKind>`
- **Breaking**: `.with_serializer()` renamed to `.with_serializer_raw()` — old single-argument pattern
- **Breaking**: `OutboundRoute` type alias updated to use `SerializerKind`
- **Breaking**: `.with_deserializer()` on `InboundConnectorBuilder` now expects `Fn(RuntimeContext<R>, &[u8]) -> Result<T, String>` instead of `Fn(&[u8]) -> Result<T, String>` — use `.with_deserializer_raw()` for the previous bytes-only signature
- `AimDb::collect_inbound_routes()` return type updated to use `DeserializerKind`

## [1.0.0] - 2026-03-11

### Added

- `ConnectorUrl::default_port()` now handles `ws://` (→ 1883) and `wss://` (→ 8883) URL schemes for WebSocket connectors
- `ConnectorUrl::is_secure()` now includes the `wss` scheme
- Documentation updated with WebSocket URL format examples

## [0.5.0] - 2026-02-21

### Added

- **Transform API (Design 020)**: Reactive data transformations between records
  - **Single-Input Transforms**: `transform_raw()` method on `RecordRegistrar` for creating reactive derivations
  - **Multi-Input Joins**: `transform_join_raw()` for combining multiple input records with stateful handlers
  - **`TransformBuilder`**: Fluent API with `.with_state()` and `.map()` for transform configuration
  - **`JoinBuilder`**: Multi-input builder with `.input::<T>(key)` and `.with_state().on_trigger()` pattern
  - **`TransformDescriptor`**: Type-erased descriptor for storing transform configuration
  - **`JoinTrigger`**: Event type for multi-input join handlers with index and type-erased value
  - Transforms are spawned as tasks during `AimDb::build()` and subscribe to input buffers
  - Mutual exclusion enforced: a record cannot have both `.source()` and `.transform()`
  - Full tracing integration for transform lifecycle events
- **Graph Introspection API (Design 021)**: Dependency graph visualization and introspection
  - **`RecordOrigin` enum**: Classifies record data sources (`Source`, `Link`, `Transform`, `TransformJoin`, `Passive`)
  - **`GraphNode` struct**: Node metadata including origin, buffer config, tap count, outbound status
  - **`GraphEdge` struct**: Directed edge with `from`, `to`, and edge type classification
  - **`DependencyGraph` struct**: Full graph with nodes, edges, and topological ordering
  - New `AnyRecord` trait methods: `has_transform()`, `record_origin()`, `buffer_info()`, `transform_input_keys()`
  - `RecordId::new()` now accepts `RecordOrigin` parameter for accurate metadata
  - Graph methods in `AimDbInner`: `build_dependency_graph()`, `graph_nodes()`, `graph_edges()`, `graph_topo_order()`
- **Record Drain API (Design 019)**: Non-blocking batch history access
  - **`try_recv()` on BufferReader**: Non-blocking receive returning `Ok(T)`, `Err(BufferEmpty)`, or `Err(BufferLagged)`
  - **`try_recv_json()` on JsonBufferReader**: JSON-serialized non-blocking receive for remote access
  - **`record.drain` AimX protocol method**: Drain accumulated values since last call with optional limit
  - Cold-start semantics: first drain creates reader and returns empty
  - Supports `SpmcRing` (full history), `SingleLatest` (at most 1), `Mailbox` (at most 1)
  - Handler maintains per-connection drain readers via `ConnectionState`
- **Extension Macro**: `impl_record_registrar_ext!` macro in `ext_macros.rs` for generating runtime adapter extension traits
- **Dynamic Topic/Destination Routing (Design 018)**: Complete support for dynamic topic resolution
  - **Outbound (`TopicProvider` trait)**: Dynamically determine MQTT topics or KNX group addresses based on data being published
    - New `TopicProvider<T>` trait for type-safe topic determination
    - New `TopicProviderAny` trait for type-erased storage
    - New `TopicProviderWrapper<T, P>` struct for type erasure
    - New `TopicProviderFn` type alias for stored providers
    - New `topic_provider` field in `ConnectorLink` struct
    - New `with_topic_provider()` method on `OutboundConnectorBuilder`
    - `OutboundRoute` tuple now includes optional `TopicProviderFn`
  - **Inbound (`TopicResolverFn`)**: Late-binding topic resolution at connector startup
    - New `TopicResolverFn` type alias for closure-based topic resolution
    - New `topic_resolver` field in `InboundConnectorLink` struct
    - New `with_topic_resolver()` method on `InboundConnectorBuilder`
    - New `resolve_topic()` method on `InboundConnectorLink`
    - `collect_inbound_routes()` now resolves topics dynamically at route collection time
  - Enables runtime topic determination from smart contracts, service discovery, or configuration
  - Works in both `std` and `no_std + alloc` environments
- **Unit Tests**: Added comprehensive tests for `TopicProvider` and `TopicResolverFn`

### Changed

- **Renamed `.with_serialization()` to `.with_remote_access()`**: Clearer naming for JSON serialization configuration
- **`RecordId` constructor**: Now requires `RecordOrigin` parameter for dependency graph support
- **`set_from_json` protection**: Now also rejects writes on records with active transforms (in addition to sources)
- **Outbound Route Collection**: `collect_outbound_routes()` now returns `OutboundRoute` tuples with optional `TopicProviderFn`
- **Inbound Topic Resolution**: `collect_inbound_routes()` now calls `link.resolve_topic()` instead of `link.url.resource_id()` directly, enabling dynamic topic resolution

## [0.4.0] - 2025-12-25

### Added

- **RecordKey Trait (Issue #65)**: `RecordKey` is now a trait instead of a struct
  - Enables user-defined enum keys with `#[derive(RecordKey)]` for compile-time safety
  - `as_str()` method for string representation
  - `link_address()` method for connector metadata (MQTT topics, KNX addresses)
  - `Borrow<str>` bound for O(1) HashMap lookups by string
  - Blanket implementation for `&'static str`
- **StringKey Type**: New struct replacing old `RecordKey` struct
  - `Static(&'static str)` variant for zero-allocation keys
  - `Interned(&'static str)` variant using `Box::leak` for O(1) Copy/Clone
  - Implements `RecordKey` trait
- **derive Feature**: New feature flag enabling `#[derive(RecordKey)]` macro via `aimdb-derive`

### Changed

- **Breaking: RecordKey struct → trait**: The `RecordKey` struct is now a trait. Use `StringKey` for string-based keys or define custom enum keys with `#[derive(RecordKey)]`
- **StringKey Memory Model**: Dynamic keys now use `Box::leak` (interning) instead of `Arc<str>`. This optimizes for O(1) cloning at the cost of never freeing dynamic key memory (acceptable for startup-time registration pattern)

### Migration Guide

**RecordKey is now a trait (breaking change)**

If you were using `RecordKey` directly, switch to `StringKey`:

```rust
// Before (this PR)
use aimdb_core::RecordKey;
let key: RecordKey = "sensors.temp".into();

// After
use aimdb_core::StringKey;
let key: StringKey = "sensors.temp".into();
```

**For compile-time safe keys (recommended for embedded)**

Use the new derive macro:

```rust
use aimdb_core::RecordKey;  // Now a trait, re-exported from aimdb-derive

#[derive(RecordKey, Clone, Copy, PartialEq, Eq)]
pub enum AppKey {
    #[key = "temp.indoor"]
    TempIndoor,
    #[key = "temp.outdoor"]
    TempOutdoor,
}

// Compile-time typo detection!
let producer = db.producer::<Temperature>(AppKey::TempIndoor);
```

**Note on StringKey memory model**

`StringKey::intern()` leaks memory intentionally for O(1) Copy/Clone. This is
designed for startup-time registration (<1000 keys). In debug builds, a
warning fires if you exceed 1000 interned keys.

## [0.3.0] - 2025-12-15

### Added

- **RecordId + RecordKey Architecture (Issue #60)**: Complete rewrite of internal storage for stable record identification
  - `RecordId`: u32 index wrapper for O(1) Vec-based hot-path access
  - `StringKey`: Hybrid `&'static str` / interned with `Borrow<str>` for zero-alloc static keys and flexible dynamic keys
  - O(1) key resolution via `HashMap<RecordKey, RecordId>`
  - Type introspection via `HashMap<TypeId, Vec<RecordId>>`
- **Key-Based Producer/Consumer API**:
  - `produce::<T>(key, value)`: Produce to specific record by key
  - `subscribe::<T>(key)`: Subscribe to specific record by key  
  - `producer::<T>(key)`: Get key-bound producer
  - `consumer::<T>(key)`: Get key-bound consumer
- **New Types**: `Producer<T, R>` and `Consumer<T, R>` for key-bound access with `.key()` accessor
- **Introspection Methods**:
  - `records_of_type::<T>()`: Returns `&[RecordId]` for all records of type T
  - `resolve_key(key)`: O(1) lookup returning `Option<RecordId>`
- **New Error Variants**:
  - `RecordKeyNotFound`: Key doesn't exist in registry
  - `InvalidRecordId`: RecordId out of bounds  
  - `TypeMismatch`: Type assertion failed during downcast
  - `AmbiguousType`: Multiple records of same type (use key-based API)
  - `DuplicateRecordKey`: Key already registered
- **RecordMetadata Extensions**: Now includes `record_id: u32` and `record_key: String` fields
- **Buffer Metrics API**: New `BufferMetrics` trait and `BufferMetricsSnapshot` struct for buffer introspection (feature-gated behind `metrics`)
  - `produced_count`: Total items pushed to the buffer
  - `consumed_count`: Total items consumed across all readers  
  - `dropped_count`: Total items dropped due to lag (documents per-reader semantics)
  - `occupancy`: Current buffer fill level as `(current, capacity)` tuple
- **DynBuffer Metrics Method**: Added `metrics_snapshot()` method to `DynBuffer` trait (returns `Option<BufferMetricsSnapshot>`)

### Removed

- **Breaking: Legacy Type-Only Methods**: Removed methods that accessed records by type alone:
  - `get_typed_record<T>()`: Use `get_typed_record_by_key::<T>(key)` instead
  - `produce<T>(value)`: Use `produce::<T>(key, value)` instead
  - `subscribe<T>()`: Use `subscribe::<T>(key)` instead
  - `producer<T>()`: Use `producer::<T>(key)` instead
  - `consumer<T>()`: Use `consumer::<T>(key)` instead
  - This eliminates `AmbiguousType` errors - all record access now requires explicit keys

### Changed

- **Breaking: `configure<T>()` Signature**: Now requires key parameter: `configure::<T>("key", |reg| ...)`
- **Breaking: Internal Storage**: Changed from `BTreeMap<TypeId, Box<dyn AnyRecord>>` to:
  - `Vec<Box<dyn AnyRecord>>` for O(1) hot-path access by RecordId
  - `HashMap<RecordKey, RecordId>` for O(1) name lookups
  - `HashMap<TypeId, Vec<RecordId>>` for type introspection
- **Breaking: Key-Based API Only**: All producer/consumer methods now require a record key parameter. This properly supports multi-instance records (e.g., multiple temperature sensors with the same type)
- **SecurityPolicy**: `ReadWrite` variant now uses `HashSet<String>` for writable record keys
- **Dependencies**: Added `hashbrown` for no_std-compatible HashMap with `default-hasher` feature
- **Breaking: DynBuffer No Longer Has Blanket Impl**: The blanket `impl<T, B: Buffer<T>> DynBuffer<T> for B` has been removed. Each adapter now provides its own explicit `DynBuffer` implementation. This enables adapters to provide metrics support via `metrics_snapshot()`. Custom `Buffer<T>` implementations must now also implement `DynBuffer<T>` explicitly.

## [0.2.0] - 2025-11-20

### Added

- **Bidirectional Connector Support**: New `ConnectorBuilder` trait enables connectors to support both outbound (AimDB → External) and inbound (External → AimDB) data flows
- **Type-Erased Router System**: New `Router` and `RouterBuilder` in `src/router.rs` automatically route incoming messages to correct typed producers without manual dispatch
- **Inbound Connector API**: New `.link_from()` method for configuring inbound connections (External → AimDB) with deserializer callbacks
- **Outbound Connector API**: New `.link_to()` method for configuring outbound connections (AimDB → External), replacing generic `.link()`
- **Producer Trait**: New `ProducerTrait` for type-erased producer calls, enabling dynamic routing of messages to different record types
- **Resource ID Extraction**: Added `ConnectorUrl::resource_id()` method to extract protocol-specific resource identifiers (topics, keys, paths)
- **Producer Factory Pattern**: New `ProducerFactoryFn` and `InboundConnectorLink` types for creating producers dynamically at runtime
- **Consumer Trait for Outbound Routing**: New `ConsumerTrait` and `AnyReader` traits in `src/connector.rs` enable type-erased outbound message publishing, mirroring `ProducerTrait` architecture
- **Outbound Route Collection**: Added `AimDb::collect_outbound_routes()` method to gather all configured outbound connectors with their type-erased consumers, serializers, and configs
- **Consumer Factory Pattern**: New `ConsumerFactoryFn` type alias and factory storage in `OutboundConnectorLink` for capturing types at configuration time
- **Type-Erased Consumer Adapter**: Added `TypedAnyReader` in `src/typed_api.rs` implementing `AnyReader` trait for `Consumer<T, R>`

### Changed

- **Breaking: Connector Registration**: Changed from `.with_connector(scheme, instance)` to `.with_connector(builder)` - connectors now registered as builders
- **Breaking: Async Build**: `AimDbBuilder::build()` is now async to support connector initialization during database construction
- **Breaking: ConnectorBuilder Trait**: Updated `build()` signature to take `&AimDb<R>`, enabling route collection via `db.collect_inbound_routes()`
- **Breaking: Sync Bound on Records**: Added `Sync` bound to `AnyRecord` trait and `TypedRecord<T, R>` implementation. Record types must now be `Send + Sync` (previously only `Send`). This enables safe concurrent access from multiple connector tasks in the bidirectional routing system. Types that were `Send` but not `Sync` must be wrapped in `Arc<Mutex<_>>` or `Arc<RwLock<_>>` for interior mutability.
- **Breaking: Connector Naming Refactor**: Renamed connector-related APIs to explicitly distinguish outbound (AimDB → External) from inbound (External → AimDB) flows:
  - `TypedRecord::connectors` field → `outbound_connectors`
  - `TypedRecord::add_connector()` → `add_outbound_connector()`
  - `TypedRecord::connectors()` → `outbound_connectors()`
  - `TypedRecord::connector_count()` → `outbound_connector_count()`
  - `AnyRecord::connector_count()` → `outbound_connector_count()`
  - `AnyRecord::connector_urls()` → `outbound_connector_urls()`
  - `AnyRecord::connectors()` → `outbound_connectors()`
  - `RecordMetadata::connector_count` → `outbound_connector_count`
  - Inbound methods (`inbound_connectors()`, `add_inbound_connector()`) remain unchanged for clarity
- **Deprecated `.link()`**: Generic `.link()` method deprecated in favor of explicit `.link_to()` and `.link_from()`
- **Builder API**: Enhanced builder to validate buffer requirements for inbound connectors at configuration time
- **Breaking: Outbound Consumer Architecture**: Refactored outbound connector system to use trait-based type erasure:
  - Removed: `TypedRecord::spawn_outbound_consumers()` method (automatic spawning)
  - Changed: `OutboundConnectorLink` now stores `consumer_factory: Arc<ConsumerFactoryFn>` instead of direct consumer
  - Changed: Connectors must now implement `spawn_outbound_publishers()` and explicitly call `db.collect_outbound_routes()`
  - Impact: All connectors require code changes to support outbound publishing via the new trait system

### Fixed

- **Memory Management**: Removed `async-trait` dependency that leaked `std` into `no_std` builds, replaced with manual `Pin<Box<dyn Future>>`
- **Type-Erased Routing**: Fixed producer storage in collections by implementing `ProducerTrait` with `Box<dyn Any>` downcasting
- **Buffer Validation**: Added validation ensuring inbound connectors have configured buffers before link creation
- **Consumer Factory Downcasting**: Fixed type downcasting in `typed_api.rs` line 565 - changed from downcasting to `Arc<AimDb<R>>` to downcasting to `AimDb<R>` then wrapping in Arc, resolving "Invalid db type in consumer factory" runtime panic

### Removed

- **Channel Stream**: Removed obsolete channel-based stream abstraction in favor of router-based routing
- **Automatic Outbound Spawning**: Removed `TypedRecord::spawn_outbound_consumers()` method - outbound publisher spawning now explicit via `ConsumerTrait` and `spawn_outbound_publishers()`

## [0.1.0] - 2025-11-06

### Added

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
- Connector abstraction for external system integration
- Builder pattern API for database configuration
- Record lifecycle management with type-safe APIs

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/aimdb-dev/aimdb/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/aimdb-dev/aimdb/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/aimdb-dev/aimdb/compare/v0.5.0...v1.0.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
