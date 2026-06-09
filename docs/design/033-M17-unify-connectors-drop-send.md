# Unify Embassy Connectors — Centralized Adapter Spine (revised from "Drop `Send`")

**Version:** 0.2
**Status:** ✅ Implemented — **via the centralized-spine approach, not the `Send`-drop** (see Implementation Decision)
**Predecessor:** [Design 028 — Remove `Spawn` Trait](028-M13-remove-spawn-trait.md) (✅ Implemented)
**Motivating beneficiary:** [#121 — `aimdb-tcp-connector` (tokio + embassy-net)](https://github.com/aimdb-dev/aimdb/issues/121)
**Last Updated:** June 9, 2026
**Milestone:** M17 — Architectural clean-up

---

## Implementation Decision (supersedes the proposal below)

The original proposal — *drop `Send` from the connector/future contract* — was **not**
implemented. Investigation found it forces the `!Send`-ness onto the std/Tokio side
(the proposal's own goal of "keep `Send` on data") and that its §5 mechanism is unsound:

- `run_client`'s engine drives a `Box<dyn Connection>` (from a runtime-resolved
  `Box<dyn Dialer>`) across `.await`, so it becomes **unconditionally `!Send`** once
  `Connection: Send` is dropped — the "auto-`Send` named engine" cannot recover it, and
  the associated-`Conn`-type fix breaks `Box<dyn Dialer>` object-safety.
- `run_session` is **inherently `!Send`** (its subscription-pump `FuturesUnordered<BoxFut>`),
  so the WebSocket server would lose axum's multi-thread per-connection parallelism, and
  `tokio::spawn(runner.run())` would break across ~37 app/test sites.

**Chosen instead (the "centralized Embassy wrapper" alternative this doc had rejected):**
keep the std contract exactly as-is and confine all the Embassy single-core `unsafe` +
`SendFutureWrapper` into **one audited place**, `aimdb-embassy-adapter::connectors`:

- **Session transports** (serial, later TCP) contribute a `Framer` (or a `Connection`)
  and wrap it in `EmbassySessionClient` / `EmbassySessionServer` (+ `OneShotDialer` /
  `OneShotListener` / `OneShotCell` / the framed `EmbassyConnection`).
- **Data-plane transports** (MQTT, KNX) contribute an `EmbassySinkRaw` / `EmbassySourceRaw`
  (or, where their channels are already `Send`, a plain `Connector`/`Source`) and ride
  core's existing `pump_sink` / `pump_source` via the `EmbassySink` / `EmbassySource`
  bridges; their protocol task is force-`Send`ed once via `into_box_future`.

**Outcome:** the serial Embassy half dropped from a 407-line hand-roll (7 `unsafe impl`s) to
thin sugar with **zero `unsafe`**; the MQTT/KNX Embassy halves deleted their hand-rolled
pump loops and `SendFutureWrapper` use, riding core's pumps; **all** connector-crate
`unsafe`/`SendFutureWrapper` is gone (confined to the adapter). The std/Tokio side,
`aimdb-client`, the WS server, examples, tests, and `aimdb-pro` are **unchanged**.

> The sections below are the original (rejected) proposal, kept for the motivation and
> architecture audit, which remain accurate.

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
  - [Every Embassy connector hand-rolls what Tokio reuses](#every-embassy-connector-hand-rolls-what-tokio-reuses)
  - [The cost scales with every new transport](#the-cost-scales-with-every-new-transport)
- [Current Architecture — Audit](#current-architecture--audit)
  - [The runner is already single-task cooperative](#the-runner-is-already-single-task-cooperative)
  - [Where `Send` propagates](#where-send-propagates)
  - [Where `Send` is actually exercised](#where-send-is-actually-exercised)
  - [The Embassy workaround today](#the-embassy-workaround-today)
- [Key Insight — `Send` is a boxing-boundary decision](#key-insight--send-is-a-boxing-boundary-decision)
- [Proposed Design](#proposed-design)
  - [1. Drop `Send` from the future/transport contract](#1-drop-send-from-the-futuretransport-contract)
  - [2. Keep `Send` on identity and data](#2-keep-send-on-identity-and-data)
  - [3. The runner becomes `!Send`](#3-the-runner-becomes-send)
  - [4. Unify the generic session connectors](#4-unify-the-generic-session-connectors)
  - [5. Preserve direct-spawn for `aimdb-client`](#5-preserve-direct-spawn-for-aimdb-client)
- [Type-System Changes](#type-system-changes)
- [Platform-Specific Concerns](#platform-specific-concerns)
- [Affected Crates](#affected-crates)
- [Breaking Changes & Migration](#breaking-changes--migration)
- [Alternatives Considered](#alternatives-considered)
- [Implementation Plan](#implementation-plan)
- [Decisions](#decisions)
- [Out of Scope](#out-of-scope)

---

## Summary

The generic, transport-agnostic session connectors (`SessionClientConnector` /
`SessionServerConnector` in `aimdb-core/src/session/connector.rs`) let a Tokio
transport crate contribute just a `Dialer`/`Listener`/`Connection` triple and a
codec, and inherit all the engine wiring (reconnect, pumps, accept loop,
fan-out). The UDS and serial **Tokio** halves are one-line sugar over them.

The **Embassy** half of every connector cannot use that spine. It re-implements
`ConnectorBuilder` by hand — calling the bare engines (`run_client`, `serve`,
`pump_client`) directly and force-`Send`ing every future with
`aimdb-embassy-adapter`'s `SendFutureWrapper` plus a sprinkle of
`unsafe impl Send`/`Sync` on the transport, dialer, listener, and builder types
(see `aimdb-serial-connector/src/embassy_transport.rs`).

The root cause is a single, deliberate choice: **the connector contract is
`Send`-everywhere.** `ConnectorBuilder::build` returns
`Vec<Pin<Box<dyn Future + Send>>>`, and `Connection`/`Listener`/`Dialer` and the
`BoxFut`/`BoxStream` aliases all carry `Send`. That bound was inherited from the
Tokio-shaped, multi-threaded execution model. Embassy is single-core
cooperative; its primitives and HAL peripherals are `!Send` *by design*.

This design **drops `Send` from the future/transport contract** so that one
connector implementation per transport serves both runtimes — exactly as the
Tokio halves already do — while keeping `Send` on the *data and identity* types
(`RuntimeAdapter`, the type-erased `Arc<dyn Any + Send + Sync>` payloads,
`JoinQueue<T: Send>`).

The change is sound because **AimDB already drives every future on a single
cooperative task** ([`AimDbRunner::run()`](../../aimdb-core/src/builder.rs)
collects everything into one `FuturesUnordered`). The `Send` bound is never
exercised by AimDB's own machinery — it is consumed only when an application
hands the *whole* aggregate task to a multi-threaded runtime via
`tokio::spawn(runner.run())`. That single ergonomic — `tokio::spawn` of the
runner — is the only thing we give up, and it has a one-line migration.

---

## Motivation

### Every Embassy connector hand-rolls what Tokio reuses

Today the same transport ships two structurally different implementations:

| Transport | Tokio half | Embassy half |
|---|---|---|
| UDS / serial | ~1-line sugar over `SessionClientConnector` / `SessionServerConnector` | full hand-rolled `ConnectorBuilder` + `SendFutureWrapper` + `unsafe impl Send/Sync` × 4 |
| MQTT / KNX | thin wrapper | hand-rolled pump loops (can't ride `pump_sink`/`pump_source`) |

The Embassy `embassy_transport.rs` for serial even carries a dedicated
doc comment titled *"Why this half hand-rolls `ConnectorBuilder`"*. The hand-roll is
a mechanical translation of the Tokio sugar:

- `D: Clone` (clone the dialer out of `&self`) → `RefCell<Option<_>>::take()`
- `Send + Sync` bounds → `unsafe impl Send/Sync` justified by the single-core
  invariant
- `Send` futures → wrap every `recv`/`send`/engine future in `SendFutureWrapper`

None of this is transport logic. It is per-crate boilerplate that re-asserts the
same single-core safety argument, each copy an independent place for that
`unsafe` to drift.

### The cost scales with every new transport

[#121 (`aimdb-tcp-connector`, tokio + embassy-net)](https://github.com/aimdb-dev/aimdb/issues/121)
is the next connector in the queue. Under the current design it will need a fully
hand-rolled Embassy half — a fourth copy of the `SendFutureWrapper` + `unsafe`
pattern — for what is, on Tokio, a few lines over the existing spine. Every
future transport (CoAP, BLE, CAN, …) pays the same tax. This design pays it down
once.

This is also the **unfinished half of [Design 028](028-M13-remove-spawn-trait.md)**.
M13 removed `Spawn` and made `build()` *collect* futures into a `Vec<BoxFuture>`
driven by `run()`. It deliberately left the `Send` bound on that `BoxFuture` in
place. With `Spawn` gone, that residual `Send` is now the *only* thing forcing
Embassy connectors to diverge.

---

## Current Architecture — Audit

### The runner is already single-task cooperative

[`AimDbRunner::run()`](../../aimdb-core/src/builder.rs) collects **every** future
the database needs — connectors, producer/consumer loops, transforms, the remote
supervisor, `on_start` tasks — into one `FuturesUnordered` and polls them on one
task:

```rust
let mut set: FuturesUnordered<BoxFuture> = self.futures.into_iter().collect();
while set.next().await.is_some() {}
```

Nothing in `aimdb-core` ever `tokio::spawn`s these futures individually. The
session engines themselves fan out per-connection and per-subscription with
*nested* `FuturesUnordered` + `select_biased!`
(`aimdb-core/src/session/server.rs`), never a spawn. So:

> The entire connector / producer / consumer surface is **already** cooperative
> on a single task on every runtime. `Send` buys *relocation of that one
> aggregate task onto a worker thread* — it does **not** buy intra-database
> parallelism, because two connectors are never polled simultaneously.

### Where `Send` propagates

| Location | Bound | Role |
|---|---|---|
| `aimdb-core/src/builder.rs` | `type BoxFuture = Pin<Box<dyn Future<Output=()> + Send + 'static>>` | the runner's collected future type |
| `aimdb-core/src/connector.rs` | `trait ConnectorBuilder<R>: Send + Sync` | connector entry point |
| `aimdb-core/src/connector.rs` | `build(..) -> Pin<Box<dyn Future<Output=DbResult<Vec<Pin<Box<dyn Future + Send>>>>> + Send>>` | build returns a `Send` Vec of `Send` futures |
| `aimdb-core/src/session/mod.rs` | `type BoxFut<'a,T> = Pin<Box<dyn Future + Send + 'a>>` | every transport/dispatch async return |
| `aimdb-core/src/session/mod.rs` | `type BoxStream<'a,T> = Pin<Box<dyn Stream + Send + 'a>>` | subscription stream |
| `aimdb-core/src/session/mod.rs` | `trait Connection: Send`, `Listener: Send`, `Dialer: Send`, `Source: Send` | Layer-1 transport |
| `aimdb-core/src/session/mod.rs` | `trait Dispatch: Send + Sync`, `Session: Send`, `EnvelopeCodec: Send + Sync` | Layer-3 dispatch |
| `aimdb-core/src/session/connector.rs` | `D: Dialer + Clone + Send + Sync`, `LF/DF: Fn(..) + Send + Sync` | generic connector bounds |

### Where `Send` is actually exercised

Across the whole workspace, the `Send` bound on the connector/runner contract is
*consumed* (i.e. a future is actually moved to another thread) in exactly one
shape — `tokio::spawn` of the runner or a directly-driven engine:

| Site | What is spawned |
|---|---|
| `examples/remote-access-demo/src/server.rs` | `tokio::spawn(runner.run())` |
| `examples/weather-mesh-demo/weather-station-{alpha,beta}/src/main.rs` | `tokio::spawn(runner.run())` |
| `aimdb-websocket-connector/examples/ws_{client,server}.rs` | `tokio::spawn(runner.run())` |
| `aimdb-client/src/engine.rs` | `tokio::spawn(engine_fut)` (the `run_client` engine) |

Every other Tokio example already drives the runner with `builder.run().await`
(`examples/tokio-{mqtt,knx}-connector-demo`), which needs **no** `Send` —
`block_on` never requires it.

### The Embassy workaround today

`aimdb-embassy-adapter::SendFutureWrapper` is a `struct SendFutureWrapper<F>(F)`
with `unsafe impl<F> Send for SendFutureWrapper<F> {}`, justified by the
single-core, no-preemption, no-thread-migration Embassy invariant. Each Embassy
connector wraps every `recv`/`send`/`connect`/`accept`/engine future in it and
adds `unsafe impl Send`/`Sync` to the connection, dialer, listener, and builder
structs. This is the entire delta between the Tokio and Embassy halves.

---

## Key Insight — `Send` is a boxing-boundary decision

A future is `Send` iff every value held across an `.await` is `Send`. On Tokio,
all transport values are `Send`, so the **concrete** engine future *is* `Send`.
It only loses that property when it is **boxed into a `dyn Future` whose type
does not advertise `Send`**.

Therefore `Send` is not a property we must thread through the engines — it is a
single decision made at each `dyn`-boxing boundary:

1. **`ConnectorBuilder::build` → the runner.** The returned `Vec<BoxFuture>` is a
   `dyn` box. This is the *one* uniform boundary every connector funnels
   through, so its `Send`-ness is what unifies (or splits) the connectors. This
   box **must drop `Send`** to admit Embassy.
2. **Direct engine consumers** (`aimdb-client`). These don't go through the
   runner; they hold the concrete engine future. If we keep that future's type
   `Send`-transparent (generic / un-pre-boxed), std callers keep `tokio::spawn`.

Once boundary (1) drops `Send`, the runner is `!Send` and nothing downstream
needs `Send` — a `FuturesUnordered<F>` polls `F: !Send` perfectly well on one
thread.

---

## Proposed Design

### 1. Drop `Send` from the future/transport contract

Remove `+ Send` / `Send` supertrait from:

- `BoxFuture` (`builder.rs`) and the `ConnectorBuilder::build` return type
  (both the outer build future and the inner `Vec` element).
- `ConnectorBuilder<R>` supertrait (`: Send + Sync` → no auto-trait bound).
- `BoxFut`, `BoxStream` (`session/mod.rs`).
- `Connection`, `Listener`, `Dialer`, `Source`, `Session` (drop `: Send`).
- `Dispatch`, `EnvelopeCodec` (drop `: Send + Sync`).

### 2. Keep `Send` on identity and data

Explicitly **unchanged**, because they describe data at rest, not futures in
flight, and removing them would ripple needlessly:

- `RuntimeAdapter: Send + Sync`, `TimeOps` associated `Instant`/`Duration: Send + Sync`.
- The type-erased payloads: `Arc<dyn Any + Send + Sync>` (runtime ctx,
  `PeerInfo::ext`, `SessionCtx::ext`), `Payload = Arc<[u8]>`.
- `JoinQueue<T: Send>` and the transform fan-in channels.

A `!Send` future may freely hold `Send` data; these two facts do not conflict.

### 3. The runner becomes `!Send`

`AimDbRunner` and its `run()` future are now `!Send`. Applications drive it one
of three ways (all already valid on a multi-threaded Tokio runtime):

```rust
// (a) Block on it — simplest, multi-thread runtime is fine (block_on ≠ Send).
runner.run().await;

// (b) Concurrent with other main-task work, single-threaded scope:
let local = tokio::task::LocalSet::new();
local.run_until(async { tokio::task::spawn_local(runner.run()); /* … */ }).await;

// (c) Dedicated current-thread runtime on its own std thread.
std::thread::spawn(|| {
    tokio::runtime::Builder::new_current_thread().enable_all().build()
        .unwrap().block_on(runner.run());
});
```

### 4. Unify the generic session connectors

With the bounds relaxed, `SessionClientConnector` / `SessionServerConnector`
host an Embassy peripheral directly. Two mechanical adjustments replace what the
hand-roll did manually:

- **Client:** store the dialer behind interior mutability and move it out once in
  `build`, dropping the `D: Clone` requirement (the engine already owns the
  dialer for the connection's life and re-dials via `&self`):

  ```rust
  pub struct SessionClientConnector<D, C> {
      scheme: String,
      dialer: RefCell<Option<D>>,   // moved out once in build()
      codec: C,
      config: ClientConfig,
  }

  impl<R, D, C> ConnectorBuilder<R> for SessionClientConnector<D, C>
  where R: TimeOps + 'static, D: Dialer + 'static, C: EnvelopeCodec + Clone + 'static
  { /* build(): take dialer, run_client(..), collect pumps + engine */ }
  ```

- **Server:** keep the listener/dispatch factories, drop their `Send + Sync`
  bound. A once-only listener (a moved-in UART) is captured the same way the
  hand-roll does — `RefCell<Option<_>>` inside the factory closure.

Embassy connectors then collapse to *only* the `Dialer`/`Listener`/`Connection`
triple plus the one-line sugar — identical in shape to the Tokio halves.
`SendFutureWrapper` and every `unsafe impl Send/Sync` in the connector crates are
deleted.

### 5. Preserve direct-spawn for `aimdb-client`

`aimdb-client` calls `run_client` directly and `tokio::spawn`s the returned
engine future (`engine.rs`). `run_client` currently returns
`(ClientHandle, BoxFut<'static, ()>)` — a pre-`Send`-boxed future. If we simply
drop `Send` from `BoxFut`, that `tokio::spawn` stops compiling.

To keep std direct-spawn working *without* reintroducing `unsafe`, give the
engine a **named, generic future type** so its `Send`-ness is inferred from the
inputs rather than erased by the box:

```rust
pub fn run_client<D, C, R>(dialer: D, codec: C, config: ClientConfig, clock: Arc<R>)
    -> (ClientHandle, ClientEngine<D, C, R>)   // was: (ClientHandle, BoxFut<'static, ()>)
where D: Dialer + 'static, C: EnvelopeCodec + 'static, R: TimeOps + 'static;

// ClientEngine<D,C,R>: Future<Output=()>; auto-Send iff D, C, R are Send.
```

- **std** (`aimdb-client`, Tokio transports): `ClientEngine<…>` is `Send`,
  so `tokio::spawn(engine)` keeps compiling — no migration.
- **connector spine** (`SessionClientConnector::build`): boxes `ClientEngine`
  into the runner's now-`!Send` `Vec<BoxFuture>` — the box drops the `Send`
  capability the single-task runner never needed.
- **Embassy**: `ClientEngine` is `!Send`, driven on the cooperative runner. No
  wrapper.

`serve` is already `pub async fn serve(..)` (returns an anonymous `impl Future`),
so its `Send`-ness is inferred at each call site for free — no newtype needed.

---

## Type-System Changes

| Item | Before | After |
|---|---|---|
| `BoxFuture` (builder) | `… + Send + 'static` | `… + 'static` |
| `ConnectorBuilder<R>` | `: Send + Sync` | (no auto-trait supertrait) |
| `build()` return | `Vec<Box<dyn Future + Send>>`, outer `+ Send` | drop both `Send` |
| `BoxFut` / `BoxStream` | `… + Send + 'a` | `… + 'a` |
| `Connection`/`Listener`/`Dialer`/`Source`/`Session` | `: Send` | (none) |
| `Dispatch`/`EnvelopeCodec` | `: Send + Sync` | (none) |
| `SessionClientConnector` `D` | `Dialer + Clone + Send + Sync` | `Dialer` (+ interior-mutability move) |
| `SessionServerConnector` `LF`/`DF` | `Fn(..) + Send + Sync` | `Fn(..)` |
| `run_client` engine return | `BoxFut<'static,()>` | named `ClientEngine<D,C,R>` |
| `RuntimeAdapter`, `Arc<dyn Any + Send + Sync>`, `JoinQueue<T: Send>` | `Send`/`Sync` | **unchanged** |

The dyn-object-safety assertions in `session/mod.rs` (`_assert_object_safe`)
stay valid — removing an auto-trait supertrait only widens the set of impls.

---

## Platform-Specific Concerns

**Tokio (std).** The runner future becomes `!Send`. Apps switch
`tokio::spawn(runner.run())` → `runner.run().await` or a `LocalSet`. Connectors
that internally rely on Tokio's own machinery (e.g. the WS connector's
`axum::serve`, which `tokio::spawn`s its own per-connection tasks) are
unaffected: those are the connector's own `Send` tasks on the ambient runtime,
independent of AimDB's `BoxFuture` type. `aimdb-client` keeps `tokio::spawn` via
the named-engine return (§5).

**Embassy (no_std + alloc).** The target outcome. `SendFutureWrapper` and all
`unsafe impl Send/Sync` in the connector crates are removed. Embassy connectors
reuse `SessionClientConnector` / `SessionServerConnector` and the bare engines
directly. The single-core cooperative invariant that previously *justified* the
`unsafe` now simply *is* the model the type system expresses.

**WASM (wasm32).** Already drives `!Send` futures via `spawn_local`
(`aimdb-wasm-adapter`). This change aligns the connector contract with the model
WASM already uses; the WASM adapter's existing `unsafe impl Send/Sync` on its
adapter becomes reviewable for removal as a follow-up (out of scope here).

---

## Affected Crates

| Crate | Impact |
|---|---|
| `aimdb-core` | bound removals; `SessionClientConnector` interior-mutability; `run_client` named return |
| `aimdb-embassy-adapter` | `SendFutureWrapper` deprecated/removed (kept one release as deprecated shim) |
| `aimdb-serial-connector` | delete `embassy_transport.rs` hand-roll; Embassy half = sugar over the spine |
| `aimdb-uds-connector` | (no Embassy half yet) gains one for free |
| `aimdb-mqtt-connector`, `aimdb-knx-connector` | data-plane Embassy pumps can ride `pump_sink`/`pump_source` once those drop `Send` (follow-up; see Out of Scope) |
| `aimdb-client` | **no source change** if §5 lands; otherwise migrate spawn → `LocalSet` |
| `aimdb-websocket-connector` | unaffected (rides `run_session`; axum owns its spawns) |
| examples (`remote-access-demo`, `weather-mesh-demo`, ws examples) | `tokio::spawn(runner.run())` → `runner.run().await` / `LocalSet` |

---

## Breaking Changes & Migration

1. **`tokio::spawn(runner.run())` no longer compiles.** Migrate to
   `runner.run().await` (most apps) or `LocalSet::spawn_local` (concurrent
   case). One-line change; documented in the runner rustdoc and `CHANGELOG`.
2. **`run_client` return type** changes from `BoxFut` to `ClientEngine<…>`. The
   future is still driven the same way; only the type name changes. Internal to
   the session layer + `aimdb-client`.
3. **Custom connectors** that named `Box<dyn Future + Send>` explicitly in their
   `ConnectorBuilder::build` must drop the `+ Send`. The `BoxFuture` alias path
   is unaffected.

No wire-format, record-API, or `link_to`/`link_from` changes. This is purely a
type-system / execution-model change.

---

## Alternatives Considered

- **Keep `Send`; factor the Embassy wrapper once.** Provide
  `EmbassySessionClient`/`Server` in `aimdb-embassy-adapter` that encapsulate
  `SendFutureWrapper` + `unsafe impl` once. *Rejected:* still two code paths and
  retains `unsafe`; doesn't unify, only de-duplicates the workaround.
- **Conditional `Send` via a feature flag** (two `BoxFuture` aliases gated by
  `send-futures`). *Rejected:* combinatorial trait surface, `cfg`-split bounds
  everywhere, and connectors compiled for the "wrong" flag silently fail to
  compose. Worse than the status quo.
- **`?Send` / maybe-`Send` generics.** Not expressible on stable Rust.
- **De-`Send` the entire executor stack** (`RuntimeAdapter`, channels, payloads).
  *Rejected:* unnecessary — a `!Send` future may hold `Send` data; this would be
  a far larger, riskier blast radius for no additional benefit.

---

## Implementation Plan

1. Land the `run_client` named-engine return (`ClientEngine<D,C,R>`) with the
   `Send`-transparent type; verify `aimdb-client` + CLI + MCP still build and
   `tokio::spawn`.
2. Drop `Send` from `BoxFut`/`BoxStream` and the `Connection`/`Listener`/
   `Dialer`/`Dispatch`/`Session`/`EnvelopeCodec` traits in `session/mod.rs`.
3. Drop `Send` from `BoxFuture` + `ConnectorBuilder` in `builder.rs`/
   `connector.rs`; fix the runner.
4. Relax `SessionClientConnector`/`SessionServerConnector` bounds; switch the
   client to interior-mutability dialer move.
5. Migrate the `tokio::spawn(runner.run())` examples to `.await` / `LocalSet`.
6. Replace the serial Embassy `embassy_transport.rs` hand-roll with sugar over
   the spine; delete its `unsafe impl`s; deprecate `SendFutureWrapper`.
7. CI gate: `cargo build` the full workspace on **both** the `tokio-runtime` and
   `embassy-runtime` feature sets; confirm the serial connector's Tokio and
   Embassy halves now share the spine.
8. Update [Design 028](028-M13-remove-spawn-trait.md) with a forward link and
   `012-M5-connector-development-guide.md` with the unified one-implementation
   guidance.

---

## Decisions

- **Drop `Send` at the boxing boundary, infer it everywhere else.** The only
  authoritative `Send` decision is the `ConnectorBuilder`/runner box; engines
  stay `Send`-transparent so std direct-spawn survives.
- **Keep data/identity `Send`.** Minimizes blast radius; the futures-vs-data
  distinction is the whole reason this is safe and small.
- **No feature flag.** A single contract for all runtimes; the unification is the
  point.

---

## Out of Scope

- **Data-plane pump unification** (`pump_sink`/`pump_source` for Embassy
  MQTT/KNX). Same root cause (`Source: Send`, `Connector: Send + Sync`); a
  natural follow-up once the session-plane change lands.
- **Removing the WASM adapter's `unsafe impl Send/Sync`.** Enabled by this
  change but tracked separately.
- **`aimdb-sync` no_std** ([#46](https://github.com/aimdb-dev/aimdb/issues/46)).
  Unrelated.
