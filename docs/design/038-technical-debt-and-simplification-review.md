# 038 — Technical Debt & Simplification Review (breaking changes allowed)

**Status:** Reviewed — owner decisions of 2026-07-01 are recorded inline as **Decision** blocks; see also the status column in §3.14
**Scope:** Whole workspace (~55,000 lines of Rust across 19 library crates, 2 tools, 20 example/bench crates)
**Goal:** Keep every current capability, but with a simpler implementation and materially less code. Unlike review 034 — which was constrained to be functionality-preserving at the API level — **breaking API changes are explicitly on the table** here. The question asked of every subsystem is: *if we rebuilt this today, knowing what we know, what would we not build?*

**Relationship to 034–037:** Review 034 identified the debt of its era (the `R` type parameter, `dyn Any` plumbing, dual-shape `DbError`, per-runtime KNX state machines) and the 035/036/037 follow-ups fixed most of it. This review is the next iteration: it assumes 034's fixes as the baseline and targets the debt that is *still there* — most of it either predates 034 and survived because removing it was breaking, or is scaffolding the 028→037 refactor series left behind.

---

## 1. Executive summary

The core dataflow idea — typed records, three buffer semantics, source/tap/transform/link stages, one builder, one runner — is sound, small, and worth keeping exactly as it is. The debt is not in the model; it is in the **layers of superseded abstraction that were never deleted**, the **speculative surface built for consumers that never arrived**, and **subsystems that grew inside this repo but belong outside it**.

The five headline findings:

