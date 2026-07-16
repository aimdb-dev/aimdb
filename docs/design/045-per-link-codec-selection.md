# Design 045: Per-link record codec selection

**Status:** implemented

**Issue:** [#178](https://github.com/aimdb-dev/aimdb/issues/178)

**Prerequisites:** issue #177 and PR #180

## 1. Problem

`Linkable` defines one default wire representation for a record type. That is a
useful 80% path, but a single type can cross heterogeneous boundaries:

```text
Temperature
    +-- UART to MCU ------> compact Postcard
    +-- MQTT to cloud ----> inspectable JSON
```

Before this design, selecting those formats independently meant repeating raw
serializer/deserializer closures at every registration site. The closures were
powerful, but they discarded the `Linkable` registrar ergonomics and made it
easy for applications to implement subtly different error and buffer behavior.

The goal is per-link selection without turning codec choice into runtime state
or weakening the bounded serialization path added by PR #180.

## 2. Decision

`aimdb-data-contracts` owns three additive pieces:

1. `LinkCodec<T>`: a format-neutral codec value captured at registration.
2. `LinkCodecBuilderExt<T>::with_link_codec(codec)`: works on inbound and
   outbound core link builders while preserving their concrete types.
3. `LinkCodecRegistrarExt::{linked_from_with, linked_to_with}`: shorthand for
   the common registrar path.

Built-in markers live in `link_codecs`:

- `Default` delegates to the record's existing `Linkable` implementation;
- `Json` is available with `linkable-json`;
- `Postcard<const N: usize = 256>` is available with `linkable-postcard`.

Example:

```rust,ignore
use aimdb_data_contracts::{
    link_codecs::{Json, Postcard},
    LinkCodecBuilderExt,
    LinkCodecRegistrarExt,
};

builder.configure::<Temperature>("temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 32 });

    reg.linked_to_with("serial://mcu/temperature", Postcard::<128>);
    reg.linked_to_with("mqtt://cloud/temperature", Json);

    reg.link_to("mqtt://cloud/alerts")
        .with_link_codec(Json)
        .with_qos(1)
        .finish();
});
```

The existing `.linked_from(url)` and `.linked_to(url)` methods are unchanged.

## 3. Codec contract

Conceptually, the trait is:

```rust,ignore
pub trait LinkCodec<T>: Clone + Send + Sync + 'static {
    const ENCODE_BUFFER_CAPACITY: Option<usize> = None;

    fn decode(&self, bytes: &[u8]) -> Result<T, String>;
    fn encode(&self, value: &T) -> Result<Vec<u8>, SerializeError>;
    fn encode_into(
        &self,
        value: &T,
        out: &mut [u8],
    ) -> Result<usize, SerializeError>;
}
```

The owned encoder is always present because it is the correctness fallback for
oversized values. `Some(N)` opts an outbound route into a reusable scratch
buffer. A codec must advertise a capacity only when its successful bounded
encoder does not allocate.

`encode_into` is required rather than given an allocating default. That makes
the performance promise explicit at every implementation that opts into
scratch storage.

The existing `SerializeError` remains the correct error layer:

```text
typed record <---- LinkCodec / SerializeError ----> opaque connector payload
session bytes <------- CodecError ----------------> AimX/session envelope
```

No `LinkCodecError` is added. Decode retains `String` to match the existing core
inbound builder and the compatibility decision from issue #177.

## 4. Registration-time specialization

The codec is captured once in AimDB's existing typed closures:

```text
control plane (once)

RecordRegistrar<T>
    |
    | with_link_codec(Postcard::<128>)
    v
OutboundConnectorBuilder<T>
    |
    +-- owned closure:  codec.encode(&T)
    `-- scratch closure: codec.encode_into(&T, &mut [u8])
                         capacity = 128
```

After `finish()`, core erases the route in the same place it already erased raw
serializers. No per-message map lookup or codec enum match is introduced.

Why a generic codec value instead of an enum:

- downstream crates can add codecs without modifying AimDB;
- a codec may carry immutable configuration;
- zero-sized markers compile down to no stored payload state;
- the only dynamic call remains the route's existing erased serializer call.

Why no registry:

```text
rejected hot path

message -> lock/map -> string or TypeId lookup -> dyn codec -> encode
```

That shape would add failure states, synchronization or lookup overhead and a
second erasure boundary to solve a configuration-time problem.

## 5. Builder composition

`with_link_codec` consumes and returns the same core builder type. It does not
wrap the builder in a new generic facade. Connector extension traits therefore
remain available:

```rust,ignore
reg.link_to("mqtt://cloud/readings")
    .with_link_codec(Json)
    .with_qos(1)
    .with_retain(false)
    .finish();
```

Repeated selection replaces the entire serialization strategy, not only its
owned half. A bounded codec installs owned and scratch callbacks; replacing it
with an owned-only codec calls
`OutboundConnectorBuilder::clear_serializer_into`. Without that reset,
`Postcard<N> -> Json` would leave the Postcard scratch callback active for
fitting values and use JSON only on fallback, mixing formats on one route.

The registrar shorthand lives in a new sibling trait. Adding required methods
to the existing public `LinkableRegistrarExt` could break a downstream manual
implementation, even though AimDB itself supplies the normal implementation.

## 6. Outbound memory and timing

PR #180 already placed scratch ownership at the route pump, where the lifetime
is easiest to prove:

```text
one outbound route pump

+---------------- stack/future state ----------------+
| scratch: Vec<u8> = vec![0; N]                       |
|   ptr ------------------------+                     |
+--------------------------------|--------------------+
                                 v
heap                        [ N initialized bytes ]
```

Per message:

```text
time -------------------------------------------------------------------->

recv T
  | codec.encode_into(&T, &mut scratch)
  |                    mutable borrow ends
  v
Scratch { len }
  | connector.publish(&scratch[..len]).await
  |                   immutable borrow held across await
  v
publish complete -> scratch may be mutated for the next value
```

No reference escapes `recv_into`; only `Scratch { len }` does. The pump checks
that `len <= scratch.len()` before borrowing the prefix.

Overflow remains a performance event, not data loss:

```text
encode_into
    +-- Ok(len) ----------> Scratch { len }
    +-- BufferTooSmall ---> codec.encode(&T) -> Owned(Vec<u8>)
    `-- InvalidData ------> log/skip under existing fused-reader semantics
```

The fallback runs once. There is no resize/retry loop.

## 7. Concrete codec behavior

### Default

`link_codecs::Default` forwards all operations and the capacity constant to
`T: Linkable`. It is useful when generic configuration code wants an explicit
codec value, but existing registrar verbs remain the simplest default path.

### JSON

JSON uses `serde_json::from_slice` and `serde_json::to_vec`. Its capacity is
`None`: encoded size has no general fixed upper bound and the implementation
does not claim a zero-allocation hot path. `encode_into` is still logically
correct when called directly, but it serializes through owned bytes; builders
do not install it.

### Postcard

`Postcard<N>` uses `postcard::to_slice` for the bounded path and
`postcard::to_allocvec` for fallback. `SerializeBufferFull` maps to the existing
unit `SerializeError::BufferTooSmall`.

`N` is a per-route RAM/performance budget, not a transport maximum. An
application must still configure connector frame limits independently.

## 8. Concurrency, cache and code-size analysis

```text
route A task ---- scratch A ---- codec A ---- publish A
route B task ---- scratch B ---- codec B ---- publish B
route C task ---- scratch C ---- codec C ---- publish C
```

- Each route owns its scratch allocation; there is no mutable aliasing.
- No lock, atomic, global pool or shared mutable codec state is added.
- Logical ownership does not force cache-line ping-pong. Allocator placement
  may put heap blocks next to each other, but routes never intentionally write
  the same bytes.
- Baseline scratch RAM is the sum of active bounded-route capacities. For 32
  `Postcard<256>` routes, payload storage is about 8 KiB plus `Vec` headers and
  allocator metadata.
- Each distinct `Postcard<N>` is a monomorphization. Embedded applications
  should standardize on a small set of capacity classes to avoid unnecessary
  flash growth.
- Codec selection adds no syscall. Transport enqueueing, network syscalls,
  protocol copies and scheduler wakeups remain outside the codec seam and
  usually dominate end-to-end latency.

## 9. Framing and adversarial input boundary

A codec receives one connector payload. It does not own:

- TCP/serial partial-read assembly;
- length-prefix or delimiter parsing;
- maximum-frame enforcement;
- reconnect/backpressure policy.

Those checks must happen before decode so an attacker cannot force an unbounded
allocation merely by declaring a huge frame. Codec implementations then treat
malformed payload content as `Err`, never as a panic.

## 10. Feature layout

```text
linkable
    +-- alloc
    `-- aimdb-core/alloc

linkable-json
    +-- linkable
    +-- serde_json
    `-- aimdb-derive

linkable-postcard
    +-- linkable
    `-- postcard (default features off, alloc fallback enabled)
```

The Makefile exercises host, no_std and Cortex-M combinations explicitly so a
new codec feature cannot silently work only under workspace feature unification.

## 11. Validation

The implementation contains:

- direct default, stateful custom, JSON and Postcard codec tests;
- malformed-input tests;
- exact and undersized Postcard buffer tests;
- a route-level test that gives the same `Reading` type JSON and Postcard
  outbound links, asserting `Owned` versus `Scratch` payload ownership;
- a deliberately undersized Postcard route that exercises the owned fallback;
- route-level inbound JSON and Postcard ingestion;
- a bounded-Postcard-to-owned-JSON replacement regression that asserts the
  stale scratch capacity is cleared and the route emits owned JSON;
- builder composition checks using config methods after codec selection;
- an MQTT integration test that compiles and builds outbound
  `.with_link_codec(...).with_qos(...).with_retain(...)` and inbound
  `.with_link_codec(...).with_qos(...)` chains, then checks the installed
  scratch capacity, MQTT options and Postcard ingest path;
- a warmed allocation benchmark comparing direct `Linkable::encode_into` with
  `Postcard<N>::encode_into`.

At implementation time, the B0 benchmark measured 10,000 iterations for each
path and reported zero allocation calls and zero allocated bytes in both
windows. This is a codec-seam claim, not a claim that a complete connector
publish performs no allocation. After capturing both results, a positive
control deliberately allocates a `Vec<u8>` with at least 64 bytes of capacity
and asserts that both counters move. Running it after the measured windows
proves the counting allocator is wired without contaminating either zero.

The final `make check` passed the host test/clippy matrix, Cortex-M and WASM
checks, dependency policy, README and code-generation drift checks, the
generated Postcard round-trip, generated Cortex-M cross-compilation and the
production-graph simulation guard. That run included the MQTT hardening case;
the complete MQTT connector suite passed 7 unit, 3 link-extension, 16
topic-provider and 2 compiled doctests.

## 12. Deferred work

- Codec selection in AimX/MCP/CLI and per-connector code generation.
- Sharing encoded bytes among routes that prove identical codec semantics.
- Schema-derived maximum encoded sizes.
- A genuinely bounded JSON writer, if a useful maximum can be proven.
- Connector-level latency benchmarks separating serialization from transport
  scheduling and syscall cost.
