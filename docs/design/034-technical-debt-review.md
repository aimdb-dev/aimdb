# 034 — Technical Debt & Architecture Review

**Status:** Draft
**Scope:** Whole workspace (aimdb-core, adapters, connectors, tools, examples, build infrastructure)
**Goal:** Identify where the codebase carries technical debt or architectural weight that does not pay for itself, and lay out a path to a leaner codebase **without changing functionality**.

---

## 1. Executive summary

AimDB's core idea is sound and the recent refactor series (028 → 033) has been moving in the right direction: the runtime-neutral session engine, the spawn-free future collection, and the removal of `R` from `Producer`/`Consumer` all reduced real debt. The remaining debt clusters into five themes:

1. **Platform variance is encoded as `cfg` forks instead of shared abstractions.** `aimdb-core` has ~128 `cfg(feature = "std")` gates; most exist only because types that live in `alloc` (`String`, `Arc`, `Box`, `Vec`) are imported through two different paths, and because `DbError` has *structurally different fields* per target.
2. **`dyn Any` is the system's plumbing.** The registry, connector factories, runtime context, serializers, and the builder's own spawn/start function lists all pass through `Box<dyn Any>`/`Arc<dyn Any>` with panicking downcasts. The codebase pays for generics (the `R` parameter on everything) **and** for type erasure at the same time.
3. **Per-runtime duplication in connectors.** The KNX/IP tunneling state machine is implemented twice (~1,000 lines each for tokio and Embassy); MQTT exists twice on two unrelated client ecosystems. The session engine in core proves the better pattern (one engine, thin transports) already works in this repo.
4. **Dead and speculative API surface.** `Database<A>`, the deprecated `.link()`, a vestigial `tokio` dependency in core, `ConnectorConfig`'s Kafka/HTTP/shmem story, and a god-trait `AnyRecord` with ~20 methods spanning six concerns.
5. **Workspace weight.** 12 example crates inside the primary workspace, plus three vendored forks (entire Embassy monorepo, mountain-mqtt, knx-pico) wired through `[patch.crates-io]`.

Rough size of the prize: removing the mechanical debt alone (themes 1 and 4) is on the order of **1,500–2,500 deleted/simplified lines in core and adapters with zero behavior change**. The connector consolidation (theme 3) is worth another ~1,500 lines but is real engineering work.

---

## 2. Method

Reviewed: all of `aimdb-core` (with full reads of `typed_api.rs`, `builder.rs`, `typed_record.rs`, `error.rs`, `connector.rs`, `record_id.rs`, `context.rs`, `database.rs`, `transport.rs`, the `session/` and `remote/` modules), the three runtime adapters, all six connectors, `aimdb-sync`, `aimdb-client`, `aimdb-codegen`, `aimdb-data-contracts`, the tools, the workspace manifests, Makefile and CI layout. Line counts below are from `wc -l` at the time of review.

---

## 3. Findings

### 3.1 The std/no_std split is implemented at the wrong level

**This is the single largest source of noise in the codebase.**

#### 3.1.1 `DbError` has different fields per target

Every variant of `DbError` ([error.rs](../../aimdb-core/src/error.rs)) carries `field: String` under `std` and `_field: ()` under `no_std`:

```rust
RecordKeyNotFound {
    #[cfg(feature = "std")]
    key: String,
    #[cfg(not(feature = "std"))]
    _key: (),
},
```

Consequences ripple through the whole crate:

