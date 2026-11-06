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
engine, `AimDbBuilder`); additive on the wire (the graph shapes do not
change). Ships as `aimdb-core` 4.0 (§8.1).

Two throwaway prototypes tested this design. The second implements all of
it except Zenoh, the CLI/MCP display and Rerun; §13 lists what it showed, and
its measurements are quoted in §4.3, §5 and §5.3.

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
ignored. No instance tag is required: `instance_tag(…)` is still accepted
and ignored, so the same application code builds with tracing on and off.
This is the opt-out for MCUs that cannot spare the RAM.

**What "on by default" reaches.** Cargo features unify across the
application's dependency graph, so tracing is on when any crate in it enables
`aimdb-core/trace-context`. The feature is in the default features of
`aimdb-core`, `aimdb-embassy-adapter` and `aimdb-wasm-adapter`. But 25 of the
44 crates in this workspace that depend on core do so with
`default-features = false`, and every Embassy example and connector disables
the Embassy adapter's defaults. In the prototype that left tracing off in
all four Embassy examples and in `aimdb-bench`, silently. So:

- **Applications that disable default features list `trace-context`.** The
  Embassy examples do (phase 1), and the Embassy getting-started docs show
  it with `.instance_tag(…)`.
- **Library crates (connectors, `aimdb-data-contracts`) never enable or
  disable it.** They follow the application.
- **Benchmarks and tests that measure tracing enable it explicitly.**

## 3. Dependency graph

