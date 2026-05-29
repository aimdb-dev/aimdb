# Phase 0 — frozen connector-session contracts (decision record)

**Version:** 1.0 (ratified)
**Status:** 🟢 Decided
**Realizes:** [036 — Masterplan](036-remote-access-masterplan.md) Phase 0
**Depends on:** [033 — Remote access as a connector](033-remote-access-as-connector.md), [034 — Server-connection trait](034-server-connection-trait.md), [035 — Connector capability model](035-connector-capability-model.md), [M16 — AimX JSON codec](../032-M16-aimx-json-codec.md)
**Implements:** [`aimdb-core/src/session.rs`](../../../aimdb-core/src/session.rs) (feature `connector-session`)
**Last Updated:** May 29, 2026
**Milestone:** M17+ (connector convergence)

---

## TL;DR

This record **freezes the cross-cutting contracts** every later phase consumes, so Phases 2–6 never rework signatures mid-flight. It ships two things:

1. **Four cross-cutting decisions** (below), each resolved with rationale and explicit deferrals.
2. **`dyn`-safe trait skeletons** — signatures only, `unimplemented!()` bodies — in [`aimdb-core/src/session.rs`](../../../aimdb-core/src/session.rs) behind the `connector-session` feature, compiling on `std` **and** `no_std + alloc` (`thumbv7em-none-eabihf`).

No engine logic, no pumps, no transport impls, no wire-protocol changes — **contracts, not behavior**.

---

## Decisions

### Decision 1 — `Payload` seam type → **raw bytes** ✅

`Payload = Arc<[u8]>` — opaque serialized bytes, the interchange between the outer `EnvelopeCodec` (protocol frame) and the inner M16 record-value `JsonCodec` (record value).

**Rationale.** Cheap-clone (refcount bump) for WS fan-out, `no_std + alloc`-native, no new dependency. The alternative (`serde_json::Value`) is zero-churn for std ports but couples the envelope to `serde_json` and forces a `Value` tree on the hot paths. Raw bytes keep **one** serde pass on the hot paths; a JSON tree materializes only where an RPC handler inspects structure.

**Convert-boundary rule** (the discipline this type enforces):
- *emit:* `T → bytes` once via `serde_json::to_vec` / `serde-json-core` — no intermediate `Value` tree.
- *ingest:* `bytes → T` once via `from_slice`.
- *pass-through:* streaming a record to subscribers and routing source→producer never touch the payload.
- *structured:* a `serde_json::Value` materializes only inside RPC handlers that inspect structure (query filters, graph introspection) — never on the streaming/produce loops.

**EnvelopeCodec implication.** `decode` yields `params`/`data` as an *unparsed* `Payload` (a slice of the frame); `encode` splices a `Payload` in verbatim. Embedding raw JSON bytes inside a textual NDJSON/WS-JSON envelope will need `serde_json::value::RawValue` (std) or a manual fixed-buffer splice (`serde-json-core`) to avoid re-escaping — an implementation concern for the Phase 2+ codecs, not a contract change.

**Future option.** `bytes::Bytes` only if cheap sub-slicing / zero-copy binary framing is later needed; `Arc<[u8]>` is the no-dependency default until then.

### Decision 2 — RPC + streaming unify in one `Dispatch` → **one trait** ✅

A single `Dispatch` trait carries all three reply cardinalities: `call` (one reply) / `subscribe` (many) / `write` (none). They differ only by return type, not by abstraction.

**Rationale.** Both existing stacks (AimX-remote, WS connector) already interleave RPC and streaming over **one** connection via a `biased select!`. A subscription is just a method whose reply is a `Stream` rather than a single value. Forking into separate RPC and streaming traits only pays off with separate transports, which AimDB does not have.

### Decision 3 — `publish` placement → **sibling capability** ✅ (ratify)

`publish` stays a sibling data-plane capability, **not** absorbed into the session trait. `Sink` = today's [`Connector`](../../../aimdb-core/src/transport.rs#L159) contract **verbatim** — no rename, no migration in Phase 0.

**Rationale.** Already decided in [033 §5](033-remote-access-as-connector.md); `publish` is the client/sink role, the session trait is the server role. MQTT *inbound* is the degenerate session case (a single `Connection`, no `Listener`). Reconciling the `Sink`/`Connector` names is **Phase 1**, not here.

### Decision 4 — embedded engine order → **client-first** ✅

The embedded push target is *an MCU dials a gateway and pushes records*, so the **Client** path (`Dialer` + `run_client`) is the embedded-critical one.

**Rationale.** The smallest engine — one connection, no accept loop, no fan-out — gives the best odds against #39's ~60–100 KB / ≥256 KB RAM budget. This orders the later phases, not the Phase 0 contracts: Phases 2–4 still build **both** sides (the substrate is role-neutral); Phase 5 does substrate-first then `run_client`; Phase 6 ships the `Dialer` half first. The frozen substrate (`Connection` / `EnvelopeCodec` / `Inbound` / `Outbound`) is deliberately role-neutral so server and client share it.

---

## Deferred — recorded here, resolved elsewhere

