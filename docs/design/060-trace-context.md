# 060 — Trace context

**Status:** 📝 Proposed

**Scope:** A trace context on every record value written by an instance: a
trace ID, the write's own span ID, its parent's span ID, and a depth. It is
carried in `WriteMetadata` (058) and propagated along derivations and across
connectors that can carry it. Propagation goes through the existing
transform, join and producer APIs, plus one new transform body for
single-input async derivations. Rerun taps get exact causal chains, and
agents can follow them through the writes they captured or can still read
(§6). Breaking for Rust APIs (join handlers, `WriteMetadata`, session
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
| `span` (8 bytes) | new | new |
| `parent` (8 bytes) | none | the cause's `span` |
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
through a `DerivedProducer<O>`:

```rust
builder.configure::<Alarm>(AlarmKey::Overheat, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .transform::<Temperature, _>(TempKey::Calibrated, |t| {
            t.on_values(|ctx, mut rx, producer| async move {
                while let Some(temp) = rx.recv().await {      // Stamped<Temperature>
                    if temp.value.celsius > 80.0 {
                        producer.produce_from(Alarm::Overheat, temp.metadata());
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
- `rx.recv()` returns `Some(Stamped<I>)` for each value and `None` only when
  the input closes. A lagging receiver skips the values it missed, reports
  them (`TransformEvent::Lagged`, as `map` and `with_state` do) and
  continues, so a burst cannot stop the transform.

### 2.3 Joins

`on_triggers` handlers receive a `DerivedProducer<O>` instead of a
`Producer<O>`. `JoinTrigger` already carries the input's `WriteMetadata`:

```rust
reg.transform_join(|b| b
    .input::<Temperature>(TempKey::Gamma)
    .input::<Humidity>(HumidityKey::Gamma)
    .on_triggers(|mut rx, producer| async move {
        while let Ok(trigger) = rx.recv().await {
            // … update state …
            producer.produce_from(dew_point(&t, &h), trigger.metadata());
        }
    }));
```

```rust
impl<O> DerivedProducer<O> {
    pub fn produce_from(&self, value: O, cause: &WriteMetadata);
    pub fn produce_origin(&self, value: O);   // timer, timeout: an explicit choice
}
```

`DerivedProducer` has no plain `produce`, so a join or an async transform
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
tc.span();       // SpanId, Display = 16 hex chars
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
ignored. No ID seed is needed or read, so an Embassy build without an
entropy source or `.id_seed()` works. This is the opt-out for MCUs that
cannot spare the RAM.

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

/// One write's identity: instance tag (high 32 bits) ‖ write counter (low 32).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId(NonZeroU64);

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
| `Producer::produce`, `DerivedProducer::produce_origin` | `None` |
| `map`, `with_state` | the input |
| `Producer::produce_from`, `DerivedProducer::produce_from` | the given metadata |
| inbound connector with a trace on the wire | the wire trace |
| AimX `write` from a peer's outbound mirror, with a trace | the wire trace (§5.1) |
| inbound connector or AimX `write` without one, AimX `record.set`, persistence restore | `None` |

An inbound value without a trace therefore gets a new ID when, and only
when, it is inserted into a local record. Every AimX `record.set` starts a
trace: a remote client cannot continue one.

### 4.2 IDs

The trace ID and the span ID are W3C-sized (16 and 8 bytes), so a later OTEL
exporter can use both unchanged. A span ID identifies one write across every
instance that takes part in a trace, and a parent field holds the parent's
full span ID, including the instance it was written on:

```text
counter  = instance write counter, starting at 1, + 1 per write
span     = instance_tag (32 bits) ‖ counter (low 32 bits)
trace_id = seed (8 bytes) ‖ counter (8 bytes)   (origin writes only)
```

There is no mixing step: two instances with different tags can never produce
the same span, whatever their counters. (A mixed 32-bit span cannot give
that guarantee: `0x10 ⊕ 1` and `0x13 ⊕ 2` are the same input to the mix, so
two instances can emit the same span, and a mirrored write can end up with
`span == parent`.)

**Guarantee.** Within one trace, two writes have different span IDs if

1. the instances that wrote them have different instance tags, or
2. for a write pair with the same tag, the full 64-bit counter values
   differ by at least 1 and by less than 2³².

Condition 2 holds for every pair of writes that can coexist in one trace,
across restarts. A trace from before a restart can still reach an instance
through a retained MQTT message, an AimX late-join snapshot or a peer's
buffered mirror write, and then mix with writes made after it. So, for a
given tag:

- the counter never goes backwards (a restart with a reset counter repeats
  old values), and
- the counter never advances by 2³² or more between writes that can coexist
  in one trace, counting every skipped range: a `counter_start` after a
  restart, and the collision fast-forward below. (A jump of 2³² wraps the
  low 32 bits and reproduces old spans, and no collision check notices,
  because the parent may come from another tag.)

A deployment that cannot keep that bound uses a fresh tag.

Tag zero is reserved, so a span is never all zero, including at the counter
wrap.

**Instance tags.** By default the tag is the high 32 bits of the instance's
seed (below); if those are zero, a non-zero value derived from the low 32
bits replaces them. The seed is drawn per boot, so a restart repeats the
previous tag only with probability 2⁻³²: unlikely, not impossible. Tags of
n instances are distinct with probability at least 1 − n² / 2³³ (100
instances: about 1 in 860,000 to collide). Deployments that cannot accept
either probability assign tags.

Deployments that need the guarantee without probability assign tags with
`AimDbBuilder::instance_tag(u32)`, for example from a provisioning table.
`instance_tag(0)` is a configuration error. A provisioned tag that is reused
across restarts keeps condition 2 in one of two ways:

- **a distinct tag per boot**, for example a device number combined with a
  persisted boot epoch; or
- **persisted counter progress**: the application saves a high-water mark
  of the counter and restarts just above it with
  `AimDbBuilder::counter_start(u64)`. The restart may skip values the
  instance used without saving them, but the total advance between writes
  that can share a trace must stay below 2³²; if the application cannot
  bound the unsaved writes, it uses a fresh tag instead.

**Detecting a collision.** Two cases can only come from a broken condition
1 or 2, and the instance reports both (`TraceEvent::TagCollision`, counted
with `observability`) while still writing the value:

- **An inbound span carries the instance's own tag with a counter it has not
  reached yet** (another instance with the same tag, or the instance's own
  span from before a restart). The instance moves its counter past that
  value within the current 32-bit window before it writes, so the new span
  differs from the parent and later spans do not repeat it. The move counts
  toward the 2³² bound.
- **A new span would equal its cause's span.** The writer takes the next
  counter value instead. A write never stores `span == parent`.

These are safety nets, not the guarantee: a pre-restart span that never
arrives cannot move the counter, and a repeat whose parent carries another
tag is not visible to either check. Condition 2 still needs one of the
provisioning rules above.

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
tests. With `trace-context` enabled, `build()` fails with a configuration
error if neither supplies a seed. With it off, no seed is needed and none is
read: a seedless Embassy build works.

The counter is a 64-bit atomic where the target has one and a critical
section elsewhere (Cortex-M has no native 64-bit atomics). Every write
touches it; there is no RNG call per write. The seed fills the trace ID's
first 8 bytes and, unless `instance_tag` overrides it, the instance tag.

### 4.3 Memory and time

`WriteMetadata` grows from 8 to 48 bytes (8 stamp + 40 trace: 16 trace ID,
8 span, 8 parent, 1 depth, padded to 8). Every buffer slot pays it:

| `T` | `Stamped<T>` (058) | `Stamped<T>` (060) | Added per slot |
|---|---|---|---|
| `u8`, `f32`, `f64` | 16 | 56 | 40 |
| 12 B, align 4 | 24 | 64 | 40 |
| 24 B, align 8 | 32 | 72 | 40 |

A 16-slot `f32` ring on Embassy goes from 256 to 896 bytes. 32-bit write
identities would not save space: with the 8-byte stamp, the struct pads to
48 bytes either way once a parent carries its instance. With
`trace-context` off (§2.6), nothing changes. The 058 `size_of` assertions
are updated; the allocation gates must show no added allocation.

Measured in the prototype (trace ID and depth only, 32-byte metadata): no
allocation added on the produce, consume, connector or AimX paths;
`record.get` allocates 93 more bytes per call for its longer reply. On the
host, a write that drew from the counter took about 14 ns longer and one
that did not about 3 ns. The prototype guarded the counter with a lock; with
a native atomic, and every write now drawing a span, the cost is re-measured
in phase 1, including on Embassy (`embassy-bench-stm32h5`) with the 48-byte
envelope against tracing off. The 48 bytes are accepted; the benchmark
documents the cost, it does not reopen the decision.

## 5. Wire

One carrier value everywhere:
`<trace_id: 32 hex>/<depth>/<span: 16 hex>/<parent: 16 hex>`, with parent
`0000000000000000` for an origin. For example
`4bf92f3577b34da6a3ce929d0e0e4736/3/9c41e2d000000a1f/5a07b3f100000004`.

| Connector | Carries it as |
|---|---|
| AimX over TCP, UDS, WebSocket | optional `trace` field next to `ts` on every value |
| AimX over Serial | off by default; the connector's configuration turns it on |
| MQTT v5 (both backends, §5.3) | user property `aimdb-trace` |
| Zenoh | attachment key `aimdb-trace` |
| KNX | not carried |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "trace":"4bf92f3577b34da6a3ce929d0e0e4736/3/9c41e2d000000a1f/5a07b3f100000004","data":{"celsius":21.5}}
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
- About 79 bytes per value on AimX (the prototype measured 45 for the ID and
  depth alone; the two span IDs add 34). No connector wraps the payload.
- **The trace never costs a value.** A carrier with a size limit (an MQTT
  broker's maximum packet size, a transport's frame limit) drops the trace
  before it drops the value: if the value fits without the trace but not
  with it, it is sent without one, and the omission is counted per route.
  The receiver starts a trace, as for any untraced value. Only a value that
  does not fit even without the trace is rejected.

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
- **Cost:** the property adds about 84 bytes per publish (1 + 2 + 11 + 2 +
  68).
- **Size limits:** the publish is sized with the property first. If it
  exceeds the broker's maximum packet size or the embedded write ring, it is
  sized again without the property; if it fits, it is sent untraced and
  counted as `trace_omitted` on the route. Only a publish that does not fit
  without the property is rejected as oversize (§5).
- **Other subscribers:** an MQTT 5 broker forwards user properties to MQTT 5
  subscribers unchanged and drops them for 3.1.1 subscribers; the payload
  never changes. A retained message keeps its property, so a late subscriber
  continues the retained write's trace.
- **Brokers that only speak 3.1.1:** not supported. The Tokio backend
  requires MQTT 5; a 3.1.1-only broker refuses the connection, and the
  connector reports it as an MQTT version error naming the broker. Mosquitto
  1.6+, EMQX, HiveMQ and AWS IoT Core speak MQTT 5.

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

Agents read the same through MCP: `get_record` returns the current value
with its span and parent. An agent can follow a parent link only to a write
it captured (a `watch` stream, a Rerun recording) or can still read: if the
parent's record has been written again since, `get_record` returns the newer
value, whose span does not match, and the agent reports the hop as no
longer available instead of guessing. Looking up past writes by span is not
part of this design.

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
| Join handlers | `on_triggers` receives `DerivedProducer<O>` instead of `Producer<O>` |
| `WriteMetadata` | 48 bytes; `trace() -> Option<TraceContext>`; `from_wire_stamp` becomes `from_wire(stamp, Option<TraceContext>)` |
| Session engine | new fields and parameters (§5.2); every `Session` and `EnvelopeCodec` implementation updates |
| Runtime adapters | `RuntimeOps::id_seed()`; Embassy takes its seed in the adapter configuration |
| `build()` | with `trace-context` enabled, fails when neither the adapter nor `.id_seed()` supplies a seed |
| MQTT, Tokio backend | MQTT 5 instead of 3.1.1; 3.1.1-only brokers are no longer supported (§5.3) |
| `TransformBuilder`, `Producer<T>`, `InboundDispatch`, `TopicMatch`, `AimDbBuilder` | additive: `on_values`, `produce_from`, `dispatch_traced`, `trace()`, `instance_tag`, `counter_start` |
| `aimdb-client` | `RecordUpdate` gains `trace` |
| AimX | optional `trace` field, minor version; graph shapes unchanged |

In the prototype, the only user code that had to change was the three
weather-mesh join handlers (`produce` → `produce_from(…, trigger.metadata())`)
and the codegen join stub (`Producer<O>` → `DerivedProducer<O>`).

### 8.1 Release

AimDB has no active user base to protect, so 060 ships as a minor release
although it breaks APIs:

- `aimdb-core` and the adapter and connector crates: 3.x minor.
- `aimdb-client`: 0.9.0.
- AimX protocol: the next minor after 059's 4.1. The wire change is additive.

The CHANGELOG lists every row of the table above under "Breaking changes".

## 9. Phases

1. **Core:** envelope with span and parent, IDs and seeds, automatic
   propagation in `map` / `with_state`, the single-input `on_values` body,
   `DerivedProducer` in joins, `Producer::produce_from`, the `trace-context`
   feature, tests on Tokio, Embassy and WASM. Enough for Rerun taps on one
   instance.
2. **AimX:** `trace` field on values and on mirror `write` frames, session
   engine, client, CLI, MCP; Serial off by default. Enough for chains across
   AimX-linked instances in both directions.
3. **MQTT v5** on both backends, and **Zenoh**.

## 10. Tests

- Source → `map` → `with_state` → `on_values` → join keeps one trace ID; depth
  grows by one per write; each write's `parent` is its cause's `span`.
- Every write gets a new span; within the §4.2 bound (2³² writes) spans in
  one instance do not repeat, and they are never zero.
- **Counter wrap:** with `counter_start(2³² − 1)`, the next two writes get
  `tag‖ffffffff` and `tag‖00000000`; neither is the reserved zero span.
  `instance_tag(0)` fails `build()`, and a seed whose high 32 bits are zero
  yields a non-zero default tag.
- **Retained message after reboot:** a device with provisioned tag 7
  publishes a retained MQTT value with span `7‖1`, restarts with tag 7 and a
  reset counter, and ingests the retained value as its first write. It
  reports `TagCollision`, moves its counter past 1, and stores a span that
  differs from the parent. With `counter_start` set from a persisted
  high-water mark, no collision is reported.
- **Cross-instance identity, deterministic:** two instances whose seeds
  collided under a 32-bit mix (`0x10 ⊕ 1`, `0x13 ⊕ 2`) get different spans;
  A writes, B mirrors it, and B's span differs from its parent. Two
  instances configured with the same `instance_tag` and aligned counters
  report `TagCollision` instead of storing `span == parent` silently.
- `produce_origin` and `Producer::produce` start new traces with no parent;
  `Producer::produce_from` continues one.
- `on_values`: typed receiver; zero, one and several writes per input; the graph
  shows `Transform { input }`.
- **`on_values` after lag:** a burst larger than the input ring makes the
  receiver lag; it reports `Lagged`, continues, and a later overheat still
  emits an alarm. `recv()` returns `None` only when the input closes.
- Every locally written value has `trace()` = `Some`.
- Fixed seed gives identical IDs across runs.
- Seeds: each adapter supplies one; `.id_seed()` overrides it; with
  `trace-context` enabled, `build()` fails without either; the `test_support` runtimes build without
  `.id_seed()` (core's no_std test suites, `aimdb-data-contracts`).
- `trace-context` off: `size_of::<WriteMetadata>() == 8`, `trace()` is
  `None`, AimX frames carry no `trace`, and an Embassy build with no seed
  (no adapter seed, no `.id_seed()`) succeeds on `thumbv7em-none-eabihf`.
- **Counter advance bound:** an instance with tag 7 writes `7‖1` … `7‖5` in
  trace T, saves high-water mark 5, restarts with `counter_start(5 + 100)`,
  and continues T from another device's buffered write: the new span is
  `7‖106`, distinct from every pre-restart span. The same scenario with
  `counter_start(2³² + 1)` is the documented misconfiguration: it reproduces
  `7‖1`, and the test records that no check catches it, which is why §4.2
  makes the bound a provisioning rule.
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
  the trace; a publish without the property starts one. A publish that fits
  the broker's maximum packet size without the property but not with it is
  delivered untraced and counted as `trace_omitted`; one that does not fit
  without the property is rejected as oversize.
- `graph.nodes` and `graph.edges` replies are byte-identical to 4.x for a
  fixture with a transform, an `on_values` transform and a join.
- A value that is migrated on the inbound path keeps its trace.
- **Exact parents:** in the diamond `Raw → A, Raw → B, A + B → C`, each write
  of `C` names the span of the write of `A` or `B` that triggered it; with
  that write missing from a recording, the parent is reported as known but
  not captured.
- **Agent follow-up:** after `Alarm` names a `Temperature` write as its
  parent and `Temperature` is written again, `get_record("Temperature")`
  returns a span that does not match, and the hop is reported as no longer
  available.
- `size_of::<WriteMetadata>() == 48`; allocation gates unchanged.

## 11. Alternatives considered

1. **Merge `transform` and `transform_join` into one builder.** Changes the
   graph shapes on the wire and every transform call, and makes a
   single-input caller downcast its own declared type through the boxed join
   path. Rejected: a typed `on_values` body on the existing single-input builder
   (§2.2).
2. **Parent candidates from the graph instead of stored parents.** Costs
   nothing per slot, but is ambiguous in diamonds, which some pipelines
   have, and agents cannot act on "one of these two". Rejected: store the
   parent span.
3. **32-bit span IDs mixed from salt and counter.** Saves 8 bytes per slot
   (40 instead of 48), but two instances can feed the mix the same input, so
   a span is not unique across instances and a parent cannot name its
   instance.
   Rejected: instance tag ‖ counter (§4.2).
4. **Random 64-bit span IDs.** Needs an RNG call per write and is still only
   probabilistically unique. Rejected: tags are drawn once, and can be
   assigned for a hard guarantee.
5. **Pick a parent by stamp order.** Wrong in the diamond case even with
   every write captured. Rejected.
6. **Mint a trace ID in the client for untraced values.** Two reads of the
   same stored value would report different IDs. Rejected: `None` on read,
   new ID only on local insertion.
7. **Implicit "current trace"** (task-local). Not available on Embassy,
   unclear across `.await`. The cause is an argument instead.
8. **Sampling and a span sink.** No consumer needs them yet.
9. **Plain `produce()` in joins and tasks.** A forgotten cause would silently
   start a new trace. `DerivedProducer` makes it a compile error.
10. **Carry the trace inside the payload** where a protocol has no headers.
   Breaks every non-AimDB consumer.
11. **Defaulted `*_traced` session methods** to keep the session engine
    additive. A session that does not override them drops traces silently,
    and the surface doubles. Rejected: new fields and parameters (§5.2).
12. **Seed from a `std` feature switch in core.** In the prototype it failed
    37 existing tests: crates such as `aimdb-data-contracts` and the WASM
    adapter build core without `std` on hosted targets. Rejected: the
    adapter supplies the seed (§4.2).

## 12. Decisions

All earlier open questions are settled:

| Question | Decision |
|---|---|
| Tokio MQTT backend | moves to MQTT 5 and requires it; no 3.1.1 mode |
| Serial links | trace off by default; the connector's configuration turns it on |
| `record.set` | every remote set starts a trace |
| Single-input async transform | ships now, as `on_values` (§2.2) |
| Parent identity | exact, across instances: 64-bit span ID = instance tag ‖ counter (§4.2) |
| Envelope size | 48 bytes accepted everywhere; `trace-context` off is the opt-out for small MCUs |

No open questions remain.