1. **Three runtime abstractions coexist for one job.** `RuntimeOps` (dyn-safe, the one actually used), the generic `RuntimeAdapter`/`TimeOps`/`Logger`/`Runtime` family (superseded by issue #131, still implemented by every adapter), and `aimdb_core::time::{TimestampProvider, SleepCapable}` (fully dead, consumed only by adapter wrapper modules that are themselves unused). Two of the three can be deleted outright.
2. **Three hand-written buffer implementations for three runtimes**, when the WASM adapter already proves that a single mutex+waker-list implementation covers all three semantics in ~375 lines with no runtime primitives at all. One portable buffer in core would delete ~2,000 lines, remove the per-adapter `*RecordRegistrarExt::buffer()` extension traits, and shrink the adapters to what they actually are: a clock and a logger.
3. **`DbError` is a kitchen sink with a dead ABI attached.** 27 variants spanning five layers (core, buffers, sync API, embedded hardware, transforms), a numeric error-code registry (`error_code()`/`error_category()`, 0x1000–0xC000 ranges) with **zero production consumers**, ten `is_*` predicates used only in tests, a 175-line `with_context()` match that no production code calls, and two variants (`AmbiguousType`, `DuplicateRecordKey`) that are never constructed anywhere.
4. **Two remote-access wire protocols.** The session substrate (`aimdb-core::session`) was built precisely so every transport could share one engine and one protocol — and UDS/serial/AimX ride it. The WebSocket stack then shipped its own second protocol (`aimdb-ws-protocol`'s `ServerMessage`/`ClientMessage`) with its own codec, subscription manager, and browser client. Every protocol feature (snapshots, acks, writes, queries) now exists twice.
5. **A code-generation product lives inside the database repo.** The architecture-agent pipeline (`aimdb-codegen`, 3,900 lines, including 2,234 lines of string-template Rust generation for entire Cargo projects, plus ~1,700 lines of MCP session state machine) is a separate product with a different release cadence and a template surface that silently rots against the real API. The README quickstart has *already* drifted from the API (see §2.6) — generated-code templates rot the same way, just invisibly.

Rough sizing: the mechanical deletions (§3.1–§3.8) remove **~4,500–5,500 lines** from the library crates with no capability loss. The structural consolidations (§3.9–§3.12) are worth another **~4,000–6,000 lines** but are real engineering projects. Together this is on the order of **15–20 % of the workspace**, concentrated in exactly the crates that carry the maintenance burden.

---

## 2. Assessment of the current code

### 2.1 Size and shape

| Crate | Lines (src+tests) | Role | Assessment |
|---|---:|---|---|
| `aimdb-core` | 15,069 | records, buffers traits, builder, session engine, remote access | Sound model, heavy surface. ~183 `cfg(feature = …)` gates |
| `tools/aimdb-mcp` | 6,988 | MCP server + architecture agent | Two products in one binary; agent half is scope creep |
| `aimdb-codegen` | 3,883 | state.toml → Mermaid + full Rust projects | Belongs outside the runtime repo |
| `aimdb-knx-connector` | 3,664 | KNX, sans-io engine + 2 transports | Good shape post-034; tunnel engine is the model to copy |
| `aimdb-websocket-connector` | 3,501 | WS server/client, own protocol | Duplicates the AimX protocol's job |
| `aimdb-tokio-adapter` | 3,430 | tokio runtime + buffers | ~85 % of it is the buffer impl + tests |
| `aimdb-embassy-adapter` | 2,549 | embassy runtime + buffers + connector spines | const-generic buffer API leaks everywhere |
| `aimdb-wasm-adapter` | 2,474 | wasm runtime + buffers + WS bridge | contains the third AimX-like client |
| `aimdb-sync` | 2,042 | blocking facade | Self-contained, reasonable |
| `aimdb-mqtt-connector` | 1,629 | rumqttc (tokio) + mountain-mqtt (embassy) | Two clients is inherent; fork vendoring is not |
| `aimdb-serial-connector` | 1,487 | COBS framing + 2 transports | Good shape (sans-io framing shared) |
| `aimdb-data-contracts` | 1,468 | Streamable/Migratable/Observable/Linkable/Simulatable | Not consumed by core; half the traits have no consumer at all |
| `tools/aimdb-cli` | 1,546 | CLI over aimdb-client | Fine |
| `aimdb-client` | 1,244 | AimX client for CLI/MCP | Third client engine in the workspace |
| `aimdb-persistence`(+sqlite) | 1,335 | tap-based persistence | Good design (rides existing subscriber model) |
| `aimdb-uds-connector` | 391 | UDS transport over session engine | **The model citizen** — this is what a connector should cost |
| `aimdb-ws-protocol` | 353 | second wire protocol | Exists only because of finding #4 |
| `aimdb-executor` | 288 | runtime traits | 60 % of it is superseded (§2.2) |
| `aimdb-derive` | 266 | `#[derive(RecordKey)]` | Fine |

Comment/doc density is ~24 % of all lines. That is not automatically debt — but a large share of it is *provenance narration* ("design 036 W1", "issue #133 contract", "pre-W8 double box is gone") rather than behavior documentation. Provenance belongs in git blame and the design docs; in the code it goes stale and doubles the reading distance of every function. See D10.

### 2.2 The runtime abstraction: one contract, three implementations of the contract's *definition*

What core actually consumes is exactly one trait — `aimdb_executor::RuntimeOps` (5 methods: `name`, `now_nanos`, `unix_time`, `sleep`, `log`), held as `Arc<dyn RuntimeOps>` and surfaced to user code through `RuntimeContext` → `Time`/`Log` accessors. This design is good: dyn-safe, tiny, one boxed sleep per call which matches the cost model.

Around it, two fossil layers survive:

- **The generic family** `RuntimeAdapter` + `TimeOps` (associated `Instant`/`Duration` types, 8 methods) + `Logger` + the `Runtime` bundle + `RuntimeInfo`, all in `aimdb-executor`. Core's own `lib.rs` says it no longer consumes them ("import those traits from `aimdb_executor` directly if an adapter still needs them"). No adapter *needs* them — each adapter implements `TimeOps`/`Logger` and then immediately re-flattens them into its `RuntimeOps` impl. The family exists so that the family's implementations have something to implement.
- **`aimdb_core::time`** (196 lines): `TimestampProvider`, `SleepCapable`, `utils::measure_async`, `utils::create_timeout_error` (which maps a timeout to `ConnectionFailed { endpoint: "timeout" }` — a category error in itself). Its only consumers are `aimdb-tokio-adapter/src/time.rs` (69 lines) and `aimdb-embassy-adapter/src/time.rs` (133 lines), wrapper modules that nothing else in the workspace calls.

There is also leftover *executor* surface from before the runner model: `TokioAdapter::spawn_task` and `TokioBuffer::spawn_dispatcher` (plus the Embassy equivalent) are public API with zero callers in the workspace — design 028 removed the spawn model, but the methods stayed.

### 2.3 Buffers: the same three semantics, hand-built three times

The three semantics (SPMC ring, single-latest, mailbox) are implemented per adapter:

- **Tokio** (1,495 lines): `broadcast` + `watch` + a hand-rolled mutex/waker-list mailbox, with `ReusableBoxFuture` gymnastics because tokio's channels expose no poll API.
- **Embassy** (698 lines): `PubSubChannel`/`Watch`/`Channel` with **five const-generic parameters** (`CAP, SUBS, PUBS, WATCH_N`) that leak into every embedded call site and require an `unsafe` `&'static` promotion helper.
- **WASM** (375 lines): `Rc<RefCell<…>>` + waker lists — all three semantics, no runtime primitives, no per-message allocation, and the simplest code of the three.

The WASM implementation is the existence proof: buffer semantics need a lock and a waker list, not a runtime. The tokio mailbox variant already *is* this pattern (`StdMutex<MailboxState>` + `Vec<Waker>`), sitting next to two tokio-channel-based variants that need reusable-box workarounds to fit the poll-based `BufferReader` trait that core defined *for* them.

The knock-on structure this forces:

- A **static `Buffer<T>` trait** (with associated `Reader`) in core whose only consumers are the three adapters' own `DynBuffer` impls — no generic code anywhere uses `Buffer<T>`.
- Three **per-adapter registrar extension traits** (`TokioRecordRegistrarExt`, embassy/wasm equivalents) existing solely to provide `.buffer(cfg)`.
- The **`buffer_raw` / `buffer_with_cfg` / `buffer_cfg` triple** on the registrar, and the `("unknown", None)` fallback in `buffer_info()` for buffers registered without a cfg — complexity that exists only because core cannot construct its own buffers.

### 2.4 Records, registry, and validation: correct, but built for a scale it will never see

- `AimDbInner` maintains **five parallel index structures** (`storages`, `by_key`, `by_type`, `types`, `keys`) for a registry that is immutable after `build()` and holds tens of records. `types` exists to pre-validate downcasts that `as_any().downcast_ref()` already validates; `keys` is a reverse map of `by_key`; `by_type` serves one introspection method (`records_of_type`) with a single caller chain.
- The **`AnyRecord` trait family** (`AnyRecord` + supertraits `RecordIntrospect`, `RecordMetricsReset`, plus `JsonRecordAccess`) is the 036 split of the old god-trait — but `TypedRecord` still implements ~25 methods across the four traits, many of them one-line delegations to inherent methods of the same type (`fn has_buffer(&self) { TypedRecord::has_buffer(self) }` × 10).
- **Writer-exclusivity validation is duplicated in two directions**: `InboundConnectorBuilder::finish()` checks `has_producer()`/`has_transform()` before registering, *and* `TypedRecord::{set_producer, set_transform, add_inbound_connector}` re-check the same invariants "from the other direction". Two code paths, two sets of error strings, one rule.
- `AnyRecord::validate()`'s doc comment states "Must have exactly one producer and at least one consumer"; the implementation enforces neither (both are optional). The single check it performs (remote access needs a buffer) could live with the other build() checks.
- `DbError::with_context()` — a 175-line exhaustive match that rebuilds every variant to prepend a string — has **no production caller** (only the CLI's `anyhow::Context` shadows the name). Same for `into_anyhow`, the ten `is_*_error()` predicates (one test caller), the numeric `error_code()`/`error_category()` scheme, and 8 of the 9 `RESOURCE_TYPE_*` constants.

### 2.5 The remote-access stack: one excellent engine, two protocols, three clients

The session substrate (`session/mod.rs` + server/client engines + pumps, ~2,300 lines) is genuinely good work: dyn-safe layering (Connection/Listener/Dialer → EnvelopeCodec → Dispatch/Session), runtime-neutral (`futures` channels + `RuntimeOps` clock), and it made `aimdb-uds-connector` a 391-line crate. That is the target picture.

But the workspace ended up with:

| | Protocol | Codec | Client engine |
|---|---|---|---|
| UDS / serial / TCP | AimX (`session::aimx`) | `AimxCodec` | core `session::client` + `aimdb-client` (host tools) |
| WebSocket / browser | `aimdb-ws-protocol` (`ServerMessage`/`ClientMessage`) | `aimdb-websocket-connector/src/codec.rs` (507 lines) | `aimdb-wasm-adapter::WsBridge` (836 lines) |

Both protocols are JSON envelopes carrying subscribe/write/snapshot/ack/ping over the *same* `EnvelopeCodec`/`Dispatch` engine. The differences (MQTT-style wildcards, query messages) are features, not architecture — they could be AimX methods. Every cross-cutting concern (auth, limits, late-join snapshots, error mapping) is currently implemented and tested twice, and the browser story is permanently a second-class fork of the native story.

`aimdb-client` additionally reimplements request/response demux over its own connection type instead of reusing `session::client::run_client` — a third place that knows how to correlate AimX replies.

### 2.6 Documentation and examples

The design-doc culture (40 numbered docs) is a real asset — the 028→037 series is a model of how to pay down debt deliberately. Two liabilities against it:

- **Doc drift is already visible at the front door.** The README quickstart calls `consumer.subscribe().unwrap()` (subscribe is infallible since design 029) and `ctx.time().secs(1)` (no such method on `Time`; the API is `sleep_secs`). If the flagship example rots, generated-code templates (§2.7) rot silently.
- **In-code provenance comments** referencing design docs and issue numbers appear hundreds of times. They freeze a moment of refactoring history into files that will outlive it.

12 example crates plus the 5-crate weather-mesh demo live in the primary workspace, requiring the `default-members = ["aimdb-core"]` escape hatch and a Makefile matrix (`make all` vs `cargo build`) to keep everyday builds usable. 034 Phase 4 decided (2026-06-09) to keep the repo monolithic; this review does not relitigate that, but notes the recurring cost is real and grows with every example.

### 2.7 The architecture agent / codegen subsystem

`aimdb-codegen` reads `.aimdb/state.toml` and emits Mermaid **plus entire Rust projects** — `generate_main_rs`, `generate_cargo_toml`, `generate_hub_main_rs`, `generate_hub_tasks_rs`, `generate_binary_cargo_toml`, … (2,234 lines of string templates), with 754 lines of validation and a 541-line state model. `tools/aimdb-mcp` layers a session state machine (`Idle → Gathering → Proposing → …`, file-locked global stores, conflict detection — ~1,700 lines) on top so an LLM can drive it.

This is a *product* (an AI scaffolding assistant), not part of the data plane. It has no runtime coupling to `aimdb-core` (the templates just print API-shaped strings), which means the compiler never checks the templates against the real API — the one guarantee the rest of the repo is built on. It inflates the MCP tool from "introspect a running instance" (the advertised feature) to a 7,000-line multi-product server.

### 2.8 What is explicitly good and must not be lost

To be clear about the baseline the simplification must preserve:

- The **buffer-defines-flow** model and the three semantics.
- The typed, fused end-to-end pipeline (029/036/037): pre-resolved handles, no `dyn Any` per message, allocation-free consume path.
- **`build()` as the single error surface** collecting every `ConfigError` (issue #133) — the error-collection *mechanism* is right even though `DbError` around it is bloated.
- The **runner model** (collect futures, one `FuturesUnordered`, no spawn trait).
- The **sans-io engine pattern** (KNX `tunnel.rs`, serial `framing.rs`) and the session substrate.
- The dependency graph with cycle detection and topo-ordered collection.
- `aimdb-sync`'s honest scope ("simplicity over perfect semantics").

---

## 3. Potential to reduce code without losing functionality

Ordered roughly by (value ÷ risk). Line counts are estimates against current `wc -l`; "breaking" means downstream source changes required, not capability loss.

### 3.1 Delete the two superseded runtime abstractions — **~700 lines, breaking (trivially)**

- Remove `RuntimeAdapter`, `TimeOps`, `Logger`, `Runtime`, `RuntimeInfo` from `aimdb-executor`; keep only `RuntimeOps`, `LogLevel`, `BoxFuture`, `ExecutorError`.
- Delete `aimdb_core::time` entirely (`TimestampProvider`, `SleepCapable`, `utils::*`) and the adapter `time.rs` wrapper modules (tokio 69, embassy 133, wasm 292 lines).
- Delete `TokioAdapter::spawn_task`, `TokioBuffer::spawn_dispatcher`, and the Embassy dispatcher equivalents (pre-028 executor surface, zero callers).
- Fold what remains of `aimdb-executor` (~120 lines) into `aimdb-core` and retire the crate. A crate boundary for one dyn trait that only core consumes is pure workspace weight; adapters already depend on core.

Migration cost for users: adapters implement one trait instead of four; `use aimdb_executor::…` becomes `use aimdb_core::…`.

### 3.2 One portable buffer implementation in core — **~2,000 lines net, breaking for adapter authors only**

Implement the three semantics once in `aimdb-core` using the pattern the WASM adapter and the tokio Mailbox already use: `Mutex` (std) / `spin::Mutex` (no_std) around slot-or-ring state + a waker list. Both mutex flavors are already dependencies of core; the `lock()` shim in `typed_record.rs` already abstracts them.

Consequences:

- `aimdb-tokio-adapter/src/buffer.rs` (1,495), `aimdb-embassy-adapter/src/buffer.rs` (698 + the const-generic API + the `unsafe` static-promotion helpers), `aimdb-wasm-adapter/src/buffer.rs` (375) all go away; their tests consolidate into one core test suite.
- The static `Buffer<T>` trait, the `DynBuffer`-blanket question, and the three `*RecordRegistrarExt` extension traits disappear; `.buffer(cfg)` becomes an **inherent** registrar method. The registrar triple (`buffer_raw`/`buffer_with_cfg`/`buffer_cfg`) collapses to one method, and `buffer_info()`'s `"unknown"` arm dies because the cfg is always known.
- Embedded call sites lose four const-generic parameters. (Capacity becomes a runtime value backed by `alloc` — already a requirement of the crate — so heapless-style static allocation is not regressed because it never existed here: `EmbassyBuffer` boxes into `Arc` today.)
- The metrics counters (`BufferCounters`) get wired once instead of three times.

Risk: replacing `tokio::sync::broadcast` with a hand-rolled ring means owning the lag/fairness logic (~200 careful lines; the algorithm is well understood — per-reader sequence numbers over a ring, exactly what broadcast does internally). Benchmarks (aimdb-bench B0–B2) exist to validate parity. This is the single highest-leverage change in the workspace: it converts "adapter" from "buffer re-implementation project" to "a clock and a logger" (~150 lines each), which is also the honest description of what porting AimDB to a new runtime should cost.

> **Decision (2026-07-01): deferred pending evidence.** Per-adapter buffers were a *deliberate* design choice — each runtime using its own highly-optimized primitives — not incidental duplication, so the burden of proof is on consolidation. Before any change: run aimdb-bench B0–B2 comparing tokio `broadcast`/`watch` against a hand-rolled mutex+waker ring **on tokio specifically** (the WASM/mailbox evidence is suggestive, not proof), and *extend the bench suite with a multi-threaded contention case* (N subscribers × M worker threads on one ring) — the existing benches are single-pipeline and will not answer the question where `broadcast` plausibly wins. A middle path stays open regardless of the tokio outcome: adopt the portable implementation in core as the default for **Embassy + WASM only** (~1,100 lines removed, including the const-generic leakage and the `unsafe` static-promotion helpers) while tokio keeps its optimized buffers behind the existing `DynBuffer` seam. Implementation detail for the eventual design doc: the no_std flavor should lock via `critical-section` rather than bare `spin` unless producers are guaranteed to run in task context only. This item gets its own design document before any code moves.

### 3.3 Put `DbError` on a diet — **~450 lines in core + ripple, breaking**

- Delete: `with_context()` (175 lines, no callers), `into_anyhow`, the ten `is_*` predicates, `error_code()`/`error_category()` and the numeric registry, `RESOURCE_TYPE_*` (keep nothing; see next point), `AmbiguousType`, `DuplicateRecordKey` (never constructed).
- Move the five sync-API variants (`AttachFailed`, `DetachFailed`, `SetTimeout`, `GetTimeout`, `RuntimeShutdown`) into a `SyncError` in `aimdb-sync` that wraps `DbError` — core should not know a blocking facade exists.
- Move `HardwareError` + the `nb::Error` conversion into the Embassy adapter's error type (its only user).
- `ResourceUnavailable` has exactly one production constructor (embassy `WouldBlock` mapping) — it moves with it.
- What remains in core: connection, buffer (4), record lookup (4–5), configuration (2), runtime, internal, transform (2), io/json wrappers. ~15 variants, all constructed by core.

If a numeric code for embedded transport is ever really needed, derive it *in the embedded transport* from the variant — a protocol concern, not an enum API.

### 3.4 Collapse the registry — **~150 lines, internal only**

Replace the five parallel structures in `AimDbInner` with:

```rust
struct RecordEntry { key: StringKey, type_id: TypeId, record: Box<dyn AnyRecord> }
storages: Vec<RecordEntry>,
by_key: HashMap<StringKey, RecordId>,
```

`records_of_type` becomes a linear scan (registry is ≤100 entries, called from introspection paths only). `get_typed_record_by_id` drops the redundant TypeId pre-check — the downcast is the check; keep the nicer `TypeMismatch` error by comparing on downcast failure.

### 3.5 Merge the record-trait family and delete delegation boilerplate — **~250 lines, internal**

With 034's `R`-removal done, the reason for supertrait splitting (keeping `AnyRecord` object-safe while cfg-gating capabilities) can be served by one trait with cfg-gated defaulted methods — which is exactly what `RecordMetricsReset` already does. Fold `RecordIntrospect` and `RecordMetricsReset` back into `AnyRecord`, keep `JsonRecordAccess` as the one genuinely optional capability, and delete the ten `Trait::method → TypedRecord::method` delegation shims by implementing directly. Remove `validate()` (its one real check joins the other build()-time checks; its doc comment currently describes rules that do not exist).

### 3.6 One home for writer-exclusivity validation — **~120 lines, internal**

Validate "at most one of {source, transform, link_from}" in exactly one place: `build()`, from `record_origin()` + counts, where the record key is already known (no more empty-key backfill for these). Delete the paired checks in the registrar builders *and* the `TypedRecord` setters, and the duplicated error strings. The user-visible behavior (all mistakes reported from `build()`) is unchanged — this is the mechanism issue #133 already established; the per-setter checks predate it.

### 3.7 Drop the raw/context (de)serializer duality — **~150 lines, breaking**

`with_serializer_raw(|v| …)` vs `with_serializer(|ctx, v| …)` (and the deserializer twins) are two public APIs, two stored `Option`s, and "mutually exclusive" bookkeeping for a distinction the user can express by ignoring a parameter. Keep only the context-aware form. A migration is a `|_ctx, v|` prefix.

### 3.8 Prune dead/unused public API — **~300 lines, breaking (nominally)**

- `RecordT` + `register_record` + `register_record_with_key`: the self-registering-record pattern has zero users in the workspace (no example, no connector, no tool). `configure()` is the API.
- `ConnectorLink::with_config` (builder sets the field directly), `outbound_connector_count` next to `outbound_connectors().len()`, `RecordValue::cloned` next to `get().clone()` — micro-surface that accumulates.
- `Extensions` audit: keep (persistence uses it), but it is the pattern to watch — type-map escape hatches attract functionality that should be parameters.

### 3.9 One remote-access protocol — **~1,200–1,800 lines, breaking for browser clients, real work**

Port the WebSocket connector to speak AimX over its existing WS transport (it already runs on the session engine; the delta is the envelope), extend AimX with the two features that justified the fork (wildcard subscribe, query passthrough — both fit the `method`/`topic` model), and rewrite `WsBridge` as an AimX client. Then delete `aimdb-ws-protocol` (353), the WS codec (507), and the protocol-specific halves of `server/` and `WsBridge`. End state: **one protocol, one snapshot/ack/auth semantics, every transport including the browser**. This also collapses the documentation story ("AimX is how you talk to AimDB — over UDS, serial, TCP, or WebSocket").

Follow-up in the same theme: fold `aimdb-client`'s hand-rolled demux onto `session::client::run_client` so reply correlation exists once.

### 3.10 Extract the architecture agent — **~4,000 lines out of this repo, organizational**

Move `aimdb-codegen` + the MCP `architecture`/`prompts` modules to their own repository/product with its own release cadence. The MCP server that remains is the advertised one: introspection of a *running* instance (records, schema, graph, metrics) — roughly 3,000 lines. If extraction is too big a step now, the minimal in-repo fix is to stop generating whole projects (delete the `generate_*_cargo_toml` / `*_main_rs` / hub templates, keep record/schema snippets) and add a CI job that compiles a generated snippet against the workspace so template drift breaks loudly.

> **Decision (2026-07-01): extraction deferred — both stay in the repo for now.** `aimdb-mcp` stays permanently (alongside `aimdb-cli`); `aimdb-codegen` stays at least until extraction is worth the disruption. The in-repo mitigation therefore becomes the *active* recommendation: add a CI job that compiles generated output against the workspace so template drift breaks loudly, and consider trimming the whole-project templates down to record/schema snippets. If codegen is extracted later, the MCP `architecture/` + `prompts/` modules must move **with** it — they are its main consumer, and leaving them behind would invert the coupling instead of removing it.

### 3.11 Feature-flag consolidation — **~40 gates and a testable matrix, breaking for embedded fine-tuners**

Current: 9 features, 183 `cfg` gates in core, with implication chains users must learn (`std → remote-access → json-serialize`, `connector-session` separate but required by aimx…). Collapse to five:

- `std` (default)
- `remote` = today's `json-serialize` + `remote-access` (they ship together on every std path)
- `connector-session` — **stays independent.** The first draft of this doc bundled it into `remote`; review falsified that premise: `aimdb-mqtt-connector`'s and `aimdb-knx-connector`'s `embassy-runtime` features and `aimdb-embassy-adapter`'s `connectors` feature all pull `aimdb-core/connector-session` *alone* (for `pump_sink`/`pump_source`/`Source`/`Payload`), precisely to keep `serde_json`/AimX off flash-constrained targets. That gate is evidence the boundary is well-drawn — keep it.
- `observability` = `metrics` + `profiling` — verified: the pair accounts for ~61 of the 183 gates and nothing in the workspace selects one without the other
- `defmt` / `tracing` as today (log backends)

Every collapsed pair removes both a documentation burden and a never-tested build configuration.

> **Decision (2026-07-01): accepted as amended above** (`observability` merge confirmed; `connector-session` excluded from the `remote` bundle).

### 3.12 Examples and workspace hygiene — **build-time, not lines**

Respecting the 034 Phase-4 decision to stay monolithic: at minimum move the 5-crate weather-mesh demo and the embassy demo crates behind a separate workspace file (they already need custom targets/features), which lets `default-members` and half the Makefile matrix disappear. Vendored forks: track upstreaming of the knx-pico and mountain-mqtt patches as first-class issues — each merged patch deletes a submodule and a `[patch.crates-io]` entry.

> **Decision (2026-07-01): discarded for now.** Examples will be reduced once the codebase is stable. The vendored-fork upstreaming runs on its own track (an Embassy PR is already filed — see D11).

### 3.13 Smaller opportunistic items

- **`ConnectorUrl`**: connectors receive the broker endpoint in their constructor; link URLs (`mqtt://commands/temperature`) use `host` as the first topic segment and reassemble it in `resource_id()`. Split the type honestly: an *endpoint* URL (host/port/credentials, parsed once per connector) and a *link address* (`scheme` + `resource` + query). Deletes the credential/port parsing from the per-link path and the `rfind(':')`-on-IPv6 class of edge cases.
- **`StringKey::intern` debug plumbing**: the dedup table earns its place; the debug-only `INTERNED_KEY_COUNT`/1000-cap assertion is a config knob nobody asked for — a doc note on the leak contract suffices.
- **`TokioAdapter`'s `Logger`**: emoji `println!` as the default log backend of a library is a demo aesthetic; forward to the `log` or `tracing` facade and let the binary choose.
- **README/quickstart CI**: compile the README example (doctest via `#include` or a `examples/readme.rs`) so §2.6 drift cannot recur.
- **`graph.rs` shapes**: `RecordGraphInfo` is a projection of `RecordIntrospect` re-collected at build; pass the trait object instead of copying into a parallel struct.

### 3.14 Estimated totals and review status

| Cluster | Est. lines removed | Breaking? | Risk | Status (2026-07-01) |
|---|---:|---|---|---|
| §3.1 runtime abstractions | ~700 | trivial | low | accepted |
| §3.2 portable buffers | ~2,000 (full) / ~1,100 (middle path) | adapter authors | medium (perf parity) | **deferred** — needs tokio bench evidence + own design doc |
| §3.3 DbError diet | ~450 | match sites | low | accepted |
| §3.4–§3.6 registry/traits/validation | ~520 | no | low | accepted |
| §3.7–§3.8 API pruning | ~450 | mechanical | low | accepted |
| §3.9 one protocol | ~1,500 | browser clients | medium–high | open (no decision yet; needs design doc) |
| §3.10 extract agent | ~4,000 (relocated) | no | organizational | **deferred** — CI drift check instead |
| §3.11 features | (gates, not lines) | feature selectors | low | accepted as amended |
| §3.12 examples/workspace | (build time) | no | low | **discarded for now** |

---

## 4. Bad design decisions (a self-critical list)

These are root causes, not symptoms — each explains a cluster of the findings above. Several were reasonable at the time; they are listed because *recognizing the pattern* is what prevents the next instance.

**D1 — Building abstractions for consumers that never arrived.** The numeric error-code ABI, `with_context`, the `is_*` predicate family, `RecordT` self-registration, `Simulatable`/`Observable` contracts with no core integration, Kafka/HTTP/DDS/shmem routing documentation, `default_port` for protocols with no connector — all built ahead of demand, none consumed. Speculative generality is the workspace's most repeated pattern. *Rule going forward: no public API without a caller in the same PR.*

> **Decision (2026-07-01): `Simulatable`/`Observable` exit D1 by completion, not deletion.** These traits are intended core parts of the data-contract story — a `Simulatable` record should hand its generator straight to a source, an `Observable` record should be tappable out of the box — and review confirmed the gap: `RecordRegistrar::source()`/`.tap()` are fully generic with no trait bounds, `Simulatable::simulate()` is only ever called by hand inside example producers, and `Observable`'s sole non-example consumer is the opt-in `log_tap()` helper the user must wire manually. The fix is to add the missing consumer rather than delete the API, via **extension traits in `aimdb-data-contracts`** (the `.persist()` pattern), *not* trait bounds on `.source()`/`.tap()` themselves, which must stay general-purpose: a `SimulatableRegistrarExt::simulate(cfg)` that installs the generate→produce→sleep source loop, and an `ObservableRegistrarExt::observe(node_id)` that installs `log_tap` as a tap. Core takes no new dependency; the engine consumes the traits through the same mechanism third parties would use. The README's `Observable` claim ("automatic per-record metrics") must either become true (wire it to the `metrics`/`profiling` snapshot path) or be reworded — today the trait is about log formatting. This gets its own follow-up design doc; it also turns `aimdb-data-contracts` from a side-channel into a consumed layer. The *other* D1 items (error-code ABI, `with_context`, predicates, `RecordT`, phantom-protocol docs) remain deletions.

**D2 — Adding the new abstraction without deleting the old one.** Three time abstractions (§2.2), executor surface surviving the executor's removal (spawn_task/spawn_dispatcher), the generic trait family surviving #131, per-setter validation surviving the #133 build()-collection design, `RecordIntrospect` delegation shims surviving the 036 split. Every refactor in the 028→037 series moved the architecture forward and left its scaffolding standing. The debt is not that the refactors happened — it is that "delete the superseded thing" was never the final commit of the series.

**D3 — Treating the runtime as the unit of implementation instead of the unit of *scheduling*.** Deciding early that each runtime adapter would ship its own buffer implementations coupled the data structures to the runtimes, which created the static `Buffer` trait, the extension-trait registration pattern, the const-generic leakage on Embassy, and three test suites for one behavior. The actual runtime dependency of a buffer is "wake me later" — a `Waker`, which is runtime-neutral by construction. The WASM adapter accidentally proved this and nobody promoted the proof to the architecture.

> **Decision (2026-07-01): reframed — this was a deliberate bet, not an accident.** Per-adapter buffers were chosen intentionally so each runtime could use its own optimized primitives. The bet's premise is now testable and will be tested (§3.2 decision: tokio benchmarks first, then a dedicated design document) rather than assumed in either direction.

**D4 — Two wire protocols for one feature.** The session substrate was designed (remote-access-via-connectors.md) to make transports thin over one protocol. Shipping the WebSocket stack with its own message set anyway — presumably to move fast on the browser demo — forked snapshots, acks, auth, errors, and client engines. The lesson is about sequencing: the browser client should have been the *forcing function* to extend AimX (wildcards, query), not a bypass around it.

**D5 — URL cosplay for topic addressing.** `link_to("mqtt://sensors/temp")` reads nicely, but it launders a topic string through a URL parser: the first topic segment becomes `host`, gets credential/port treatment it can never have, and `resource_id()` glues it back together. Meanwhile the *real* endpoint URL is a constructor argument parsed by the same type. One type, two incompatible meanings — a classic stringly-typed API dressed up as structure.

**D6 — Scope creep into product territory.** The architecture agent (TOML state machine + full-project codegen + MCP ideation sessions) is a genuinely interesting product that does not belong in the data-plane repo. It ties an LLM-workflow release cadence to the database's, and its string templates are the only "API consumers" in the workspace the compiler cannot check. The README drift (§2.6) is the small, visible version of the same failure mode.

> **Decision (2026-07-01):** `aimdb-mcp` stays (alongside `aimdb-cli`); `aimdb-codegen` stays in the repo *for now* — extraction deferred. See the §3.10 decision for the active mitigation (CI drift check for generated code) and the constraint if extraction happens later (the MCP `architecture/`+`prompts/` modules move with codegen).

**D7 — Encoding scale assumptions the system contradicts.** Five index structures and O(1)-everything rhetoric for a registry of ≤100 records that is immutable after startup; interned-and-leaked `Copy` keys with a debug-only misuse detector for keys created once at boot; `by_type` indexing for a lookup used only by introspection. The hot path (produce/consume) was correctly optimized via pre-resolved handles (029) — which made the registry's micro-optimizations irrelevant, and they stayed anyway.

**D8 — The kitchen-sink error enum.** Letting every layer (sync facade, embedded HAL, transforms, IO) add variants to `DbError` instead of defining errors where they occur produced a 27-variant enum with cfg-gated arms, a 64-byte size test, and conversion helpers in adapters that exist to satisfy the enum rather than the caller. Layer-local errors wrapping inward (`SyncError(DbError)`) is the boring, right answer.

**D9 — cfg-gates as a feature system.** Nine features with implication chains and 183 gates in core means the tested matrix is a sliver of the shipped matrix. Several distinctions (`metrics` vs `profiling`, `json-serialize` vs `remote-access` vs `connector-session`) exist because features were added per design-doc rather than per user-facing capability. Every gate is a fork of the codebase; forks need justification stronger than "it was a separate milestone."

**D10 — Narrating provenance instead of documenting behavior.** Hundreds of comments cite design docs and issue numbers ("design 036 W1", "issue #133 contract", "pre-W8 the outer+inner box…"). Six months from now these are riddles; the design docs already hold the history. Comments should state invariants and constraints the code cannot express — the current style doubles file length (24 % comment density) while the README example silently stopped compiling.

> **Decision (2026-07-01):** accepted — file a tracked task/issue for a docs clean-up pass (strip provenance narration to invariants; fix the README quickstart; add the README-compiles CI check from §3.13).

**D11 — Vendoring an ecosystem to patch a leaf.** Carrying the entire Embassy monorepo plus two forked crates as submodules with `[patch.crates-io]` — to hold version-compatibility patches and small fixes — taxes every clone, every CI run, and every contributor's mental model. The patches were the debt; the submodules made them structural.

> **Decision (2026-07-01):** handled separately — an upstream Embassy PR is already filed. Each merged upstream patch retires a submodule and a `[patch.crates-io]` entry.

**D12 — Marketing vocabulary in API design.** "Four capability traits, opt-in, type-checked" (`Streamable`/`Migratable`/`Observable`/`Linkable`) is README architecture; in the code, core consumes none of them, `Observable` and `Simulatable` have no non-example consumer, and the real capability system is feature flags + `with_remote_access()`. When the story and the code diverge, both rot: the story constrains refactoring, the code embarrasses the story. Ship the trait when the engine consumes it.

> **Decision (2026-07-01): resolve by making the code match the story.** Data contracts are a core part of AimDB's identity — the marketed capability model must become real rather than be walked back. Same resolution as D1: implement the engine-side consumption of the contract traits (extension-trait integration, README claims made true or corrected). The principle stands for *future* work in the inverse direction: ship the trait when the engine consumes it.

---

## 5. Sequencing (updated with the 2026-07-01 decisions)

1. **Deletion pass** (§3.1, §3.3, §3.8, dead adapter surface) — one PR series, pure removals, immediately shrinks the review surface for everything after. Add the README-compiles CI check in the same series.
2. **Internal consolidation** (§3.4, §3.5, §3.6, §3.13) — no user-visible change, unlocks cleaner core files.
3. **Docs clean-up pass** (D10 decision) — strip provenance narration to invariants, fix the README quickstart. Tracked as its own task/issue.
4. **Buffer evidence, then decision** (§3.2 decision) — extend aimdb-bench with a multi-threaded contention case; run tokio `broadcast`/`watch` vs. mutex+waker ring; write the buffer design doc with the numbers; only then implement (portable-everywhere, or the Embassy+WASM middle path, or status quo for tokio).
5. **Feature collapse as amended** (§3.11) — `observability` merge + `remote` = json-serialize + remote-access; `connector-session` stays independent.
6. **Data-contracts integration** (D1/D12 decision) — own design doc: `SimulatableRegistrarExt` / `ObservableRegistrarExt`, metrics claim made true or reworded, README capability table updated.
7. **Protocol unification** (§3.9, still open) — own design doc: AimX extensions (wildcards, query), WS transport port, browser client rewrite, `aimdb-ws-protocol` retirement.
8. **Codegen drift check** (§3.10 decision) — CI job compiling generated output against the workspace; template trimming as follow-up. Extraction stays deferred.

Follow-up design docs to open: buffers (step 4, with acceptance criteria = benchmark deltas), data-contracts integration (step 6), protocol unification (step 7, with a protocol feature matrix).