| deferral | lands in |
|---|---|
| Auth-context shape — one `SessionCtx` for AimX `SecurityPolicy` + WS `Permissions`? (`SessionCtx`/`PeerInfo` are opaque placeholders for now) | **Phase 4** |
| `Source` cardinality + backpressure (one multiplexed `Source` per scheme; block vs drop+count) | **Phase 1** |
| Client surface — `pump_client` mirroring-only vs caller RPC (`request_id_counter`) | ✅ **Phase 2** — *resolved: one engine, both. `run_client`+`ClientHandle` shipped; the caller-RPC half landed in **Phase 3** as `AimxConnection` ([038](038-phase3-aimx-client.md)); `pump_client` mirroring deferred to the server port (its route-collection deps share the server `Dispatch` machinery)* |
| Envelope wire convergence — NDJSON vs WS-JSON (the codec is pluggable) | deferrable past **Phase 4** |
| Bounded-resource policy — `heapless`/const-generic vs runtime config (`SessionLimits` is a stub) | **Phase 5** |
| Memory budget validation against #39 target | **Phase 7** |

---

## The frozen contract sheet

All in [`aimdb-core/src/session.rs`](../../../aimdb-core/src/session.rs), feature `connector-session`. Signatures copied verbatim from their canonical sketches — transport + `EnvelopeCodec` + `Dispatch` from [034 § The three layers](034-server-connection-trait.md), `Sink` / `Source` / `Dialer` from [035 § The toolkit](035-connector-capability-model.md).

**Shared aliases & types**
- `BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>`, `BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>`
- `Payload = Arc<[u8]>` (Decision 1)
- `SessionCtx`, `PeerInfo` — opaque placeholders (auth deferred to Phase 4); `SessionLimits` — stub (Phase 5)
- `Inbound` / `Outbound<'a>` — role-neutral logical message set (field types align with the existing AimX wire in `remote::protocol`: `id`/`seq` are `u64`, `topic`/`sub`/`method` are `String`/`&str`)
- supporting errors: `TransportError` (+ `TransportResult`), `CodecError`, `RpcError`, `AuthError`

**Transport (Layer 1)** — `Connection` (`recv`/`send`/`peer`), `Listener` (`accept`), `Dialer` (`connect`, the dual of `Listener`).

**Dispatch (Layer 3)** — `Dispatch` (`authenticate`/`open`) + `Session` (`call`/`subscribe`/`write`), `EnvelopeCodec` (`decode`/`encode`).

> **Phase 3 server-port refinement (additive).** The dispatch role was **split** into a shared `Dispatch` (`Send + Sync`, one `Arc<dyn Dispatch>` per server: `authenticate` + an `open(&SessionCtx) -> Box<dyn Session>` factory) and a per-connection `Session` (`Send`, one `Box<dyn Session>` per accepted connection: `call`/`subscribe`/`write` on `&mut self`). The engine (`run_session`) owns the `Box<dyn Session>` and threads `&mut` into it, so a connection can hold mutable dispatch state — `record.drain`'s lazy per-record cursors today, per-session auth identity in Phase 4 — without a lock. This is the **one seam the AimX wire reshape did not dissolve** (it is about `Dispatch` being a shared `&self`, not the wire). `Session::subscribe` is **defaulted** to `Err(RpcError::NotFound)` since its stream is `'static` (it captures cloned handles) and so is side-neutral. The split is additive and object-safe (the Phase-0 `Box<dyn …>` object-safety tests still hold, now covering `Session`); it mirrors the precedent of the Phase-2 `encode_inbound`/`decode_outbound` addition below — made when the server port was built, the phase where per-connection dispatch state is nailed down.

> **Phase 2 refinement (additive).** `EnvelopeCodec` gained its *client* direction — `encode_inbound` + `decode_outbound` — so the proactive `run_client` engine reuses the **same** codec object as the reactive server (the role-neutral-substrate invariant). The two Phase 0 server-direction signatures are unchanged; this is a pure addition, made when the client engine was built (the engine phase is where the client codec direction is nailed down).

**Data-plane (035)** — `Sink` (`publish` = today's `Connector` verbatim), `Source` (`next`).

### Faithful-translation notes (where a verbatim copy needed a concrete choice)

- **Transport frame type.** 034 sketches `Connection::recv -> Option<Bytes>`. Per Decision 1 (no `bytes` crate) the owned transport frame is `Vec<u8>`; `EnvelopeCodec::decode(&[u8])` borrows it. `Payload = Arc<[u8]>` is reserved for the *record-value* seam, not raw transport frames.
- **`Sink::publish` lifetimes.** Match today's `Connector::publish` exactly: the returned `BoxFut<'_, …>` is tied to `&self` only (params get independent elided lifetimes). The new session traits keep 034's `<'a>` form where the future borrows params (`call`/`write`/`authenticate`/`send`).
- **`: Send` on `Dialer` / `Source`.** Added for consistency with the rest of the boxed-future family (they are driven inside the runner's `FuturesUnordered`); 035's sketch omitted the bound.

---

## Acceptance criteria — status

- [x] `037-phase0-contracts.md` exists, with a resolution + rationale for Decisions 1–4 and a "deferred to Phase N" line per deferral.
- [x] Trait skeletons for all Scope types compile on `std` (`cargo check -p aimdb-core --features connector-session`).
- [x] Same skeletons compile for `no_std + alloc` (`cargo check -p aimdb-core --target thumbv7em-none-eabihf --no-default-features --features "alloc,connector-session"`).
- [x] Every trait is object-safe — `_assert_object_safe(&dyn …)` (all targets) + `traits_are_object_safe` test building `Box<dyn …>` per trait.
- [x] Skeletons are unimplemented (`unimplemented!()`) — contracts, not behavior.
- [x] [036](036-remote-access-masterplan.md)'s decision-gate table marks the Phase-0 rows resolved.
```
