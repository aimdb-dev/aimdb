# 060 — Trace context

**Status:** 📝 Proposed

**Scope:** A trace context on every record value written by an instance: a
trace ID, the write's own span ID, its parent's span ID, and a depth. It is
carried in `WriteMetadata` (058) and propagated along derivations and across
connectors that can carry it. Propagation goes through the existing
transform, join and producer APIs, plus one new transform body for
single-input async derivations. Rerun taps and agents get exact causal
chains (§6). Breaking for Rust APIs (join handlers, `WriteMetadata`, session
engine, runtime adapters); additive on the wire (the graph shapes do not
change). Ships as a 3.x minor release despite the breaks (§8.1).

A throwaway prototype of phases 1 and 2 (trace ID and depth, without span
IDs) passed every test in §10 that it covered; its measurements are quoted in
§4.3 and §5.

---

## 1. The rule

> Every record write carries a trace context. A write either **starts** a
> trace or **continues** the trace of the value that caused it, and names
> that value as its parent. A trace context is created only when a value is
> written locally; a value read from a peer carries exactly what the peer
> sent.

| Field | Origin write | Derived write |
|---|---|---|
| `trace_id` (16 bytes) | new | the cause's |
| `span` (4 bytes) | new | new |
| `parent` (4 bytes) | none | the cause's `span` |
| `depth` (`u8`) | 0 | the cause's + 1, saturating at 255 |

The stamp (058) says **when** a value was written here. The trace context
says **why**: which chain it belongs to and exactly which write caused it.
The stamp stays local; the trace context is inherited.

## 2. API

### 2.1 Unchanged

Sources, links, `tap`, `transform` and `transform_join` keep their builders.

```rust
reg.source(|ctx, producer| async move {
    loop {
        producer.produce(read_sensor().await);   // starts a trace
        ctx.time().sleep_secs(1).await;
    }
});

// map and with_state: the framework continues the input's trace.
reg.transform::<Temperature, _>(TempKey::Raw, |t| t.map(|raw| Some(calibrate(raw))));
```

### 2.2 Single-input async transforms

A single-input transform gains an async body, next to `map` and
`with_state`. It receives the input as typed `Stamped<I>` values and writes
through a `DerivedWriter<O>`:

```rust
builder.configure::<Alarm>(AlarmKey::Overheat, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .transform::<Temperature, _>(TempKey::Calibrated, |t| {
            t.task(|ctx, mut rx, writer| async move {
                while let Ok(temp) = rx.recv().await {        // Stamped<Temperature>
                    if temp.value.celsius > 80.0 {
                        writer.produce_from(Alarm::Overheat, temp.metadata());
                    }
                }
            })
        });
});
```

- It replaces a `tap` that writes another record through `db.producer()`:
  the write becomes an edge of the dependency graph.
- It can await, use timers and write zero, one or many values per input.
- In the graph it is the existing `RecordOrigin::Transform { input }`, so
  the graph shapes on the wire do not change.
- The receiver is typed: no boxing, no downcast.

### 2.3 Joins

`on_triggers` handlers receive a `DerivedWriter<O>` instead of a
`Producer<O>`. `JoinTrigger` already carries the input's `WriteMetadata`:

```rust
reg.transform_join(|b| b
    .input::<Temperature>(TempKey::Gamma)
    .input::<Humidity>(HumidityKey::Gamma)
    .on_triggers(|mut rx, writer| async move {
        while let Ok(trigger) = rx.recv().await {
            // … update state …
            writer.produce_from(dew_point(&t, &h), trigger.metadata());
        }
    }));
```

```rust
impl<O> DerivedWriter<O> {
    pub fn produce_from(&self, value: O, cause: &WriteMetadata);
    pub fn produce_origin(&self, value: O);   // timer, timeout: an explicit choice
}
```

`DerivedWriter` has no plain `produce`, so a join or an async transform
cannot forget its cause.

### 2.4 Producers

`Producer<T>` keeps `produce`, which starts a trace, and gains
`produce_from(value, &WriteMetadata)` for code outside the graph, for
example a task that bridges an external system. Such writes have an exact
parent, but the graph has no edge for them (the record stays `Passive`).
Derivations from another record should use a transform (§2.2).

### 2.5 Reading

```rust
impl WriteMetadata {
    pub fn trace(&self) -> Option<TraceContext>;
}

let t = reader.recv().await?;
let tc = t.metadata().trace().expect("local writes always carry a trace");
tc.trace_id();   // TraceId, Display = 32 hex chars
tc.span();       // SpanId, Display = 8 hex chars
tc.parent();     // Option<SpanId>: None for an origin
tc.depth();      // u8
```