- **Every construction site needs a dual branch.** `builder.rs` alone has ~8 of these blocks ([builder.rs:185-196](../../aimdb-core/src/builder.rs#L185-L196), [builder.rs:707-737](../../aimdb-core/src/builder.rs#L703-L737), …); `typed_record.rs`, `typed_api.rs` and the adapters repeat the pattern. Helper constructors (`DbError::runtime_error`, `record_key_not_found`, …) exist but are only used on some paths, so both styles coexist.
- **Error context is silently discarded on embedded targets**, where debugging is hardest. The no_std `Display` impl can only print a numeric code.
- **The no_std shape cannot be tested from a std build** — the enum is literally a different type.

The kicker: `aimdb-core` **always** requires `alloc` (`extern crate alloc` is unconditional, and the `alloc` feature underpins every other feature). `alloc::string::String` is available on every supported target, and `thiserror` 2.x supports no_std. The dual-field design solves a problem the crate does not have. If string formatting cost on MCU is the real concern, the right shape is still *one* shape (e.g. `&'static str` context or a compact context struct) — not two enums in a trench coat.

#### 3.1.2 Duplicate import dances and duplicated impl blocks

~25 sites import the same `alloc` types through two paths:

```rust
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc};
#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};
```

`std::sync::Arc` *is* `alloc::sync::Arc`. One unconditional `use alloc::...` works everywhere. The extreme case is [context.rs](../../aimdb-core/src/context.rs), where the **entire `RuntimeContext` impl block is written twice** — std and no_std versions that are character-for-character identical except for the `Arc` path.

#### 3.1.3 Feature-flag combinatorics and a layering violation

`aimdb-core` has 11 features with implication chains (`std → remote-access → json-serialize`, `std → connector-session`, `std → tokio`). Gate counts inside core: 128 `std`, 62 `tracing`, 41 `profiling`, 28 `remote-access`, 22 `metrics`, 12 `json-serialize`, 11 `connector-session`, 7 `defmt`. The matrix is untestable in full, and several gates interact (e.g. `profiling` needs the `RuntimeForProfiling` marker-trait workaround in [lib.rs:46-64](../../aimdb-core/src/lib.rs#L46-L64)).

Concrete vestige: the `std` feature still enables a **non-dev `tokio` dependency with `net`, `io-util`, `sync`, `time`** — but after the session-engine refactor (030/033), the only `tokio::` references left in `aimdb-core/src` are inside `#[cfg(test)]` modules, which are covered by the dev-dependency. Core advertises runtime neutrality while unconditionally pulling tokio into every std build.

The 62 `#[cfg(feature = "tracing")]` blocks wrapping individual `tracing::info!` calls deserve their own mention: one internal `log_debug!`/`log_info!` macro that expands to nothing when the feature is off would delete ~120 lines of attribute noise and make the code readable again.

### 3.2 `dyn Any` is the load-bearing plumbing

The design is "generic at the edges, type-erased in the middle" — and the middle is wide:

| Mechanism | Location | Erasure |
|---|---|---|
| Producer routing | `ProducerTrait::produce_any` | `Box<dyn Any + Send>` per message, downcast per message |
| Consumer routing | `ConsumerTrait::subscribe_any`, `AnyReader::recv_any` | `Box<dyn Any + Send>` per received value |
| Connector factories | `ConsumerFactoryFn` / `ProducerFactoryFn` | take `Arc<dyn Any>`, downcast to `AimDb<R>`, **panic** on mismatch ([typed_api.rs:856-884](../../aimdb-core/src/typed_api.rs#L856-L884)) |
| Runtime context | `source_raw`/`tap_raw` closures receive `Arc<dyn Any + Send + Sync>`; adapters call `RuntimeContext::extract_from_any` which **panics** on mismatch | erased and recovered inside one crate |
| Serializers | `SerializerFn(&dyn Any)`, downcast_ref to `T` | per publish |
| Builder internals | `spawn_fns: Vec<(StringKey, Box<dyn Any + Send>)>`, `start_fns: Vec<Box<dyn Any + Send>>`, downcast at `build()` with `expect` ([builder.rs:325-330](../../aimdb-core/src/builder.rs#L325-L330)) | self-inflicted |

The last row is the clearest case of self-inflicted erasure: `AimDbBuilder<R>` is *already generic over `R`*, and both `on_start()` and `configure()` are only callable after `.runtime()` fixes `R`. The struct could store `Vec<SpawnFnType<R>>` directly. The stated justification (sharing the field with the `NoRuntime` builder) doesn't hold — `start_fns` on the `NoRuntime` builder is always empty because `on_start` isn't available there, and the field type may mention `R` regardless since the struct is generic.

Most of the remaining erasure exists because of one root cause, covered next.

### 3.3 The runtime parameter `R` infects everything — and then gets erased anyway

`TypedRecord<T, R>`, `RecordRegistrar<'a, T, R>`, `AimDb<R>`, `TransformDescriptor<T, R>`, `ConnectorBuilder<R>` — the runtime adapter type threads through every public signature. Yet:

- Connectors implement `ConnectorBuilder<R>` **for all `R`** (e.g. [tokio_client.rs:102](../../aimdb-knx-connector/src/tokio_client.rs#L102)) — they never use `R`.
- The machinery that actually runs services receives the runtime as `Arc<dyn Any>` (`runtime_any()`, `extract_from_any`) — the static type is erased and recovered with panicking downcasts inside the same process.
- Design docs 028/029 (spawn removal, `R`-free `Producer`/`Consumer`) show the project already concluded `R` was over-applied; `Producer<T>`/`Consumer<T>` are now clean. The record registry and registrar are the unfinished half of that migration.

What still genuinely needs `R`: stage-profiling clocks (`TimeOps`), join fan-in (`JoinFanInRuntime`), and the context handed to user closures. All of these are *capabilities*, and the session engine already demonstrates the alternative — runtime-neutral code using `async-channel`/`futures` primitives with capabilities passed as values, not type parameters.

### 3.4 Panic-as-validation, inconsistently applied

Configuration errors split arbitrarily between two failure models:

- **Panics:** invalid URL, missing serializer/deserializer, unregistered scheme, duplicate producer, transform/source conflicts, missing buffer in a consumer factory, type mismatch in `configure` (`assert!`).
- **`DbResult` from `build()`:** duplicate keys, missing runtime, dependency cycles, record validation.

"Fail fast at boot" is a defensible philosophy for an embedded-first library — but then `build()` should not also have a `Result` channel for the *same class* of mistakes. Pick one: either builder methods stay infallible and `build()` performs **all** validation and returns every error (preferable — it also fixes the panic-deep-inside-a-factory-closure problem where the panic site is far from the user's mistake), or panics are the documented contract everywhere.

### 3.5 The lifetime-chained registrar API

`RecordRegistrar` methods take `&'a mut self` and return `&'a mut Self` where `'a` is the *struct's own* lifetime parameter ([typed_api.rs:382-403](../../aimdb-core/src/typed_api.rs#L382-L403)). This is the classic fluent-API lifetime bug: the first method call mutably borrows the registrar for its entire remaining lifetime, so users *must* write one unbroken chain — the crate's own test admits it: *"the registrar's lifetime only permits one borrow chain at a time"* ([typed_api.rs:1714-1715](../../aimdb-core/src/typed_api.rs#L1714-L1715)). It also forces the `for<'a> FnOnce(&'a mut RecordRegistrar<'a, T, R>)` HRTB in `configure`, and it's why `OutboundConnectorBuilder` needs `registrar: &'a mut RecordRegistrar<'a, …>` (a double-`'a` that is almost always a mistake in Rust). The fix is mechanical: fresh lifetimes per call (`fn source(&mut self, …) -> &mut Self`).

### 3.6 Protocol details leak into core; config is stringly-typed

- `with_qos`, `with_retain` — MQTT concepts — are methods on core's generic `OutboundConnectorBuilder`/`InboundConnectorBuilder`, implemented by pushing `("qos", "1")` into a `Vec<(String, String)>`.
- [transport.rs](../../aimdb-core/src/transport.rs)'s `ConnectorConfig` claims protocol-agnosticism with documented interpretations for **Kafka, HTTP, and shmem connectors that do not exist**. Its typed `qos`/`retain` fields are acknowledged in its own doc comment to be unable to represent "unspecified", so the real data flows through `protocol_options: Vec<(String, String)>` anyway.
- A hand-rolled URL parser (`ConnectorUrl::parse`, ~350 lines with tests) lives in core to support this addressing scheme.

Keep the URL-based wiring (it's the product's UX), but the *typed* MQTT knobs belong in the MQTT connector as an extension trait, and `ConnectorConfig`'s speculative fields should go.

### 3.7 Connectors duplicate whole protocol state machines per runtime

| Connector | tokio side | Embassy side | Shared protocol core |
|---|---|---|---|
| KNX | 994 lines (`tokio_client.rs`) | 1,061 lines (`embassy_client.rs`) | **none** — connect/ACK/keepalive/reconnect implemented twice |
| MQTT | 402 lines on `rumqttc` | 494 lines on `mountain-mqtt` | none — two unrelated client ecosystems |
| Serial | 393 lines | — | `framing.rs` (COBS) shared ✔ |

The KNX case is the worst: KNX/IP tunneling is a wire protocol (frames in `knx-pico`), and the connection lifecycle (ConnectRequest → ConnectResponse, TunnelingRequest/Ack, keepalive, reconnect-with-backoff) is pure logic that doesn't need to know whether bytes come from `tokio::net::UdpSocket` or `embassy_net::udp`. A sans-io state machine plus two ~150-line transport shims would replace ~2,000 lines with ~700. The repo has already proven this pattern works at scale: the session engine (`session/client.rs`, `server.rs`, `pump.rs`) is exactly this shape, and UDS/serial/WebSocket all ride it.

MQTT is harder (the duplication is hidden inside third-party clients), so it's lower priority — but note that maintaining a *fork* of mountain-mqtt to keep it compatible (see §3.9) is part of the same cost.

### 3.8 Dead, deprecated, and leftover surface

| Item | Location | Status |
|---|---|---|
| `Database<A>` wrapper | [database.rs](../../aimdb-core/src/database.rs) (140 lines) | Only referenced by the `TokioDatabase`/`EmbassyDatabase` type aliases, which are themselves used nowhere in the workspace. Delete all three. |
| `.link()` | [typed_api.rs:581-591](../../aimdb-core/src/typed_api.rs#L581-L591) | Deprecated since 0.2.0; workspace is at 1.1.0. Delete. |
| `tokio` dependency of `aimdb-core` | [Cargo.toml](../../aimdb-core/Cargo.toml) | Only used in `#[cfg(test)]`; dev-dependency already covers that. Remove from `[dependencies]` and from the `std` feature. |
| `ConsumerTrait::subscribe_any` returning `DbResult` | [typed_api.rs:294-310](../../aimdb-core/src/typed_api.rs#L294-L310) | Infallible since M14; the `Result` is kept "so connector code stays unchanged". Flatten it. |
| `AnyRecord` god-trait | [typed_record.rs:217-362](../../aimdb-core/src/typed_record.rs#L217-L362) | ~20 methods mixing storage access, validation, connector enumeration, graph reporting, JSON remote access, profiling, and metrics. Split into focused traits (storage + introspection + remote), or at minimum move the `cfg`-gated JSON/profiling methods to sub-traits so the core trait is stable. |
| `aimdb-data-contracts` traits (`Simulatable`, `Settable`, `MigrationChain`, …) | whole crate | Consumed only by the wasm adapter, websocket connector and the weather demo. Fine to keep, but audit which traits have zero implementors outside examples — several look speculative. |
| `aimdb-codegen` docs/templates referencing "the actual 0.5.x API" | [lib.rs](../../aimdb-codegen/src/lib.rs) | Version skew: workspace is 1.1.0. Symptom of a 2,244-line string-template generator (`rust.rs`) that must chase the API by hand. |

### 3.9 Workspace and dependency structure

> **Decision (2026-06-09):** reviewed and **accepted as-is**. The monorepo is preferred, and the vendored forks are required (Embassy version compatibility, knx-pico/mountain-mqtt fixes). The observations below stay recorded as accepted trade-offs, not work items; the publish/Makefile complexity they cause is the accepted cost.

- **Examples in the primary workspace.** 12 example crates are workspace members (weather-mesh-demo alone is 5). Every `cargo metadata`, lockfile update, and dependabot run pays for them, and the root manifest needs the `default-members = ["aimdb-core"]` escape hatch with a comment apologizing for it. Moving `examples/` to its own workspace (path-dependent on the libraries) keeps `cargo build`/`clippy` honest and the lockfile small.
- **Three vendored forks as git submodules**, wired via `[patch.crates-io]`: the **entire Embassy monorepo** (7 crates pinned), mountain-mqtt, and knx-pico. Each fork is a standing maintenance liability (rebases, security updates, contributor onboarding — a fresh clone needs submodule init before anything builds) and the patches cannot ship to crates.io, which is why a 13-step `publish-check`/`publish` flow exists in the 551-line Makefile. Track upstreaming the knx-pico/mountain-mqtt fixes and pinning Embassy to released crates as a explicit goal; every quarter the forks live, the diff grows.
- **551-line Makefile** wrapping cargo. Much of it is the feature-matrix and embedded-target juggling from §3.1.3 — it shrinks naturally as the feature graph shrinks; `make check` aggregating fmt/clippy/test/embedded/wasm/deny is worth keeping.

### 3.10 Smaller items worth fixing opportunistically

- **`StringKey::intern` leaks every dynamic key** (`Box::leak`, [record_id.rs:291-306](../../aimdb-core/src/record_id.rs#L291-L306)) to get `Copy`. Guarded only by a debug-build counter (cap 1000). For the stated multi-tenant edge use case ("tenant.{}.sensors") this is an unbounded leak in a long-lived process. A real interner (dedup on intern) would at least make re-interning the same key free; document the contract loudly either way.
- **`OutboundRoute` is a 5-tuple** type alias ([builder.rs:57-63](../../aimdb-core/src/builder.rs#L57-L63)); every connector destructures it positionally. Make it a struct.
- **O(n) scans in the builder**: `configure()` does `records.iter().position(...)` per call and `build()` does `by_key.keys().find(...)` per topo entry. Irrelevant at 10 records, but trivial to fix while touching the file.
- **`ext_macros.rs` generates the adapter extension traits** (`TokioRecordRegistrarExt`, …) via `macro_rules!`, which hides the primary user-facing API from rustdoc/IDE navigation and bakes magic numbers into call sites (`EmbassyBuffer::<T, 16, 4, 4, 1>`). With the registrar lifetime fix (§3.5) and an `R`-slimmed registrar (§3.3), the macro can likely become a plain generic impl.
- **Two JSON wire protocols** (AimX in `session/aimx` + `remote/`, and `aimdb-ws-protocol` for WebSocket/browser) implement overlapping subscribe/write/query semantics with separate codecs and error mapping. Both now ride the same session engine, which is the hard part done — protocol convergence (or at least a shared envelope/error vocabulary) is a candidate for the next protocol-breaking release, not urgent.

---

## 4. The worst design decisions (root-cause list)

Ranked by how much debt each one generated. These are the decisions to *not make again*, independent of how quickly the symptoms get cleaned up.

1. **Encoding the std/no_std split as structural `cfg` forks instead of building on `alloc`.**
   The decision that `DbError` would have different fields per target — rather than committing to `alloc` types everywhere (which the crate requires anyway) — multiplied every error site by two, made errors untestable cross-target, and threw away diagnostics on exactly the targets that need them most. The duplicated `RuntimeContext` impl and the 25 dual-import headers are the same decision repeated. **~Several hundred lines of pure noise, plus an ongoing tax on every new error site.**

2. **Making the runtime adapter a generic parameter `R` on the whole object graph — and then escaping it with `dyn Any` wherever it became unworkable.**
   You currently pay for both: generic infection in every signature (`TypedRecord<T, R>`, `RecordRegistrar<'a, T, R>`, HRTB closures) *and* runtime downcasts with panicking failure paths (`extract_from_any`, factory downcasts, `spawn_fn` downcasts). Either discipline alone would have been cheaper. Designs 028/029 already started the walk-back; the registry is the unfinished half.

3. **Implementing each connector once per runtime instead of separating protocol from I/O.**
   Two full KNX/IP lifecycle implementations and two unrelated MQTT stacks. The cost isn't just the ~2,500 duplicated lines — it's that every bug fix and feature lands twice or diverges. The session engine proves the team knows the right pattern; KNX/MQTT predate it and were never folded in.

4. **Validating configuration by panicking inside builder closures and factories, mixed with a `Result`-returning `build()`.**
   Two failure contracts for one class of error, with the worst panics (consumer-factory "requires a buffer") firing at spawn time, far from the line the user got wrong.

5. **Vendoring an ecosystem (Embassy fork + mountain-mqtt fork + knx-pico fork) as submodules patched over crates.io.** *(accepted trade-off — see §3.9)*
   Justified as a temporary compatibility measure, but it now sits between the project and every release: clean clones don't build without submodules, crates.io publishing needs a bespoke procedure, and the forks drift further from upstream every month.

6. **The `'a`-self-referential fluent registrar.**
   A one-character-class lifetime mistake (`&'a mut self` returning `&'a mut Self` with `'a` = the struct's parameter) that hardened into API shape: it dictated the HRTB in `configure`, the double-`'a` connector builders, and the "single chain only" usage rule that tests have to tiptoe around.

7. **Speculative generality.**
   `ConnectorConfig` documenting Kafka/HTTP/shmem semantics for connectors that were never built; data-contract traits with no implementors outside demos; `Database<A>` as a second façade over `AimDb`; MQTT's `qos`/`retain` lifted into core "for protocol-agnosticism". Each item is small; the habit is the debt.

8. **Keeping 12 example crates inside the primary workspace.** *(accepted trade-off — see §3.9)*
   It made `cargo build` of the workspace expensive enough that `default-members` had to be restricted to `aimdb-core` — which means the default build no longer validates what contributors actually change.

9. **`StringKey::intern` = `Box::leak` to get `Copy` keys.**
   Fine for the static-key embedded path; wrong as the *only* path for dynamic keys in long-lived multi-tenant edge processes. The debug-only counter is an admission of the problem, not a fix.

10. **Two parallel remote-access protocols (AimX and WS-JSON).**
    Understandable history (browser clients needed JSON-over-WS before the session engine existed), but the result is two subscribe/write/query vocabularies, two codecs, and two error mappings to keep in sync.

---

## 5. Remediation roadmap (functionality-preserving)

Ordered so that each phase is independently shippable and nothing changes observable behavior.

> **Tracking** (breaking changes accepted per decision 2026-06-09 — clean structure beats API stability):
>
> | Issue | Covers |
> |---|---|
> | [#129](https://github.com/aimdb-dev/aimdb/issues/129) | Phase 1 — `DbError` unification (§3.1.1) |
> | [#132](https://github.com/aimdb-dev/aimdb/issues/132) | Phase 1 — alloc imports, logging shim, dead API, tokio dep, `OutboundRoute` (§3.1.2, §3.1.3, §3.8, §3.10) |
> | [#130](https://github.com/aimdb-dev/aimdb/issues/130) | Phase 2 — `RuntimeOps` groundwork, typed builder internals, registrar lifetimes (§3.2, §3.5) |
> | [#133](https://github.com/aimdb-dev/aimdb/issues/133) | Phase 2 — panic-free builder validation (§3.4) |
> | [#134](https://github.com/aimdb-dev/aimdb/issues/134) | Phase 2 — MQTT knobs out of core, `ConnectorConfig` pruning (§3.6) |
> | [#131](https://github.com/aimdb-dev/aimdb/issues/131) | Phase 3 — remove `R` from the object graph; data-plane de-`Any` as stretch (§3.2, §3.3) — **implemented** (the §6 de-`Any` stretch was split out as a follow-up, per decision 2026-06-10) |
> | [#135](https://github.com/aimdb-dev/aimdb/issues/135) | Phase 3 — sans-io KNX state machine (§3.7) — **implemented** (`aimdb-knx-connector/src/tunnel.rs`) |
>
> Unfiled by intent: data-plane de-`Any` (§3.2 / #131 §6 — file after #131 merges), `AnyRecord` split (file after #131), KNX ACK-retransmit `TunnelConfig` knob (spec-conformant single retransmit; today expire-and-log, from #135), `StringKey::intern` (§3.10), MQTT consolidation review, AimX/WS protocol convergence. Phase 4 dropped (§3.9 decision).
>
> **All unfiled items are now consolidated in [036 — Follow-up Refactoring](036-followup-refactoring.md), which supersedes this list as the live tracker.**

### Phase 1 — Mechanical deletions (low risk, high noise reduction)

| Action | Est. effect |
|---|---|
| Unify `DbError` on `alloc::string::String` fields; delete all `_field: ()` variants, the no_std `Display` table, and every dual construction branch | −400 to −600 lines, every future error site is one branch |
| Replace all `#[cfg(std)] use std::… / #[cfg(not(std))] use alloc::…` pairs with unconditional `alloc` imports; merge the duplicated `RuntimeContext` impl blocks | −150 lines |
| Internal `log_*!` macros wrapping `tracing`/`defmt`; delete the 62 per-call-site `cfg` gates | −120 lines of attributes |
| Delete `Database<A>`, `TokioDatabase`, `EmbassyDatabase`, deprecated `.link()` | −180 lines |
| Drop `tokio` from `aimdb-core` `[dependencies]` and the `std` feature | dependency graph honesty; faster std builds |
| Flatten `subscribe_any`'s vestigial `DbResult`; struct-ify `OutboundRoute` | small |

### Phase 2 — Internal de-erasure and API hygiene (no public behavior change)

- Store `spawn_fns`/`start_fns` as their typed forms inside `AimDbBuilder<R>`; delete the `Box<dyn Any>` round-trips and their `expect`s.
- Fix registrar lifetimes (`&mut self -> &mut Self` with fresh lifetimes); simplify `configure`'s closure bound; collapse `ext_macros.rs` into plain generic impls if the lifetime fix permits.
- Move all builder-time validation into `build()` returning `DbResult` (collect *all* errors, not first); keep panics only for internal invariants. Public API shape is unchanged — only failure timing moves from "panic mid-configure" to "error from build()", which is strictly friendlier.
- Move `with_qos`/`with_retain` into an MQTT-connector extension trait over the generic builder; deprecate the core methods for one release. Delete `ConnectorConfig`'s speculative fields, keep `protocol_options` + `timeout_ms`.

### Phase 3 — Architecture consolidation (real work, do per-connector)

- **Finish the `R` removal program** (the 029 direction): make `TypedRecord<T>` runtime-free by representing the runtime capabilities it actually needs (clock, join fan-in) as values, mirroring how the session engine already works. This is what finally deletes the `Arc<dyn Any>` runtime-context plumbing and the factory downcasts.
- **Sans-io the KNX connector**: extract the tunneling lifecycle (connect, ACK bookkeeping, keepalive timers as "next deadline" outputs, reconnect policy) into a shared state machine; reduce `tokio_client.rs`/`embassy_client.rs` to socket shims. Apply the same review to MQTT afterwards (harder, third-party clients involved).
- Decide the long-term story for AimX vs WS-JSON (shared envelope/error vocabulary at minimum).

### Phase 4 — Repository structure — **dropped (decision 2026-06-09)**

The monorepo (including the example crates) and the vendored fork submodules stay as they are — see the decision note in §3.9. Upstreaming individual knx-pico/mountain-mqtt patches remains welcome opportunistically, but is not a tracked goal.

### Explicitly keep (this is the good architecture)

- The **session engine** (`session/`) and the M17 connector-spine consolidation — it is the template the rest of the codebase should converge on.
- `WriteHandle`/pre-resolved `Producer`/`Consumer` hot path (029) — correct and fast.
- `RecordKey`/`RecordId` logical-vs-physical identity split and the dependency graph with topological spawn ordering.
- The design-doc discipline in `docs/design/` — it is the reason this review could reconstruct intent at all.

---

## 6. Suggested sequencing

Phase 1 is a weekend-sized, reviewable-in-one-PR-each cleanup and removes the noise that makes every later diff harder to read — do it first and alone. Phase 2 next; it shrinks the surface Phase 3 has to move. Phase 3 items are independent of each other and can ride the existing milestone cadence (the KNX sans-io extraction is the best first candidate — self-contained, measurable, and it retires the largest single duplication in the repo).