No change. `RecordOrigin`, `EdgeType` and the AimX `graph.*` replies keep
their AimX 4.x shapes (`{"transform":{"input":…}}`,
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
counter  = instance write counter, starting at 1 at build(), + 1 per write
span     = instance_tag (32 bits) ‖ counter (low 32 bits)
trace_id = instance_tag (4 bytes) ‖ 0 (4 bytes) ‖ counter (8 bytes)   (origin writes only)
```

There is no mixing step and no randomness: two instances with different
tags can never produce the same span or trace ID, whatever their counters.
(A mixed 32-bit span cannot give that guarantee: `0x10 ⊕ 1` and `0x13 ⊕ 2`
are the same input to the mix, so two instances can emit the same span, and
a mirrored write can end up with `span == parent`.) The four zero bytes of
the trace ID are reserved; a later exporter may fill them, for example with
a boot value (below).

**Instance tags are the application's choice.** The application assigns
each instance its tag with `AimDbBuilder::instance_tag(NonZeroU32)`, for
example from a provisioning table, a device number or a configuration file.
With `trace-context` enabled, `build()` fails with a configuration error if
no tag is set. The framework does not derive tags from randomness, the chip
ID or the runtime: a tag the user chose is distinct by construction, can be
read in a trace, and stays the same across restarts. Tags are `NonZero`, so
a span is never all zero, including at the counter wrap. The `test_support`
builders set a fixed tag, so tests that do not care about it need no change.

**Guarantee.** Within one trace, two writes have different span IDs if

1. the instances that wrote them have different instance tags, or
2. they were written by the same instance in the same run, fewer than 2³²
   writes apart.

Keeping tags distinct across instances is the application's
responsibility; §4.2.1 lists what the framework detects when it is not.

**Restarts are not protected.** The counter starts at 1 on every `build()`,
so a restarted instance repeats its span and trace IDs from before the
restart. A trace from before a restart can still reach the instance (a
retained MQTT message, an AimX late-join snapshot, a peer's buffered mirror
write) and mix with new writes; such a trace may then contain two writes
with the same span. AimDB instances are long-running and restarts are rare,
so this is accepted. One case is still prevented, because it would make a
write its own parent:

- **A new span would equal its cause's span.** The writer takes the next
  counter value instead. A write never stores `span == parent`. Without
  this, a restarted instance that ingests its own retained value `tag‖1` as
  its first write would store `tag‖1` with parent `tag‖1`, a cycle that
  breaks every consumer walking parents.

### 4.2.1 Detecting duplicate tags

Two instances with the same tag break condition 1 silently in general. The
framework reports what it can see, without changing any write:

- **An inbound span carries the instance's own tag**
  (`TraceEvent::OwnTagInbound`, counted with `observability`). It means
  either another instance uses the same tag, or the instance's own write
  from before a restart has come back (a retained message after a reboot).
  It is a diagnostic, not an error: the second cause is normal operation.
- **An AimX peer announces the same tag** (`TraceEvent::DuplicateTag`).
  `hello` and `welcome` gain an optional `instance_tag` field, sent by
  instances with `trace-context` enabled (a CLI or MCP client sends none).
  Whichever side sees its own tag reports it and keeps the connection: the
  trace is advisory and never costs a link. The WebSocket server does not
  read `hello`, so over WebSocket only the client side checks.

MQTT and Zenoh peers announce nothing, so a duplicate tag behind a broker
shows up only as `OwnTagInbound`.

### 4.2.2 Counter

The counter is a 64-bit atomic where the target has one, and a
`critical-section` lock elsewhere (Cortex-M has no native 64-bit atomics).
It must be a critical section, not a spin lock: a record written from an
interrupt handler while the main context holds a spin lock would deadlock a
single-core MCU. (The prototype used a spin lock; it is not acceptable for
the implementation.) Every write touches the counter; there is no RNG call,
per write or at `build()`, so no runtime adapter needs an entropy source.

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

The sizes are compile-time assertions in `aimdb-core` (48 bytes with
`trace-context`, 8 without), checked on the host, `thumbv7em-none-eabihf`
and `wasm32-unknown-unknown`.

Measured in the second prototype (48-byte envelope, native atomic counter,
release build, host): a write takes about 10–12 ns longer than on `main`,
origins and continued writes alike, since every write now draws a span.

| Operation | `main` | prototype |
|---|---|---|
| `produce` (origin) | 104.7 ns | 116.2 ns |
| `produce_from` | n/a | 115.0 ns |
| `produce` + `try_recv` | 112.8 ns | 125.2 ns |

No allocation is added on the produce, consume and connector paths
(`aimdb-bench` B0 suites), nor on AimX subscription events. `record.get`
gains one allocation per call (§5). The Embassy benchmark
(`embassy-bench-stm32h5`) runs in phase 1 to document the MCU cost; the 48
bytes are accepted, so it does not reopen the decision.

## 5. Wire

One carrier value everywhere:
`<trace_id: 32 hex>/<depth>/<span: 16 hex>/<parent: 16 hex>`, with parent
`0000000000000000` for an origin. For example
`5a07b3f1000000000000000000000002/3/9c41e2d000000a1f/5a07b3f100000004`.

| Connector | Carries it as |
|---|---|
| AimX over TCP, UDS, WebSocket | optional `trace` field next to `ts` on every value |
| AimX over Serial | off by default; a client turns it on for its session in `hello` (§5.0), the server's configuration can change the default |
| MQTT v5 (both backends, §5.3) | user property `aimdb-trace` |
| Zenoh | attachment key `aimdb-trace` |
| KNX | not carried |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "trace":"5a07b3f1000000000000000000000002/3/9c41e2d000000a1f/5a07b3f100000004","data":{"celsius":21.5}}
```

- The field is optional: an older peer or a link with tracing off sends
  none. AimX decoders ignore unknown fields, so this is the next minor
  version after 059's 4.1. Nothing else on the wire changes.
- Absence means different things on the two paths, with the same frame:
  - **observing** (client `get`, `watch`, `drain`): the value has no trace,
    `trace()` is `None`;
  - **ingesting** (inbound connector writing locally): the local write starts
    a trace.
- A malformed field (bad hex, depth above 255, zero trace ID, a span or
  non-zero parent with tag zero, `span == parent`) is treated as absent: the value is kept and its trace is `None`. The trace is
  advisory and must never cost a value.
- A receiver that ingests a traced value continues it: same ID, depth + 1,
  a new span, and the sender's span as parent.
- 79 bytes per value on AimX at depth 0–9, 80 at 10–99, 81 above
  (measured: an `event` with a small value grows from 73 to 152 bytes). No
  connector wraps the payload.
- **`record.get` must not gain an allocation.** The AimX encoder serializes
  a reply frame with `serde_json::to_vec`, which starts at 128 bytes and
  doubles. With the trace, a small `record.get` reply crosses 256 bytes
  (190 → 268) and pays one more reallocation: in the prototype, 17 → 18
  allocations per call and +704 bytes. The implementation assembles the
  reply around the already exactly sized `{"value","ts","trace"}` object,
  as subscription events already do, so the allocation gate holds.
  Subscription events, which reuse their buffer, were unaffected.
- **The trace never costs a value.** A carrier with a size limit (an MQTT
  broker's maximum packet size, a transport's frame limit) drops the trace
  before it drops the value: if the value fits without the trace but not
  with it, it is sent without one, and the omission is counted per route.
  The receiver starts a trace, as for any untraced value. Only a value that
  does not fit even without the trace is rejected.

### 5.0 Per-session trace request

Whether an AimX session carries traces is a session setting with a
per-transport default, not a property of the codec: on by default over
TCP, UDS and WebSocket, off over Serial, where every byte counts. The
server's connector configuration can change the default
(`.trace_default(bool)`).

A client overrides the default for its own session with an optional `trace`
field in `hello`:

```json
{"version":"4.2","client":"aimdb-profiler","trace":true}
```

- `true` turns tracing on for this session, `false` turns it off, and
  absence keeps the default. `welcome` echoes the effective setting, so a
  client knows whether a missing `trace` means "untraced value" or "tracing
  off".
- The setting covers both directions of the session: with it off, the
  server sends no `trace` and ignores one it receives (on `write` frames
  too), exactly as for a peer with tracing off.
- On Serial the setting lasts until the next `hello`, which starts a new
  session (the serial connector's existing rule). A client attaching to an
  MCU over a UART, for example a profiler, gets traced values without a
  firmware rebuild.
- The WebSocket server does not read `hello`; it always uses its default.
- A server built with `trace-context` off cannot trace: it answers
  `"trace":false` in `welcome` whatever the client asked.

There is one `AimxCodec`; the session tells it per frame whether to write
the `trace` field. `AimxCodecUntraced` is not needed.

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
WebSocket connector's session pass it to the record write. For the
WebSocket connector that reaches further than its `Session`: its fan-out bus
(`ClientManager::broadcast`) and its late-join snapshot cache carry the trace
too, and its public `Snapshot` type gains a field. Inbound connectors
call `InboundDispatch::dispatch(topic, payload, trace)`; the ingest
closure receives it through a new `TopicMatch::trace()` accessor, so
`IngestFn` keeps its signature. There is no untraced `dispatch` next to it:
a connector that kept calling one would drop traces without notice.
Transports without a carrier (KNX) pass `None`.

### 5.3 MQTT v5

Both MQTT backends speak MQTT 5 and carry the trace as one user property.

- **Tokio backend:** moves from `rumqttc`'s 3.1.1 client to `rumqttc::v5`.
  Outbound: the publish loop already receives each value's `WriteMetadata`
  (`OutboundMessage::metadata`) and publishes with
  `user_properties: [("aimdb-trace", "<carrier>")]`. Inbound: the event loop
  reads the property from each `Publish` and passes it to `dispatch`.
- **Embassy backend (`mountain-mqtt`):** already speaks MQTT 5 and accepts up
  to 32 properties per inbound publish, but neither reads nor writes user
  properties. `deliver` reads `aimdb-trace`; `publish_staged` adds it.
- **Cost:** the property adds 84 bytes per publish (1 + 2 + 11 + 2 + 68;
  measured: a small publish grows from 33 to 117 bytes).
- **Size limits:** the publish is sized with the property first. If it
  exceeds the broker's maximum packet size or the embedded write ring, it is
  sized again without the property; if it fits, it is sent untraced and
  counted as `trace_omitted` on the route. Only a publish that does not fit
  without the property is rejected as oversize (§5).
- **Oversize on the Tokio backend is new.** Today only the Embassy backend
  skips oversize publishes. `rumqttc` sends any publish, and in the
  prototype a publish over the broker's limit closed the whole connection;
  publishes after it were lost until the reconnect. With MQTT 5 the broker
  announces its maximum packet size in CONNACK, so the Tokio backend checks
  each publish against it before sending, rejects an oversize one, and the
  connection survives.
- **Tested** against mosquitto 2.0.18 with both backends (the Embassy
  backend over `TokioNet`): the receiver continues the publisher's trace
  with its write as parent; with `max_packet_size 160`, a value that fits
  only without the property arrives untraced and is counted; an oversize
  value is rejected and a later publish still arrives. The 3.1.1 refusal
  path is implemented but untested: no 3.1.1-only broker was available.
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
| WebSocket connector | its public `Snapshot` type gains the trace; its fan-out carries it (§5.2) |
| Serial connector | tracing off by default; `.trace_default(bool)` on the server (§5.0) |
| `AimDbBuilder` | `instance_tag(NonZeroU32)`; with `trace-context` enabled, `build()` fails without it (§4.2) |
| `InboundDispatch` | `dispatch` takes `Option<TraceContext>`; no untraced variant (§5.2) |
| AimX handshake | `hello` and `welcome` gain an optional `instance_tag` (§4.2.1) and `trace` (§5.0) |
| MQTT, Tokio backend | MQTT 5 instead of 3.1.1; 3.1.1-only brokers are no longer supported (§5.3) |
| `TransformBuilder`, `Producer<T>`, `TopicMatch` | additive: `on_values`, `produce_from`, `trace()` |
| `aimdb-client` | `RecordUpdate` gains `trace` |
| AimX | optional `trace` field, minor version; graph shapes unchanged |

In the prototype, the user code that had to change was the join handlers
(three weather-mesh stations, the Tokio and WASM join tests:
`produce` → `produce_from(…, trigger.metadata())`) and the codegen join stub
(`Producer<O>` → `DerivedProducer<O>`). The required instance tag adds one
line to every application builder; the prototype derived tags from a seed
and did not measure that.

### 8.1 Release

060 breaks Rust APIs, so it ships as a major release:

- `aimdb-core`: 4.0.0. The adapter and connector crates take their next
  major version with it.
- `aimdb-client`: 0.9.0 (a breaking bump for a 0.x crate).
- AimX protocol: 4.2, the next minor after 059's 4.1. The wire change is
  additive; the protocol version is independent of the crate version.

The CHANGELOG lists every row of the table above under "Breaking changes".

## 9. Phases

1. **Core:** envelope with span and parent, IDs and the required instance tag, automatic
   propagation in `map` / `with_state`, the single-input `on_values` body,
   `DerivedProducer` in joins, `Producer::produce_from`, the `trace-context`
   feature and its listing in the Embassy examples (§2.6), tests on Tokio,
   Embassy and WASM. Enough for Rerun taps on one instance.
2. **AimX:** `trace` field on values and on mirror `write` frames, session
   engine (WebSocket fan-out included), client, CLI, MCP; Serial off by
   default with the per-session request in `hello` (§5.0); the `record.get` reply without an added allocation; tags in the
   handshake. Enough for
   chains across AimX-linked instances in both directions.
3. **MQTT v5** on both backends, with the Tokio oversize check, and
   **Zenoh**.

## 10. Tests

- Source → `map` → `with_state` → `on_values` → join keeps one trace ID; depth
  grows by one per write; each write's `parent` is its cause's `span`.
- Every write gets a new span; within one run and the §4.2 bound (2³²
  writes) spans in one instance do not repeat, and they are never zero.
- **Counter wrap:** with the counter set to 2³² − 1 through a test-only
  hook, the next two writes get `tag‖ffffffff` and `tag‖00000000`; neither
  is the zero span.
- **Tag required:** with `trace-context` enabled, `build()` without
  `instance_tag` fails; with it off, it succeeds, and `instance_tag(…)` is
  accepted and ignored. The `test_support` builders build without one.
- **Origin trace ID:** an origin write with tag 7 and counter 5 has trace ID
  `00000007 00000000 0000000000000005`.
- **Retained message after reboot:** a device with tag 7 publishes a
  retained MQTT value with span `7‖1`, restarts, and ingests the retained
  value as its first write. It reports `OwnTagInbound` and stores span `7‖2`
  with parent `7‖1`: never `span == parent`.
- **Cross-instance identity, deterministic:** instances with tags `0x10` and
  `0x13` get different spans at any counters; A writes, B mirrors it, and
  B's span differs from its parent. Two instances configured with the same
  tag and aligned counters store no `span == parent` and report
  `OwnTagInbound`.
- **Duplicate tag over AimX:** two instances with the same tag connect; both
  sides (client side only over WebSocket) report `DuplicateTag`, and values
  still flow. A CLI client without a tag triggers nothing.
- `produce_origin` and `Producer::produce` start new traces with no parent;
  `Producer::produce_from` continues one.
- `on_values`: typed receiver; zero, one and several writes per input; the graph
  shows `Transform { input }`.
- **`on_values` after lag:** a burst larger than the input ring makes the
  receiver lag; it reports `Lagged`, continues, and a later overheat still
  emits an alarm. `recv()` returns `None` only when the input closes.
- Every locally written value has `trace()` = `Some`.
- The same tag and the same writes give identical IDs across runs.
- `trace-context` off: `size_of::<WriteMetadata>() == 8`, `trace()` is
  `None`, AimX frames carry no `trace`, and an Embassy build without
  `instance_tag` succeeds on `thumbv7em-none-eabihf`.
- A malformed `trace` field on a value or `write` frame (including a tag-zero
  span and `span == parent`) keeps the value and decodes as `None`.
- **Legacy frame, both paths:** the same AimX value frame without `trace`
  - decoded twice by `aimdb-client` gives `None` both times;
  - ingested by an inbound connector gives a local write with a new trace at
    depth 0.
- AimX round-trip of a traced value; ingesting it continues the trace with
  the sender's span as parent.
- **Mirror push:** instance A mirrors a record to instance B over AimX; B
  stores A's trace ID, A's depth + 1 and A's span as parent. A `write` frame
  without `trace` starts a trace on B. The same over the WebSocket connector.
- **Per-session trace request:** over Serial, a session without `trace` in
  `hello` gets no `trace` fields and the server ignores inbound ones; a
  client sending `"trace":true` gets traced events and `welcome` reports
  `"trace":true`; the next client's `hello` without the field is untraced
  again. Over UDS, `"trace":false` turns tracing off for that session only.
  A server built with `trace-context` off answers `"trace":false`.
- `record.set` always starts a trace.
- **MQTT v5:** a publish carries `aimdb-trace`; a receiving instance continues
  the trace; a publish without the property starts one. A publish that fits
  the broker's maximum packet size without the property but not with it is
  delivered untraced and counted as `trace_omitted`; one that does not fit
  without the property is rejected as oversize.
- `graph.nodes` and `graph.edges` replies are byte-identical to AimX 4.1 for a
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
- `size_of::<WriteMetadata>() == 48` (8 with `trace-context` off), asserted
  at compile time on every target; allocation gates unchanged, including
  `record.get`.
- **Feature reach:** the Embassy examples build with `trace-context` on; a
  benchmark that measures tracing fails if it builds without it.
- **Counter on Cortex-M:** writing a record from an interrupt handler while
  the main context writes does not deadlock.
- **MQTT oversize (Tokio):** a publish over the broker's maximum packet size
  is rejected before sending, and the connection survives it.

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
3. **32-bit span IDs mixed from seed and counter.** Saves 8 bytes per slot
   (40 instead of 48), but two instances can feed the mix the same input, so
   a span is not unique across instances and a parent cannot name its
   instance.
   Rejected: instance tag ‖ counter (§4.2).
4. **Random 64-bit span IDs.** Needs an RNG call per write and is still only
   probabilistically unique. Rejected: tags assigned by the application.
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
12. **Instance tags derived from a per-boot seed** that each runtime adapter
    supplies (`RuntimeOps::id_seed`), with an optional override. Needs an
    entropy source on every platform (an Embassy adapter field, the WASM
    `Crypto` feature), and tags are only probabilistically distinct: 100
    instances collide with probability about 1 in 860,000, and a restart can
    repeat the previous tag. Rejected: the application assigns tags (§4.2).
    (The prototype also showed that a `std` feature switch in core is the
    wrong place for a seed: 37 tests failed, because several crates build
    core without `std` on hosted targets.)
13. **A spin lock for the counter on targets without 64-bit atomics.**
    Simple, but deadlocks a single-core MCU when an interrupt handler writes
    a record while the main context holds it. Rejected: `critical-section`.
14. **Uniqueness across restarts** with a persisted counter high-water mark
    (`counter_start`) or a distinct tag per boot. Needs persistent storage
    or provisioning on every device, and only covers the restarts the
    application handles correctly. Rejected: AimDB instances are
    long-running and restarts are rare; repeats across a restart are
    accepted, and only `span == parent` is prevented (§4.2).

## 12. Decisions

All earlier open questions are settled:

| Question | Decision |
|---|---|
| Tokio MQTT backend | moves to MQTT 5 and requires it; no 3.1.1 mode |
| Serial links | trace off by default; a client turns it on per session in `hello`, the server's configuration changes the default (§5.0) |
| `record.set` | every remote set starts a trace |
| Single-input async transform | ships now, as `on_values` (§2.2) |
| Parent identity | exact, across instances: 64-bit span ID = instance tag ‖ counter (§4.2) |
| Instance tag | assigned by the application, required with `trace-context`; no seed, no entropy source |
| Restarts | IDs may repeat across a restart; only `span == parent` is prevented |
| Release | `aimdb-core` 4.0.0; AimX 4.2 |
| Envelope size | 48 bytes accepted everywhere; `trace-context` off is the opt-out for small MCUs |

No open questions remain.

## 13. Prototype results

The second throwaway prototype implemented §2 to §5 except Zenoh and the
CLI/MCP display. It was not merged. It derived tags from a per-boot seed and
had `counter_start` for restarts; both have since been replaced (§4.2,
alternatives 12 and 14), and the rows below for them are kept as history.
The handshake tag exchange (§4.2.1) was not prototyped.

| Claim | Result |
|---|---|
| Propagation through `map`, `with_state`, `on_values` and joins; parent = cause's span | holds |
| Exact parents in the diamond | holds, over 20 diamonds |
| Cross-instance parents over AimX (UDS), both directions | holds |
| Cross-instance parents over MQTT 5, both backends, real broker | holds |
| Review example `0x10 ⊕ 1` / `0x13 ⊕ 2` | distinct spans |
| Same tag, aligned counters | collision reported, no self-parent |
| Retained value after reboot | collision reported, no self-parent |
| Counter wrap, tag zero | no zero span |
| `on_values` after lag | skips, reports, a later value still produces |
| Tracing off without an ID source | builds; no_std suites pass with tracing on and off |
| 48 / 8 bytes | asserted on host, Cortex-M, wasm32 |
| Wire cost | AimX +79 bytes, MQTT +84 bytes |
| Allocations | none added, except `record.get` (+1, fixed by §5) |
| Write time | +10–12 ns per write on the host |
| Existing suites | pass (987 tests in the touched crates), codegen drift passes |

Findings that changed this document: tracing's reach through
`default-features = false` (§2.6); a critical section, not a spin lock, for
the counter (§4.2.2); the `record.get` allocation (§5); the WebSocket fan-out
and snapshot cache (§5.2); the Tokio MQTT oversize check (§5.3); the
adapter and connector API changes (§8).