- **In process:** every value read from a local record has `Some`.
- **`aimdb-client`:** `Some` when the peer sent a `trace`, `None` when it did
  not (an older peer, or a link with tracing off). The client never invents
  an ID, so two reads of the same stored value report the same thing.

`produce_from` takes `&WriteMetadata`. If its trace is `None` (metadata
decoded from a peer and passed back in), the write starts a trace.

### 2.6 Opting out

Tracing is on by default, behind the `trace-context` feature. Turning it
off removes the trace from `WriteMetadata` (back to 8 bytes, §4.3),
`trace()` returns `None`, AimX omits the field and inbound traces are
ignored. This is the opt-out for MCUs that cannot spare the RAM.

## 3. Dependency graph

No change. `RecordOrigin`, `EdgeType` and the AimX `graph.*` replies keep
their 4.x shapes (`{"transform":{"input":…}}`,
`{"transform_join":{"inputs":[…]}}`). A single-input async transform is a
`Transform { input }` like `map`.

## 4. Inside the database

### 4.1 Envelope

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId([u8; 16]);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId(NonZeroU32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceContext {
    trace_id: TraceId,
    span: SpanId,
    parent: Option<SpanId>,
    depth: u8,
}

pub struct WriteMetadata { stamp: Stamp, trace: Option<TraceContext> }
```

`SpanId` is non-zero, so `Option<SpanId>` and `Option<TraceContext>` cost no
extra bytes.

`RecordWriter::push(value, cause: Option<TraceContext>)` stays the one place
that creates `WriteMetadata` for a buffer, and always stores `Some`: every
write gets a new span; `None` as the cause starts a trace, `Some(c)`
continues `c` with `parent = c.span`. Only
`WriteMetadata::from_wire(stamp, Option<TraceContext>)` (replacing
`from_wire_stamp`) can hold `None`, and it never reaches a buffer.

| Writer | Cause |
|---|---|
| `Producer::produce`, `DerivedWriter::produce_origin` | `None` |
| `map`, `with_state` | the input |
| `Producer::produce_from`, `DerivedWriter::produce_from` | the given metadata |
| inbound connector with a trace on the wire | the wire trace |
| AimX `write` from a peer's outbound mirror, with a trace | the wire trace (§5.1) |
| inbound connector or AimX `write` without one, AimX `record.set`, persistence restore | `None` |

An inbound value without a trace therefore gets a new ID when, and only
when, it is inserted into a local record. Every AimX `record.set` starts a
trace: a remote client cannot continue one.

### 4.2 IDs

The trace ID is W3C-sized (16 bytes), so a later OTEL exporter can use it
unchanged. Span IDs are 32 bits: they only need to be unique within one
trace, and two of them fit in the 8 bytes the exact parent may cost.

```text
counter  = instance counter, + 1 per write
span     = mix32(salt32 ⊕ counter)          // bijective; 0 is skipped
trace_id = salt (8 bytes) ‖ counter (8 bytes)   (origin writes only)
```

- Within one instance, span IDs are unique for 2³² writes. Two writes from
  different instances in the same trace collide with probability about
  k² / 2³³ for k such writes.
- An OTEL exporter widens span IDs to W3C's 8 bytes by prefixing the
  writing instance's salt.

The salt is taken once at `build()`, from the runtime adapter, not from a
`std` switch in core: several crates build core without `std` even on a
hosted target, so a feature flag picks the wrong source.

```rust
pub trait RuntimeOps {
    // …
    /// A 64-bit seed for trace IDs, or `None` if the platform has no entropy.
    fn id_seed(&self) -> Option<u64>;
}
```

| Adapter | Seed |
|---|---|
| Tokio | OS randomness (`getrandom`) |
| WASM | `crypto.getRandomValues` |
| Embassy | from the adapter's configuration: a hardware RNG value, or the chip ID mixed with a persisted boot counter |
| `test_support` runtimes | a fixed seed |

`AimDbBuilder::id_seed(u64)` overrides the adapter, for reproducible IDs in
tests. If neither supplies a seed, `build()` fails with a configuration
error.

The counter is a 64-bit atomic where the target has one and a critical
section elsewhere (Cortex-M has no native 64-bit atomics). Every write
touches it; there is no RNG call per write.

### 4.3 Memory and time

`WriteMetadata` grows from 8 to 40 bytes (8 stamp + 28 trace, padded to 8).
Every buffer slot pays it:

| `T` | `Stamped<T>` (058) | `Stamped<T>` (060) | Added per slot |
|---|---|---|---|
| `u8`, `f32`, `f64` | 16 | 48 | 32 |
| 12 B, align 4 | 24 | 56 | 32 |
| 24 B, align 8 | 32 | 64 | 32 |

A 16-slot `f32` ring on Embassy goes from 256 to 768 bytes. With
`trace-context` off (§2.6), nothing changes. The 058 `size_of` assertions
are updated; the allocation gates must show no added allocation.

Measured in the prototype (trace ID and depth only, 32-byte metadata): no
allocation added on the produce, consume, connector or AimX paths;
`record.get` allocates 93 more bytes per call for its longer reply. On the
host, a write that drew from the counter took about 14 ns longer and one
that did not about 3 ns. The prototype guarded the counter with a lock; with
a native atomic, and every write now drawing a span, the cost is re-measured
in phase 1.

## 5. Wire

One carrier value everywhere:
`<trace_id: 32 hex>/<depth>/<span: 8 hex>/<parent: 8 hex>`, with parent
`00000000` for an origin. For example
`4bf92f3577b34da6a3ce929d0e0e4736/3/9c41e2d0/5a07b3f1`.

| Connector | Carries it as |
|---|---|
| AimX over TCP, UDS, WebSocket | optional `trace` field next to `ts` on every value |
| AimX over Serial | off by default; the connector's configuration turns it on |
| MQTT v5 (both backends, §5.3) | user property `aimdb-trace` |
| Zenoh | attachment key `aimdb-trace` |
| KNX | not carried |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "trace":"4bf92f3577b34da6a3ce929d0e0e4736/3/9c41e2d0/5a07b3f1","data":{"celsius":21.5}}
```

- The field is optional: an older peer or a link with tracing off sends
  none. AimX decoders ignore unknown fields, so this is the next minor
  version after 059's 4.1. Nothing else on the wire changes.
- Absence means different things on the two paths, with the same frame:
  - **observing** (client `get`, `watch`, `drain`): the value has no trace,
    `trace()` is `None`;
  - **ingesting** (inbound connector writing locally): the local write starts
    a trace.
- A malformed field (bad hex, depth above 255, zero trace ID or span) is
  treated as absent: the value is kept and its trace is `None`. The trace is
  advisory and must never cost a value.
- A receiver that ingests a traced value continues it: same ID, depth + 1,
  a new span, and the sender's span as parent.
- About 63 bytes per value on AimX (the prototype measured 45 for the ID and
  depth alone; the two span IDs add 18). No connector wraps the payload.

### 5.1 AimX outbound mirror writes

The `trace` field on values covers the subscription direction (`event`,
`snap`, replies). The opposite direction, a local record mirrored to a
peer, uses the fire-and-forget `write` frame, which today carries only
`topic` and `payload`: the client's mirror pump drops `msg.metadata`, and the
receiving dispatch passes only the value to `set_record_from_json`.

- The `write` frame gains the same optional `trace` field:
  `{"t":"write","topic":…,"trace":"…","payload":…}`. It gains no `ts`: the
  receiver stamps the write locally (058).
- The mirror pump sends `msg.metadata.trace()`.
- `Inbound::Write` and `Session::write` carry an `Option<TraceContext>`
  through to the record write, which continues it or, without one, starts a
  trace.

A user-issued `record.set` is not a mirror write and always starts a trace
(§4.1).

### 5.2 Session engine

The trace travels in the session engine's own types, as new fields and
parameters, not through defaulted `*_traced` methods: a default would let a
session that does not override it drop traces silently.

| Type | Change |
|---|---|
| `SubUpdate` | `trace: Option<TraceContext>` |
| `Outbound::Event`, `Outbound::Snapshot` | `trace: Option<TraceContext>` |
| `Inbound::Write` | `trace: Option<TraceContext>` |
| `Session::write` | takes `trace: Option<TraceContext>` |
| `Session::snapshots` | returns each value's trace |
| `ClientHandle::write` | takes the trace; the mirror pump passes `msg.metadata.trace()` |

Every `Session` implementation carries the trace: the AimX session and the
WebSocket connector's session pass it to the record write. Inbound connectors
call `InboundDispatch::dispatch_traced(topic, payload, trace)`; the ingest
closure receives it through a new `TopicMatch::trace()` accessor, so
`IngestFn` keeps its signature. `InboundDispatch::dispatch` remains for
transports without a carrier.

### 5.3 MQTT v5 (draft)

Both MQTT backends speak MQTT 5 and carry the trace as one user property.

- **Tokio backend:** moves from `rumqttc`'s 3.1.1 client to `rumqttc::v5`.
  Outbound: the publish loop already receives each value's `WriteMetadata`
  (`OutboundMessage::metadata`) and publishes with
  `user_properties: [("aimdb-trace", "<carrier>")]`. Inbound: the event loop
  reads the property from each `Publish` and calls `dispatch_traced`.
- **Embassy backend (`mountain-mqtt`):** already speaks MQTT 5 and accepts up
  to 32 properties per inbound publish, but neither reads nor writes user
  properties. `deliver` reads `aimdb-trace`; `publish_staged` adds it.
- **Cost:** the property adds about 68 bytes per publish (1 + 2 + 11 + 2 +
  52). The embedded write ring and the broker's maximum packet size are
  checked with the property included.
- **Other subscribers:** an MQTT 5 broker forwards user properties to MQTT 5
  subscribers unchanged and drops them for 3.1.1 subscribers; the payload
  never changes. A retained message keeps its property, so a late subscriber
  continues the retained write's trace.
- **Brokers that only speak 3.1.1:** open question 1.

## 6. Rerun taps and agents

A Rerun tap is a read-only `tap` on a Tokio instance. For each value it logs
the value, the trace ID, its span, its parent and the depth, and the stamp on
that instance's own timeline. Each instance also logs its dependency graph
once. MCUs run no Rerun taps; their values show up at the hub with their
trace intact.

**Parents are exact.** A write's parent is the write in the same trace whose
span equals its `parent`. That holds in the diamond case (`Raw → A`,
`Raw → B`, `A + B → C`): each write of `C` names the write of `A` or `B`
that triggered it.

When the parent was not captured (a record without a Rerun tap, a tap that
skipped values, or an instance without one), Rerun still knows that the
parent exists and its span, and the graph names the records it can be on.
It shows the hop as known but not captured.

Depth orders a trace across instances without comparing clocks, and is
shown as a timeline. Depth saturating at 255 no longer affects parentage.

Agents read the same through MCP: `get_record` returns span and parent, so an
agent can walk a chain without guessing.

The Rerun integration itself (crate, mapping contracts to Rerun types,
layout) is a separate design.

## 7. Tools

- `aimdb get <record> --trace` and `aimdb watch <record> --trace` show the
  trace ID, span, parent and depth, or `no trace` for a value the peer sent
  without one.
- MCP `get_record` includes the trace, or `null`.

## 8. What breaks

| Area | Change |
|---|---|
| Join handlers | `on_triggers` receives `DerivedWriter<O>` instead of `Producer<O>` |
| `WriteMetadata` | 40 bytes; `trace() -> Option<TraceContext>`; `from_wire_stamp` becomes `from_wire(stamp, Option<TraceContext>)` |
| Session engine | new fields and parameters (§5.2); every `Session` and `EnvelopeCodec` implementation updates |
| Runtime adapters | `RuntimeOps::id_seed()`; Embassy takes its seed in the adapter configuration |
| `build()` | fails when neither the adapter nor `.id_seed()` supplies a seed |
| MQTT, Tokio backend | MQTT 5 instead of 3.1.1 (§5.3) |
| `TransformBuilder`, `Producer<T>`, `InboundDispatch`, `TopicMatch` | additive: `task`, `produce_from`, `dispatch_traced`, `trace()` |
| `aimdb-client` | `RecordUpdate` gains `trace` |
| AimX | optional `trace` field, minor version; graph shapes unchanged |

In the prototype, the only user code that had to change was the three
weather-mesh join handlers (`produce` → `produce_from(…, trigger.metadata())`)
and the codegen join stub (`Producer<O>` → `DerivedWriter<O>`).

### 8.1 Release

AimDB has no active user base to protect, so 060 ships as a minor release
although it breaks APIs:

- `aimdb-core` and the adapter and connector crates: 3.x minor.
- `aimdb-client`: 0.9.0.
- AimX protocol: the next minor after 059's 4.1. The wire change is additive.

The CHANGELOG lists every row of the table above under "Breaking changes".

## 9. Phases

1. **Core:** envelope with span and parent, IDs and seeds, automatic
   propagation in `map` / `with_state`, the single-input `task` body,
   `DerivedWriter` in joins, `Producer::produce_from`, the `trace-context`
   feature, tests on Tokio, Embassy and WASM. Enough for Rerun taps on one
   instance.
2. **AimX:** `trace` field on values and on mirror `write` frames, session
   engine, client, CLI, MCP; Serial off by default. Enough for chains across
   AimX-linked instances in both directions.
3. **MQTT v5** on both backends, and **Zenoh**.

## 10. Tests

- Source → `map` → `with_state` → `task` → join keeps one trace ID; depth
  grows by one per write; each write's `parent` is its cause's `span`.
- Every write gets a new span; spans in one instance never repeat or are zero.
- `produce_origin` and `Producer::produce` start new traces with no parent;
  `Producer::produce_from` continues one.
- `task`: typed receiver; zero, one and several writes per input; the graph
  shows `Transform { input }`.
- Every locally written value has `trace()` = `Some`.
- Fixed seed gives identical IDs across runs.
- Seeds: each adapter supplies one; `.id_seed()` overrides it; `build()`
  fails without either; the `test_support` runtimes build without
  `.id_seed()` (core's no_std test suites, `aimdb-data-contracts`).
- `trace-context` off: `size_of::<WriteMetadata>() == 8`, `trace()` is
  `None`, AimX frames carry no `trace`.
- A malformed `trace` field on a value or `write` frame keeps the value and
  decodes as `None`.
- **Legacy frame, both paths:** the same AimX value frame without `trace`
  - decoded twice by `aimdb-client` gives `None` both times;
  - ingested by an inbound connector gives a local write with a new trace at
    depth 0.
- AimX round-trip of a traced value; ingesting it continues the trace with
  the sender's span as parent.
- **Mirror push:** instance A mirrors a record to instance B over AimX; B
  stores A's trace ID, A's depth + 1 and A's span as parent. A `write` frame
  without `trace` starts a trace on B. The same over the WebSocket connector.
- Serial: no `trace` by default; present when turned on.
- `record.set` always starts a trace.
- **MQTT v5:** a publish carries `aimdb-trace`; a receiving instance continues
  the trace; a publish without the property starts one; on Embassy, a publish
  whose property pushes it past the broker's maximum packet size is rejected
  and counted like any oversize publish.
- `graph.nodes` and `graph.edges` replies are byte-identical to 4.x for a
  fixture with a transform, a `task` and a join.
- A value that is migrated on the inbound path keeps its trace.
- **Exact parents:** in the diamond `Raw → A, Raw → B, A + B → C`, each write
  of `C` names the span of the write of `A` or `B` that triggered it; with
  that write missing from a recording, the parent is reported as known but
  not captured.
- `size_of::<WriteMetadata>() == 40`; allocation gates unchanged.

## 11. Alternatives considered

1. **Merge `transform` and `transform_join` into one builder.** Changes the
   graph shapes on the wire and every transform call, and makes a
   single-input caller downcast its own declared type through the boxed join
   path. Rejected: a typed `task` body on the existing single-input builder
   (§2.2).
2. **Parent candidates from the graph instead of stored parents.** Costs
   nothing per slot, but is ambiguous in diamonds, which some pipelines
   have, and agents cannot act on "one of these two". Rejected: store the
   parent span.
3. **64-bit span IDs.** W3C-sized, but 16 bytes per slot instead of 8.
   Uniqueness within one trace only needs 32 bits. Rejected; an OTEL
   exporter widens them (§4.2).
4. **Pick a parent by stamp order.** Wrong in the diamond case even with
   every write captured. Rejected.
5. **Mint a trace ID in the client for untraced values.** Two reads of the
   same stored value would report different IDs. Rejected: `None` on read,
   new ID only on local insertion.
6. **Implicit "current trace"** (task-local). Not available on Embassy,
   unclear across `.await`. The cause is an argument instead.
7. **Sampling and a span sink.** No consumer needs them yet.
8. **Plain `produce()` in joins and tasks.** A forgotten cause would silently
   start a new trace. `DerivedWriter` makes it a compile error.
9. **Carry the trace inside the payload** where a protocol has no headers.
   Breaks every non-AimDB consumer.
10. **Defaulted `*_traced` session methods** to keep the session engine
    additive. A session that does not override them drops traces silently,
    and the surface doubles. Rejected: new fields and parameters (§5.2).
11. **Seed from a `std` feature switch in core.** In the prototype it failed
    37 existing tests: crates such as `aimdb-data-contracts` and the WASM
    adapter build core without `std` on hosted targets. Rejected: the
    adapter supplies the seed (§4.2).

## 12. Open questions

1. **MQTT brokers that only speak 3.1.1.** After the move to MQTT 5, should
   the Tokio backend keep a 3.1.1 mode without traces, selected per broker,
   or require MQTT 5?
2. **Name of the single-input async body.** `task` (used above) or another
   name?

Decided since the previous revision: Tokio MQTT moves to MQTT 5; Serial
leaves the trace off by default; every `record.set` starts a trace; the
single-input async transform ships now; parents are exact (32-bit span IDs);
`trace-context` off is the opt-out for small MCUs.
