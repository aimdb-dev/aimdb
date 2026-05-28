# Fold the AimX serializer/deserializer into a trait-derived JSON codec

**Version:** 0.2 (implemented — codec is no_std-capable behind the `json-serialize` feature)  
**Status:** ✅ Implemented  
**Depends on:** [M15 — Remove `latest_snapshot`](031-M15-remove-latest-snapshot.md)  
**Last Updated:** May 28, 2026  
**Milestone:** M16 — Remote-access codec extraction

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
  - [The JSON fields are a hand-rolled vtable, not data](#the-json-fields-are-a-hand-rolled-vtable-not-data)
  - [The connector layer already solved this exact problem](#the-connector-layer-already-solved-this-exact-problem)
  - [Post-M15, AimX reads and writes the buffer like any connector](#post-m15-aimx-reads-and-writes-the-buffer-like-any-connector)
  - [The capability is fragmented across three places](#the-capability-is-fragmented-across-three-places)
- [Current Architecture](#current-architecture)
- [Proposed Design](#proposed-design)
  - [1. `RemoteSerialize` — the capability trait](#1-remoteserialize--the-capability-trait)
  - [2. `JsonCodec<T>` — the type-erased storage form](#2-jsoncodect--the-type-erased-storage-form)
  - [3. One codec field replaces two closures](#3-one-codec-field-replaces-two-closures)
  - [4. `with_remote_access()` requires the trait](#4-with_remote_access-requires-the-trait)
  - [5. `JsonReaderAdapter` holds the codec](#5-jsonreaderadapter-holds-the-codec)
  - [6. The three `AnyRecord` methods delegate to the codec](#6-the-three-anyrecord-methods-delegate-to-the-codec)
  - [7. Remove `with_read_only_serialization`](#7-remove-with_read_only_serialization)
- [Alternatives Considered](#alternatives-considered)
- [no\_std Impact](#nostd-impact)
- [Implementation Plan](#implementation-plan)
- [Breaking Changes](#breaking-changes)
- [Open Questions](#open-questions)
- [Out of Scope](#out-of-scope)

---

## Summary

`TypedRecord<T, R>` carries two loose `Option<Arc<dyn Fn>>` fields —
`json_serializer` and `json_deserializer` — set by `.with_remote_access()`.
A third fragment, `JsonReaderAdapter`, bundles a bare serializer closure with
a `BufferReader<T>` for the subscription path. All three exist for one reason:
the type-erased `AnyRecord` methods (`latest_json`, `subscribe_json`,
`set_from_json`) must turn `T` into/out of `serde_json::Value` from a context
where `T: Serialize` is not in scope.

This is the same capture-the-codec-at-config-time pattern the **connector
layer** already uses (`SerializerFn` / `DeserializerFn` on `ConnectorLink` /
`InboundConnectorLink`). After M15, AimX reads and writes records through the
same buffer surface (`peek()` / `subscribe()` / `push()`) that connectors use.
AimX *is* a connector — JSON wire, RPC transport — but its codec is modelled
as bespoke record fields rather than as a connector-style codec.

This design:

1. Adds a capability trait **`RemoteSerialize`** in a new top-level module
   **`crate::codec`**, blanket-implemented for every `T: Serialize +
   DeserializeOwned`. This is the AimX/connector analogue of the data-contract
   traits (`Streamable`, `Linkable`) — a named contract that "unlocks" the JSON
   codec. Every `Streamable` type satisfies it for free.
2. Adds an object-safe **`JsonCodec<T>`** (methods `encode` / `decode`) plus a
   zero-sized `SerdeJsonCodec` built only under that bound — the type-erased
   storage form.
3. Replaces the two closure fields with **one** `remote_codec:
   Option<Arc<dyn JsonCodec<T>>>`, threaded through `latest_json`,
   `set_from_json`, `subscribe_json`, `JsonReaderAdapter`, and `RecordValue<T>`.
4. Removes the dead `with_read_only_serialization` API.

The codec is gated by a dedicated **`json-serialize`** feature (not `std`), so it is
**no_std + alloc compatible** — `serde_json` runs on `alloc` alone (the same way
the `Linkable` data contracts already serialize on embedded targets). `std`
enables `json-serialize` automatically, so AimX is unaffected; embedded targets can opt in
to get `record.latest()?.as_json()` without std/AimX. The AimX type-erased entry
points (`latest_json` / `subscribe_json` / `set_from_json`) remain `std`-gated.

Net: three fragments collapse to one trait-derived object, the serde bound is
named instead of ad-hoc, the codec is reusable on no_std, and the JSON concern
stops looking foreign inside `TypedRecord`. It does **not** relocate the codec
off the record or fold AimX into the connector spawn machinery — those are
discussed under [Alternatives](#alternatives-considered) and deferred.

This is the follow-up named in
[M15 Step 7 / Out of Scope](031-M15-remove-latest-snapshot.md#out-of-scope).

---

## Motivation

### The JSON fields are a hand-rolled vtable, not data

Every other field on `TypedRecord<T, R>` speaks `T` natively: the buffer stores
`T`, producer/consumer/transform move `T`, metadata stores only the type *name*.
The two JSON fields are different in kind — they capture
`serde_json::to_value::<T>` / `from_value::<T>` at the one site where
`T: Serialize` is statically known (`with_remote_access`, `typed_record.rs:941`)
and stash the result for replay from the type-erased `AnyRecord` impl. Rust has
no specialization, so the single blanket `impl AnyRecord for TypedRecord<T, R>`
(`typed_record.rs:1246`) cannot conditionally expose JSON only for serializable
`T`. Some stored capability is unavoidable — but it should be *one named thing*,
not two loose closures plus an adapter.

### The connector layer already solved this exact problem

Connectors face the identical "cross the type-erasure boundary with a captured
codec" problem and solve it on a **link**:

| Concern | Connector | AimX (today) |
|---|---|---|
| out (T → wire) | `ConnectorLink.serializer: SerializerKind` | `json_serializer: Arc<dyn Fn>` |
| in (wire → T) | `InboundConnectorLink.deserializer: DeserializerFn` | `json_deserializer: Arc<dyn Fn>` |
| streaming out | `consumer_factory` + `serializer` | `JsonReaderAdapter` |
| captured at | `.link_to(...).with_serializer(...)` | `.with_remote_access()` |
| stored as | type-erased on the link struct | loose fields on the record |

The connector evidence says the codec is a *link-shaped* concern, captured at
config time, not a core record field.

### Post-M15, AimX reads and writes the buffer like any connector

M15 removed `latest_snapshot`; `record.get` now reads `buffer.peek()`,
`record.subscribe` uses a real `BufferReader`, and `record.set` pushes through
`WriteHandle`. AimX no longer has any private storage — it operates on the same
buffer surface as connectors. The three operations map one-to-one onto connector
lanes:

```
  record.get        peek()      + codec.to_json     (T → Value)   ── outbound point read
  record.subscribe  subscribe() + codec.to_json     (T → Value)   ── outbound stream
  record.set        codec.from_json + push()        (Value → T)   ── inbound write
```

### The capability is fragmented across three places

`json_serializer`, `json_deserializer`, and `JsonReaderAdapter` are one concern
("turn this record's `T` into/out of JSON across the erasure boundary")
expressed three times. Consolidating them removes the ad-hoc inline
`serde_json` closures and the bare-`Fn` field on the adapter.

---

## Current Architecture

```
                         TypedRecord<T, R>
  ┌─────────────────────────────────────────────────────────────────┐
  │  DATA PLANE (speaks T)   producer · consumers · transform ·       │
  │                          buffer · connectors · metadata           │
  ├─────────────────────────────────────────────────────────────────┤
  │  SERIALIZATION (speaks serde_json::Value)   ← foreign concern     │
  │     json_serializer    Option<Arc<dyn Fn(&T)->Value>>             │
  │     json_deserializer  Option<Arc<dyn Fn(&Value)->T>>             │
  └─────────────────────────────────────────────────────────────────┘

  AnyRecord (T erased)        TypedRecord field used      JsonReaderAdapter
  ───────────────────         ──────────────────────      ─────────────────
  latest_json()        ──►    json_serializer             —
  set_from_json()      ──►    json_deserializer           —
  subscribe_json()     ──►    json_serializer  ───────►   { inner: Reader<T>,
                                                            serializer: Fn }
```

Entry points: `record.get` → `db.try_latest_as_json()` (`builder.rs:276`) →
`AnyRecord::latest_json()`; `record.subscribe` → `subscribe_json()`;
`record.set` → `set_from_json()`.

---

## Proposed Design

```
                         TypedRecord<T, R>
  ┌─────────────────────────────────────────────────────────────────┐
  │  DATA PLANE (unchanged)                                           │
  ├─────────────────────────────────────────────────────────────────┤
  │  remote_codec: Option<Arc<dyn JsonCodec<T>>>    ← one named thing │
  └─────────────────────────────────────────────────────────────────┘
       ▲ built from a ZST under the T: RemoteSerialize bound
       │
  latest_json / set_from_json / subscribe_json / RecordValue::as_json
  all route through remote_codec.{encode, decode}
```

### 1. `RemoteSerialize` — the capability trait

New, **feature `json-serialize`** (no_std + alloc compatible), in `aimdb-core` module
`crate::codec`, re-exported from the crate root:

```rust
/// A record type that can be encoded to / decoded from the JSON wire format.
///
/// Blanket-implemented for every `serde` type, so any `T: Serialize +
/// DeserializeOwned` gets a JSON codec for free. This is the AimX/connector
/// analogue of the data-contract capability traits (`Streamable`, `Linkable`):
/// a named contract that unlocks a feature.
pub trait RemoteSerialize: Sized {
    fn to_json(&self) -> Option<serde_json::Value>;
    fn from_json(value: &serde_json::Value) -> Option<Self>;
}

impl<T> RemoteSerialize for T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    fn to_json(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}
```

The whole `crate::codec` module is `#[cfg(feature = "json-serialize")]`. The feature is
`json-serialize = ["alloc", "serde_json"]`, and `std` enables it. `serde` and `serde_json`
are already `default-features = false` + `alloc` in the workspace, so nothing
pulls in `std`.

**Why a new trait instead of reusing `aimdb-data-contracts::Streamable`:** the
dependency runs `aimdb-data-contracts` → `aimdb-core`
(`aimdb-data-contracts/Cargo.toml:28`), not the reverse. Bounding a core method
on `Streamable` would be a cycle. `RemoteSerialize` is the core-local
equivalent, and because `Streamable: Serialize + DeserializeOwned`, every
`Streamable` (and every `Linkable`) type satisfies `RemoteSerialize`
automatically through the blanket impl. This is the direct answer to "unlock
the feature with a trait, like the data contracts" without inverting the
dependency graph.

### 2. `JsonCodec<T>` — the type-erased storage form

`TypedRecord<T, R>`'s `AnyRecord` impl cannot carry a `T: RemoteSerialize`
bound (it must cover non-serializable `T` too), so the capability is stored as
an object. This is the AimX counterpart of the connector layer's
`SerializerFn` / `DeserializerFn`:

Method names are `encode` / `decode` (not `to_json` / `from_json`) — a `from_*`
method taking `&self` trips clippy's `wrong_self_convention`, and `encode` /
`decode` read naturally for a codec object.

```rust
/// Type-erased JSON codec for one record type. Stored where T's serde bounds
/// are out of scope (inside the blanket `AnyRecord` impl).
pub trait JsonCodec<T>: Send + Sync {
    fn encode(&self, value: &T) -> Option<serde_json::Value>;
    fn decode(&self, value: &serde_json::Value) -> Option<T>;
}

/// Zero-sized serde-backed codec. Constructed only under `T: RemoteSerialize`,
/// so the erased `JsonCodec<T>` it yields is guaranteed valid.
pub struct SerdeJsonCodec;

impl<T: RemoteSerialize> JsonCodec<T> for SerdeJsonCodec {
    fn encode(&self, value: &T) -> Option<serde_json::Value> {
        value.to_json()
    }
    fn decode(&self, value: &serde_json::Value) -> Option<T> {
        T::from_json(value)
    }
}
```

`JsonCodec<T>` is object-safe (`T` is fixed per record), so
`Arc<dyn JsonCodec<T>>` is well-formed. The two private type aliases
`JsonSerializer<T>` / `JsonDeserializer<T>` (`typed_record.rs:54-59`) are
deleted.

### 3. One codec field replaces two closures

```rust
// Before
#[cfg(feature = "std")]
json_serializer: Option<JsonSerializer<T>>,
#[cfg(feature = "std")]
json_deserializer: Option<JsonDeserializer<T>>,

// After
/// Type-erased JSON codec; `Some` iff the record opted in via with_remote_access().
/// RecordValue::as_json and (on std) the AimX read/write/subscribe paths route through it.
#[cfg(feature = "json-serialize")]
remote_codec: Option<Arc<dyn JsonCodec<T>>>,
```

`new()` initialises a single `remote_codec: None` under `#[cfg(feature =
"json")]`. `RecordValue<T>` likewise carries a `#[cfg(feature = "json-serialize")]
codec: Option<Arc<dyn JsonCodec<T>>>` and its `as_json()` becomes
`#[cfg(feature = "json-serialize")]`, so `record.latest()?.as_json()` works on no_std.

### 4. `with_remote_access()` requires the trait

```rust
#[cfg(feature = "json-serialize")]
pub fn with_remote_access(&mut self) -> &mut Self
where
    T: crate::codec::RemoteSerialize + 'static,
{
    self.remote_codec = Some(Arc::new(crate::codec::SerdeJsonCodec));
    self
}
```

Because `RemoteSerialize` is blanket-implemented over `Serialize +
DeserializeOwned`, the bound is *source-compatible* with the previous
`T: Serialize + DeserializeOwned` — existing call sites compile unchanged. The
mirror method on `RecordRegistrar` gets the same bound and `json-serialize` gate. The name
`with_remote_access` is kept for API stability; on no_std it installs the codec
purely for local `as_json()` (there is no remote access on embedded).

### 5. `JsonReaderAdapter` holds the codec

```rust
// Before
struct JsonReaderAdapter<T: Clone + Send + 'static> {
    inner: Box<dyn BufferReader<T> + Send>,
    serializer: JsonSerializer<T>,
}

// After
struct JsonReaderAdapter<T: Clone + Send + 'static> {
    inner: Box<dyn BufferReader<T> + Send>,
    codec: Arc<dyn JsonCodec<T>>,
}
// recv_json / try_recv_json call self.codec.encode(&value)
```

`JsonReaderAdapter` stays `#[cfg(feature = "std")]` (it implements the std-only
`JsonBufferReader` for AimX streaming); under `std` the `json-serialize` feature is always
on, so `Arc<dyn JsonCodec<T>>` resolves.

### 6. The three `AnyRecord` methods delegate to the codec

These remain `#[cfg(feature = "std")]` (they are the AimX type-erased entry
points). They read the now-`json-serialize`-gated `remote_codec` field, which is always
present under `std`.

```rust
fn latest_json(&self) -> Option<serde_json::Value> {
    let value = self.buffer.as_ref()?.peek()?;
    self.remote_codec.as_ref()?.encode(&value)
}

fn subscribe_json(&self) -> DbResult<Box<dyn JsonBufferReader + Send>> {
    let codec = self.remote_codec.clone().ok_or_else(|| /* not configured */)?;
    let reader = self.subscribe()?;
    Ok(Box::new(JsonReaderAdapter { inner: reader, codec }))
}

fn set_from_json(&self, json_value: serde_json::Value) -> DbResult<()> {
    // unchanged: no-producer-override + buffer-present checks stay here
    let codec = self.remote_codec.clone().ok_or_else(|| /* not configured */)?;
    let value: T = codec.decode(&json_value).ok_or_else(|| /* schema mismatch */)?;
    self.writer_handle().push(value);
    Ok(())
}
```

`TypedRecord::latest()` and `RecordValue<T>` switch from holding a closure to
holding `Option<Arc<dyn JsonCodec<T>>>`; `RecordValue::as_json()` calls
`codec.encode(&self.value)`. No public signature changes on `latest()` /
`as_json()`.

### 7. Remove `with_read_only_serialization`

`with_read_only_serialization` (`Serialize`-only) had **zero callers** outside
its own definition (confirmed across both `aimdb` and `aimdb-pro`). It is
removed. The read-only tier disappears with it — see
[Open Questions](#open-questions).

---

## Alternatives Considered

**A — Leave it as a single bare closure field.** Collapse the two closures into
one `Option<Arc<dyn Fn>>` without a named trait. Lighter, but keeps the bound
ad-hoc and gives the reviewer's "unlock via a trait" request no answer.
Rejected in favour of the named `RemoteSerialize`.

**B — A `RemoteAccessLink` holder (codec + policy).** Wrap the codec in a struct
that *also* owns the AimX policy currently inlined in `set_from_json`
(no-producer-override, the `writable` flag, ReadOnly enforcement), making AimX a
first-class link structurally peer to `outbound_connectors` /
`inbound_connectors`. This is the fullest realisation of "AimX is a connector,"
and is the natural next step. **Deferred:** migrating the security policy widens
scope into the remote-access permission model and should be its own milestone.
This design intentionally lands only the codec so the diff stays reviewable.

**C — Relocate the codec to a db-level registry.** Move JSON access off
`TypedRecord` entirely into a `HashMap<RecordId, Box<dyn JsonAccess>>` built at
registration, and drop `latest_json` / `subscribe_json` / `set_from_json` from
`AnyRecord`. This makes `TypedRecord` purely data-plane (the reviewer's ideal).
**Rejected for now:** it adds a parallel structure plus a second lookup on the
AimX path, and it diverges from how connectors attach — connector links live on
the record (as `Vec<…Link>`), not in a db-side registry. If the std-only
surface of `AnyRecord` later becomes a maintenance burden, revisit.

**D — Bound on `aimdb-data-contracts::Streamable`.** Rejected: dependency cycle
(contracts → core). `RemoteSerialize` is the core-local equivalent and
`Streamable` types satisfy it automatically.

**E — Reuse `ConnectorLink` / the connector spawn machinery literally.**
Rejected: AimX is RPC-driven (`get`/`set` are on-demand point ops, not spawned
loops bound to a URL), carries its own security policy, and also serves
introspection (`collect_metadata`) — none of which fit a `ConnectorLink`. The
reusable unit is the *codec*, not the link/spawn plumbing.

---

## no\_std Impact

The codec is now **no_std + alloc compatible**, gated by the `json-serialize` feature
(`json-serialize = ["alloc", "serde_json"]`). The workspace already configures `serde` and
`serde_json` as `default-features = false` + `alloc`, so enabling `json-serialize` pulls in
no `std` — the same path the `Linkable` data contracts already use to serialize
JSON on embedded targets.

Feature gating:

| Item | Gate |
|---|---|
| `crate::codec` (`RemoteSerialize`, `JsonCodec`, `SerdeJsonCodec`) | `json-serialize` |
| `TypedRecord::remote_codec`, `with_remote_access`, `RecordValue::{codec, as_json}` | `json-serialize` |
| AimX type-erased methods (`latest_json` / `subscribe_json` / `set_from_json`), `JsonReaderAdapter` | `std` (and `std` ⇒ `json-serialize`) |

What no_std gains: with `json-serialize` on, embedded code can call
`record.latest()?.as_json()` and install the codec via `with_remote_access()`.
What stays std: the AimX protocol itself (Unix socket, type-erased dispatch).
Verified by building `aimdb-core` under `--no-default-features --features
alloc,json-serialize` with `#![cfg_attr(not(feature = "std"), no_std)]` active.

---

## Implementation Plan

Ordered so the workspace compiles and tests pass at every stage.

**Step 1 — Add the codec module + feature.** Add `json-serialize = ["alloc",
"serde_json"]`; make `std` enable `json-serialize`. Create `aimdb-core/src/codec.rs`
(top-level, `#[cfg(feature = "json-serialize")]`) with `RemoteSerialize` (+ blanket impl),
`JsonCodec<T>` (`encode`/`decode`), and `SerdeJsonCodec`. Declare + re-export
from the crate root. Nothing consumes it yet. Green.

**Step 2 — Swap the record over to the codec.** In one cohesive change: replace
the two fields with `remote_codec`; update `new()`, `with_remote_access` (`json-serialize`
gate, `T: RemoteSerialize` bound), `latest_json`, `set_from_json`,
`subscribe_json`, `JsonReaderAdapter`, `latest()`, and `RecordValue<T>`/`as_json`.
Delete the `JsonSerializer<T>` / `JsonDeserializer<T>` aliases. Update the
`RecordRegistrar::with_remote_access` bound + gate. Green.

**Step 3 — Remove `with_read_only_serialization`.** Delete the method (zero
callers). Green.

**Step 4 — Build matrix + tests.** Build `aimdb-core` under: default (std),
`--no-default-features --features alloc` (codec absent), and
`--no-default-features --features alloc,json-serialize` (codec present, no_std). Clippy
clean on all three. Run core lib tests + the tokio adapter remote-access/drain
integration suite (`record.get` / `set` / `subscribe` on `SingleLatest` /
`Mailbox` / SPMC Ring). Green.

---

## Breaking Changes

**`with_read_only_serialization` removed** — `pub` API with no callers in either
workspace. Any external code relying on serialize-only remote access must switch
to `with_remote_access` (which now also requires `DeserializeOwned`).

**`with_remote_access` bound + gate** — bound changes from `Serialize +
DeserializeOwned` to `RemoteSerialize` (source-compatible via the blanket impl,
no call-site changes), and the method is now gated on `json-serialize` rather than `std`.
Since `std` enables `json-serialize`, all existing std callers are unaffected.

**New public items** — `RemoteSerialize`, `JsonCodec` (methods `encode` /
`decode`), `SerdeJsonCodec`, all in `crate::codec` (additive, feature `json-serialize`).

**No wire/protocol change** — `record.get` / `set` / `subscribe` behave
identically; the codec produces the same JSON as the closures did.

---

## Open Questions

1. **Drop the read-only tier?** *Resolved — dropped.* `RemoteSerialize` requires
   both `Serialize` and `DeserializeOwned`. `with_read_only_serialization` had no
   callers in `aimdb` or `aimdb-pro`, so the `Serialize`-only tier was removed. If
   a `Serialize`-but-not-`DeserializeOwned` record ever needs read-only exposure,
   reintroduce a read-only codec variant.
2. **Blanket impl vs explicit marker.** The blanket impl makes every `serde`
   type codec-ready with zero boilerplate. The data contracts instead use
   *explicit* opt-in (`impl Streamable for T {}`). If parity / intentional opt-in
   is preferred, make `RemoteSerialize` a marker the user implements. Trade-off:
   ergonomics vs. explicitness. *(Shipped as blanket impl.)*
3. **Land Alternative B in the same milestone?** i.e. also migrate the AimX
   security policy onto a `RemoteAccessLink`. *Resolved — no.* Kept this milestone
   to the codec; the link/policy move is a future milestone.
4. **Module home.** *Resolved — `crate::codec`.* Originally proposed
   `crate::remote::codec`, but since the codec is now no_std-capable and feature-
   (`json-serialize`-) gated rather than AimX/`std`-specific, a neutral top-level
   `crate::codec` module is the correct home. `crate::remote` is itself
   `std`-gated, which would have forced the codec to be std-only.
5. **`with_remote_access` naming on no_std.** The method installs a codec; on
   no_std there is no "remote access," only local `as_json()`. The name is kept
   for std API stability, but a neutral alias (e.g. `with_json()`) could be added
   later if the embedded ergonomics warrant it.

---

## Out of Scope

- **AimX security-policy relocation** onto a link holder (Alternative B) — the
  no-producer-override / `writable` / ReadOnly checks stay inline in
  `set_from_json` for this milestone.
- **Full relocation to a db-level codec registry** (Alternative C) —
  `TypedRecord` keeps the (now single) codec field; `AnyRecord` keeps its three
  JSON methods.
- **Folding AimX into the connector spawn machinery** (Alternative E).
- **Custom (non-JSON) AimX wire formats** — `SerdeJsonCodec` is the only codec;
  a pluggable codec per connection is not introduced here.
