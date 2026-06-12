# 036 — Follow-up Refactoring: Remaining Work from the 034/035 Review Cycle

**Status:** Draft
**Predecessors:** [034 — Technical Debt & Architecture Review](034-technical-debt-review.md), [035 — Review Follow-ups: Deferred Items](035-review-followups-deferred.md)
**Baseline:** the de-genericized tree on PR [#140](https://github.com/aimdb-dev/aimdb/pull/140) (#131 R-removal + #135 sans-io KNX + the 035 fix round). Everything below assumes that baseline; nothing here should start before #140 merges.
**Scope:** Consolidate every item that 034 left "unfiled by intent" and every 035 item that is still live into one actionable list, with per-item state (verified 2026-06-11), approach, and acceptance criteria. After this doc, 034 §5 and 035 §4 are historical records; this is the single live follow-up list.

---

## 1. Where the review cycle stands

Done and merged (PRs #136–#139): 034 Phases 1–2 in full — `DbError` unification (#129), alloc imports / logging shim / dead API / tokio dep / `OutboundRoute` (#132), dyn-safe `RuntimeOps` + de-erased builder internals + registrar lifetimes (#130), panic-free builder validation (#133), MQTT knobs out of core (#134). The opportunistic §3.10 items rode along: the `log_*!` shim exists ([log.rs](../../aimdb-core/src/log.rs)), the builder's O(n) scans are gone, `ext_macros.rs` is deleted, and the codegen doc rot was fixed in the #140 fix round.

In flight (PR #140, this branch): #131 (non-generic `AimDb`/`TypedRecord`/`RuntimeContext`), #135 (sans-io KNX tunnel engine — [tunnel.rs](../../aimdb-knx-connector/src/tunnel.rs), 1,228 shared lines; shims now 581/465 lines vs. ~1,000 each before), plus 035 items 2.1 (select-loop restructure, applied) and 2.5 (`AimDb::runtime()` deleted).

Remaining: the items below. §2 is work to schedule, §3 is assessment-only, §4 is dormant triggers (no action, restated here so nothing lives only in 035).

---

## 2. Work items

### W1 — Data-plane de-`Any` (034 §3.2; the #131 §6 stretch goal, split out per decision 2026-06-10)

**Current state (verified).** #131 removed the *control-plane* erasure (runtime context, factory downcasts, builder internals). The *per-message* erasure remains intact:

| Path | Mechanism | Cost per message |
|---|---|---|
| Inbound (connector → record) | router deserializes to `Box<dyn Any + Send>`, [`ProducerTrait::produce_any`](../../aimdb-core/src/typed_api.rs#L161) downcasts to `T` | 1 heap box + 1 downcast |
| Outbound (record → connector) | [`subscribe_any`](../../aimdb-core/src/typed_api.rs#L295) → `Box<dyn AnyReader>`, [`recv_any`](../../aimdb-core/src/typed_api.rs#L276) → `Box<dyn Any + Send>`, then [`SerializerFn(&dyn Any)`](../../aimdb-core/src/connector.rs#L102) downcasts back to `T` | 1 heap box + 2 erasure crossings |
| Session pump / AimX client | same `subscribe_any`/`recv_any` pair ([pump.rs:57](../../aimdb-core/src/session/pump.rs#L57), [client.rs:538](../../aimdb-core/src/session/client.rs#L538)) | same |
| Join fan-in | each input value crosses as `Box<dyn Any + Send>` ([join.rs:54](../../aimdb-core/src/transform/join.rs#L54)) | 1 box + downcast per input |

**Approach: fuse the typed ends at registration time.** Both ends of every erased hop are typed — `T` is known in `RecordRegistrar<T>`/`TypedRecord<T>` where the route is wired, and the connector spine only actually wants *bytes* (or JSON). Instead of shipping `T` erased across the boundary and recovering it, build the typed pipeline inside a closure at registration and expose only the wire-level interface:

- **Outbound:** replace the `(Arc<dyn ConsumerTrait>, Serializer)` pair carried by `OutboundRoute` with a `SerializedSource` built where `T` is known: `subscribe()` returns a reader whose `recv()` yields the serialized payload directly (subscribe → recv → serialize all typed inside). The `Serializer::Raw`/`Serializer::Context` split ([connector.rs:122](../../aimdb-core/src/connector.rs#L122)) collapses into what the closure captures.
- **Inbound:** replace deserializer + `produce_any` with an ingest closure `Fn(&[u8], &ConsumeContext) -> Future<DbResult<()>>` capturing the typed producer and deserializer.
- **Join:** optional in scope. The erasure is confined to one module and wired once; fuse it with the same trick only if it falls out naturally, otherwise leave it and document why.

**Acceptance criteria.**
- No `Box<dyn Any>` is constructed on any per-message path (connectors, session pump, AimX client).
- `grep -rnE "dyn (core::any::)?Any\b" aimdb-core/src` hits only: [`ExtensionMap`](../../aimdb-core/src/extensions.rs#L32) (TypeId-keyed map — the standard pattern), `AnyRecord::as_any`/`as_any_mut` and `DynBuffer::as_any` (one-time typed-handle resolution at setup), the session `PeerInfo`/`SessionCtx` auth `ext` slots (per-*connection*, set once at authenticate/open), and — if left — join internals. (The latter two were missed by the original list because the literal `"dyn Any"` grep doesn't match `dyn core::any::Any`; all verified setup-time during implementation.)
- All six connectors and `aimdb-pro` compile against the new shape with no behavior change; the fake-gateway and session smoke tests pass unmodified.

**Risk notes.** Object-safe async readers are already solved in [connector.rs](../../aimdb-core/src/connector.rs) (manual boxed-future pattern — keep it). Context-aware serializers (026) need the `ConsumeContext`/`RuntimeContext` threaded as an argument rather than captured. This is a breaking change to the connector SPI; the window is already open (#131/#135 are breaking), so it should land in the **same release** as #140 if at all possible — otherwise it waits for the next breaking window.

**Size:** L (the largest remaining core item). **Status:** implemented in PR [#141](https://github.com/aimdb-dev/aimdb/pull/141) (no separate issue — went straight to PR in the post-#140 breaking window; ingest closure decided **sync**, `RuntimeContext` threaded in place of the sketched `ConsumeContext`, join left erased per the optional-scope note).

### W2 — Split the `AnyRecord` god-trait (034 §3.8)

**Current state (verified).** [`AnyRecord`](../../aimdb-core/src/typed_record.rs#L210) still has ~25 methods spanning storage/lifecycle (`validate`, `as_any`, `as_any_mut`, `drain_config_errors`, `set_writable_erased`), graph introspection (`outbound_connector_*`, `inbound_connectors`, `consumer_count`, `has_producer`/`has_buffer`/`has_transform`, `record_origin`, `buffer_info`, `transform_input_keys`, `collect_metadata`), cfg-gated JSON remote access (`latest_json`, `subscribe_json`, `set_from_json`), and cfg-gated profiling/metrics resets.

**Approach.** Split by consumer, wire with supertraits + dyn upcasting (stable since Rust 1.86; the workspace toolchain is 1.95):

```rust
pub trait AnyRecord: RecordIntrospect + Send + Sync { /* storage + lifecycle only */ }
pub trait RecordIntrospect { /* graph + metadata — consumed by graph.rs, tools */ }
#[cfg(feature = "json-serialize")]
pub trait JsonRecordAccess { /* latest_json / subscribe_json / set_from_json */ }
```

The registry keeps storing `Box<dyn AnyRecord>`; consumers upcast to the capability they need (`&dyn RecordIntrospect`) or query JSON access via an `Option<&dyn JsonRecordAccess>` accessor so the cfg-gate lives in one place. Profiling/metrics resets become default-implemented methods on a small sub-trait rather than cfg-noise on the core trait.

**Payoff:** the core storage contract stops churning every time remote access or profiling evolves, and each consumer's dependency is visible in its signature. **Acceptance:** `AnyRecord` ≤ ~8 methods; no behavior change; rustdoc for each trait states its consumer.

**Size:** M. Mechanical but wide. **File together with W1** (it touches the same files; do W2 first or in the same series — W2 shrinks the surface W1 has to move). **Status:** implemented in PR [#142](https://github.com/aimdb-dev/aimdb/pull/142), stacked on #141 (W1 had already landed, so the "W2 first" ordering note was moot). Deviations from the sketch: the JSON trait's gate is `remote-access` — the actual gate on those methods — not `json-serialize`; the resets live on a `RecordMetricsReset` supertrait (default no-ops, so the supertrait list needs no cfg); the dead `outbound_connector_urls` (cfg `std`, zero callers in-tree and in aimdb-pro) was dropped rather than moved. Result: `AnyRecord` has 6 methods.

### W3 — Execute the KNX hardware validation matrix (035 §3)

**Current state.** The 2.1 select-loop restructure and the fix-round behavior changes (heartbeat-response timeout, backoff socket pacing, send-failure untracking) are in PR #140, validated on host against the fake gateway only. Hardware (Nucleo-H563ZI + KNX/IP gateway) is available; the seven-scenario matrix and pass criteria are specified in [035 §3](035-review-followups-deferred.md) and are not duplicated here.

**This is the gate for closing the 035 loop** — ideally run before #140 merges (one bench session); at minimum before the next release that ships the KNX connector. Outcome feeds W4 (scenario 1's AckTimeout observations size the retransmit knob).

**Size:** S (one bench session). No issue needed if run as part of #140; otherwise file as a validation task.

### W4 — KNX ACK-retransmit knob in `TunnelConfig` (from the #135 review)

**Current state (verified).** When a `TunnelingRequest` is not ACKed within the timeout, the engine expires the pending slot and emits [`Action::AckTimeout`](../../aimdb-knx-connector/src/tunnel.rs#L94) (shims log a warning) — no retransmit, no disconnect. The KNXnet/IP tunneling spec (3.8.4) says: retransmit once after 1 s, then tear the connection down on the second miss.

**Approach.** `TunnelConfig` gains `ack_retransmits: u8` (default `1` = spec-conformant; `0` = today's expire-and-log for RAM-constrained builds). Design constraint to resolve in the issue: retransmission needs the frame bytes at expiry time, which means either (a) buffering the sent frame in the pending-ACK slot — 278 bytes × `PENDING_ACK_CAPACITY` extra RAM on MCU, interacting directly with the 035 §2.2 inline-frame decision — or (b) storing the semantic content (cEMI payload) and rebuilding the frame at retransmit time, which is the "semantic actions" alternative 035 §2.2 already documents. Decide (a) vs (b) once, in the issue, with the 035 §2.2 trade-off in view; gate the buffer behind `ack_retransmits > 0` either way. On final timeout, follow the spec: emit a disconnect/reconnect action rather than only warning.

**Validation:** host fake-gateway test that drops the first ACK; hardware scenario 1/3 from the 035 matrix re-run.

**Size:** S–M. File after #140 merges (touches `tunnel.rs` on the #140 baseline).

### W5 — `StringKey::intern`: dedup interner + loud contract (034 §3.10)

**Current state (verified).** [`StringKey::intern`](../../aimdb-core/src/record_id.rs#L284) still `Box::leak`s every call ([record_id.rs:297](../../aimdb-core/src/record_id.rs#L297)); re-interning the same key leaks again; guarded only by a debug-build counter (cap 1000).

**Approach.** Keep the `&'static str`/`Copy` design (it is what makes `RecordKey` free to pass around and is correct for the static-key embedded path). Add a global dedup table consulted by `intern` (std: `std::sync::Mutex<BTreeSet<&'static str>>`; no_std+alloc: `spin::Mutex` — the crate already depends on `spin` for the no_std lock in typed_record). Result: interning the same key twice returns the same allocation, making the leak bounded by the number of *distinct* dynamic keys — which is the actual lifetime contract of a record key in a long-lived process. Document exactly that contract on `intern` ("each distinct dynamic key allocates once for process lifetime; do not derive keys from unbounded input"), and keep the debug counter as a tripwire on *distinct* keys.

Explicitly rejected: a non-`Copy` `Arc<str>` key variant — it forks `RecordKey` into two shapes, which is the 034 §3.1 mistake again.

**Size:** S. Independent of everything else; opportunistic.

### W6 — `host_test_stubs!` macro for the defmt logger duplication (035 §2.4)

**Current state (verified).** The no-op `#[defmt::global_logger]`/panic-handler/time-driver block exists in three places: [session_smoke.rs](../../aimdb-embassy-adapter/tests/session_smoke.rs), [buffer.rs](../../aimdb-embassy-adapter/src/buffer.rs) (test module), [embassy_smoke.rs](../../aimdb-serial-connector/tests/embassy_smoke.rs) — the third copy that 035 named as the trigger already exists.

**Approach.** As specified in 035 §2.4: `#[macro_export] #[doc(hidden)] macro_rules! host_test_stubs` in `aimdb-embassy-adapter`, expanded once per test binary; delete the three copies and the serial-connector's standalone `defmt` dev-dependency if nothing else needs it. **Size:** S. Do it the next time any of those test files is touched, or fold into the W1/W2 series.

### W7 — `aimdb-data-contracts` trait audit (034 §3.8, last unhandled row)

**Current state (verified).** The crate still exports `SchemaType`, `Simulatable`, `Settable`, `Observable`, `Linkable`, `MigrationStep`, `MigrationChain`, `Streamable`; consumers remain the wasm adapter, the websocket connector, and the weather demo. Which traits have implementors outside examples has never been audited.

**Approach.** One-time audit: for each trait, list in-tree implementors and external consumers (aimdb-pro included). Traits with zero non-demo implementors get a deprecation note or deletion in the next breaking window — per 034 root-cause 7, speculative surface is the habit to break, not an emergency. **Size:** S (audit) + S (deletions). Output is a short table appended to this doc or the issue.

---

## 3. Assessment-only items (decision docs, not code)

### A1 — MQTT consolidation review (034 §3.7)

Still two unrelated client stacks: [tokio_client.rs](../../aimdb-mqtt-connector/src/tokio_client.rs) (396 lines, `rumqttc`) and [embassy_client.rs](../../aimdb-mqtt-connector/src/embassy_client.rs) (491 lines, forked `mountain-mqtt`). Unlike KNX, the duplicated state machine lives inside third-party clients, so the sans-io extraction that worked for #135 does not transfer directly. The review should weigh exactly three options and pick one: **(a)** keep both clients, extract only the aimdb-side glue that demonstrably drifts (topic/route mapping, reconnect policy, payload plumbing) into a shared module; **(b)** adopt one no_std-capable client on both runtimes (viability hinges on the mountain-mqtt fork's health — see the accepted 034 §3.9 fork trade-off); **(c)** sans-io MQTT engine in-tree — almost certainly rejected (re-implementing an MQTT client is not this project's job). Deliverable: a decision section in this doc or a short 03x doc; code only follows if (a) or (b) wins. **Trigger for urgency:** the next bug fixed twice, or the next mountain-mqtt fork rebase that hurts.

### A2 — AimX / WS-JSON protocol convergence (034 §3.10, root cause 10)

Both protocols now ride the session engine (the hard part), but two subscribe/write/query vocabularies, two codecs, and two error mappings remain. Scheduled work, not opportunistic: belongs to the **next protocol-breaking release**. First deliverable is a mapping table (AimX message ↔ ws-protocol message ↔ semantics) and a shared envelope/error-vocabulary proposal; only then decide whether full convergence pays. No issue until a protocol-breaking release is actually planned.

---

## 4. Dormant items — trigger-only, no action (restated from 035 so this doc is the single live list)

| Item | Decision | Re-open trigger |
|---|---|---|
| `Action::Send` 278-byte inline frame (035 §2.2) | Keep — stack-only datagram path, queue depth 0–2 in practice | MCU profiling shows the memcpy or queue slot footprint in a flame graph; or W4 chooses option (b), which partially supersedes this |
| Buffer-ext trait triplication tokio/embassy/wasm (035 §2.3) | Keep — adapter-specific by design; E0034 hazard verified absent in-tree | A real dual-adapter binary appears → introduce `BufferFactory<T>` in `aimdb-executor` then |
| `aimdb-codegen` 2,234-line string-template generator (034 §3.8) | Keep — doc rot fixed in the #140 round; the generator works | The next API change that forces a multi-day template chase → consider generating against a stable facade or trimming targets |
| Vendored forks + monorepo examples (034 §3.9) | **Decided 2026-06-09: accepted as-is** | Not a work item; do not re-propose |

---

## 5. Sequencing and tracking

1. **Merge PR #140.** Everything in §2 assumes its baseline.
2. **W3** (hardware matrix) — one bench session, ideally pre-merge; closes 035.
3. **W2 → W1** as one series in the still-open breaking window (W2 shrinks what W1 moves). These are the two issues 034 said to file "after #131 merges" — file both when #140 lands.
4. **W4** after #140, validated against W3's baseline traces.
5. **W5, W6, W7** opportunistic — fold into whatever touches the neighborhood.
6. **A1/A2** when their triggers fire; no code before a written decision.

| Item | Issue | When to file |
|---|---|---|
| W1 data-plane de-`Any` | PR [#141](https://github.com/aimdb-dev/aimdb/pull/141) | done — no separate issue, direct PR |
| W2 `AnyRecord` split | PR [#142](https://github.com/aimdb-dev/aimdb/pull/142) | done — stacked on #141, no separate issue |
| W3 hardware matrix | — | none if run with #140; else a validation task |
| W4 ACK-retransmit knob | — | on #140 merge |
| W5 `StringKey` interner | — | opportunistic; file if not done by next release |
| W6 `host_test_stubs!` | — | opportunistic |
| W7 data-contracts audit | — | opportunistic |
| A1 / A2 | — | on trigger only |

Update this table with issue numbers as they are filed; when every row is filed or closed, this doc's status moves to Final and the live list is the issue tracker again.
