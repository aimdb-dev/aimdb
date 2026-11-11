# 060 — Trace context

**Status:** 📝 Proposed

**Scope:** A trace context on every record value written by an instance: a
trace ID, the write's own span ID, its parent's span ID, and a depth. It is
carried in `WriteMetadata` (058) and propagated along derivations and across
connectors that can carry it. Propagation goes through the existing
transform, join and producer APIs, plus one new transform body for
single-input async derivations. Rerun taps get exact parent chains, and
agents can follow them through the writes they captured or can still read
(§6). Breaking for Rust APIs (join handlers, `WriteMetadata`, session
engine, `AimDbBuilder`); additive on the wire (the graph shapes do not
change). Ships as `aimdb-core` 4.0 (§8.1). Tracing is off by default and
enabled with the `trace-context` feature (§2.6).

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
            t.on_stream(|ctx, mut rx, producer| async move {
                while let Ok(temp) = rx.recv().await {        // Stamped<Temperature>
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
- The name is `on_stream`, not `on_values`: `with_state(…).on_value(…)`
  already exists as the synchronous per-value closure, and one letter is too
  small a difference for a body that runs its own loop.
- The handler has the same shape as a join's (§2.3): `|ctx, rx, producer|`,
  and `rx.recv()` returns `Ok(Stamped<I>)` for each value and `Err` only when
  the input closes, as `JoinEventRx::recv` does. A lagging receiver skips the values it missed, reports
  them (`TransformEvent::Lagged`, as `map` and `with_state` do) and
  continues, so a burst cannot stop the transform.

### 2.3 Joins

`on_triggers` handlers receive a `DerivedProducer<O>` instead of a
`Producer<O>`, and the runtime context as a new first argument, so that a
join can use timers (the case `produce_origin` exists for) and has the same
shape as `on_stream`. `JoinTrigger` already carries the input's
`WriteMetadata`:

```rust
reg.transform_join(|b| b
    .input::<Temperature>(TempKey::Gamma)
    .input::<Humidity>(HumidityKey::Gamma)
    .on_triggers(|ctx, mut rx, producer| async move {
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

**A write has one parent.** A join output names the write that triggered
it. The other inputs' values, which the handler kept as state, are not
recorded: in the example above, a dew point triggered by a temperature
continues the temperature's trace and does not mention the humidity write
it was computed with. When the inputs come from different origins, the
other input's trace ends at the join. Recording several causes per write
(OTEL "links") is not part of this design (§6).

### 2.4 Producers

`Producer<T>` keeps `produce`, which starts a trace, and gains
`produce_from(value, &WriteMetadata)` for code outside the graph, for
example a task that bridges an external system. Such writes have an exact
parent, but the graph has no edge for them (the record stays `Passive`).
Derivations from another record should use a transform (§2.2).

`aimdb-sync` is unchanged: `SyncProducer::set` and `set_value` start a
trace, like `produce`. Synchronous code sits outside the graph and has no
cause to pass; a sync `produce_from` can follow if a user needs it.

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

- **In process:** with `trace-context` enabled (§2.6), every value read from
  a local record has `Some`.
- **`aimdb-client`:** `Some` when the peer sent a `trace`, `None` when it did
  not (an older peer, or a link with tracing off). The client never invents
  an ID, so two reads of the same stored value report the same thing.

`produce_from` takes `&WriteMetadata`. If its trace is `None` (metadata
decoded from a peer and passed back in), the write starts a trace.

### 2.6 Opting in

Tracing is off by default. It sits behind the `trace-context` feature of
`aimdb-core`, which is in no crate's default features. Without it,
`WriteMetadata` stays at 8 bytes (§4.3), `trace()` returns `None`, AimX
omits the field and inbound traces are ignored. The API is the same either
way: `DerivedProducer`, `produce_from` and `instance_tag(…)` exist with the
feature off and do nothing extra, so the same application code builds with
tracing on and off, and an MCU that cannot spare the RAM pays nothing.

**Turning it on.** Every crate that depends on core forwards the feature:

```toml
# in each adapter, connector, aimdb-client, aimdb-sync, aimdb-data-contracts
[features]
trace-context = ["aimdb-core/trace-context"]
```

An application enables `trace-context` on any AimDB crate it already lists,
usually its runtime adapter. Cargo unifies features across the dependency
graph, so that one line turns tracing on in core and in every connector the
application uses. (The prototype had it on by default instead; because most
crates in this workspace take core with `default-features = false`, that
left tracing off in all four Embassy examples and in `aimdb-bench`,
silently.)

- **Library crates forward the feature and never enable it.** No crate lists
  `trace-context` in its defaults or turns it on in a dependency. Enabling it
  is the application's decision, because it also makes the instance tag
  mandatory (§4.2): a library that enabled it would make a working
  application fail at `build()`.
- **Applications that want tracing enable it and set a tag.** The examples
  that show tracing do both; the getting-started docs show the feature line
  next to `.instance_tag(…)`.
- **Benchmarks and tests that measure tracing enable it explicitly**, and
  fail if they are built without it.

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
| AimX `write` or `record.set` with a trace | the wire trace (§5.1) |
| inbound connector, AimX `write` or `record.set` without one, persistence restore | `None` |

An inbound value without a trace therefore gets a new ID when, and only
when, it is inserted into a local record. A peer that writes can name a
cause on either path, a mirror `write` or a `record.set`; the trace is
advisory and is not authenticated on either.

### 4.2 IDs

The trace ID and the span ID are W3C-sized (16 and 8 bytes), so a later OTEL
exporter can use both unchanged. A span ID identifies one write across every
instance that takes part in a trace, and a parent field holds the parent's
full span ID, including the instance it was written on:

```text
counter  = instance write counter, starting at 1 at build(), + 1 per write
epoch    = Unix time in seconds (u32) when the instance first had a wall clock; 0 before
span     = instance_tag (32 bits) ‖ counter (low 32 bits)
trace_id = instance_tag (4 bytes) ‖ epoch (4 bytes) ‖ counter (8 bytes)   (origin writes only)
```

There is no mixing step and no randomness: two instances with different
tags can never produce the same span or trace ID, whatever their counters.
(A mixed 32-bit span cannot give that guarantee: `0x10 ⊕ 1` and `0x13 ⊕ 2`
are the same input to the mix, so two instances can emit the same span, and
a mirrored write can end up with `span == parent`.)

**Epoch.** The epoch makes trace IDs differ from one run of an instance to
the next. It comes from the instance clock's `unix_time()` (058):

- The first origin write that finds `unix_time()` available stores its
  seconds as the epoch; from then on the epoch does not change in that run.
  On Tokio and WASM that is the first write. On Embassy it is the first
  write after `set_unix_time` (an RTC read at start, or SNTP).
- Until then the epoch is 0. Changing from 0 to the stored value during a
  run is safe: the counter never repeats within a run, so `(epoch, counter)`
  pairs stay distinct.
- The clock is read once per run, not per write. An origin write costs one
  more atomic load.
- The epoch is in the trace ID only. A span has no room for it (§4.3), so
  span IDs still repeat across restarts (below).

**Instance tags are the application's choice.** The application assigns
each instance its tag with `AimDbBuilder::instance_tag(NonZeroU32)`, for
example from a provisioning table, a device number or a configuration file.
With `trace-context` enabled, `build()` fails with a configuration error if
no tag is set. The framework does not derive tags from randomness, the chip
ID or the runtime: a tag the user chose is distinct by construction, can be
read in a trace, and stays the same across restarts. Tags are `NonZero`, so
a span is never all zero, including at the counter wrap. The `test_support`
builders set a fixed tag, so tests that do not care about it need no change.

**Instances without provisioning.** Browser tabs running the WASM adapter,
scaled replicas and short-lived tools have no table to take a tag from. The
application still supplies the tag, and may draw it at random when the
instance starts (`crypto.getRandomValues` in a browser, the OS RNG on a
host). That is the application's choice and the application's risk: random
tags are distinct only with high probability (100 instances collide with
probability about 1 in 860,000), and §4.2.1 reports a collision when it can
see one. The framework never draws a tag itself, so core and the adapters
need no entropy source.

**Guarantee: spans.** Within one trace, two writes have different span IDs
if

1. the instances that wrote them have different instance tags, or
2. they were written by the same instance in the same run, fewer than 2³²
   writes apart.

**Guarantee: trace IDs.** Two origin writes have different trace IDs if

1. the instances that wrote them have different instance tags, or
2. they were written by the same instance in the same run, or
3. they were written by the same instance in different runs, both runs had
   a wall clock when they wrote, and the two epochs differ.

Keeping tags distinct across instances is the application's
responsibility; §4.2.1 lists what the framework detects when it is not.

**Restarts.** The counter starts at 1 on every `build()`.

- **Trace IDs do not repeat** across a restart when the instance has a wall
  clock: the new run has a later epoch, so its traces never merge with
  traces from before the restart. Three cases are not covered and are
  accepted: an instance that never gets a wall clock (epoch 0 in every
  run), two runs whose first clock readings fall in the same second, and a
  clock set back to an earlier run's epoch. In those cases a new trace can
  reuse the ID of an old one, as it would without the epoch.
- **Span IDs repeat.** A restarted instance reuses its spans from before the
  restart. A trace from before a restart can still reach the instance (a
  retained MQTT message, an AimX late-join snapshot, a peer's buffered
  mirror write) and be continued by new writes; such a trace may then
  contain two writes with the same span, and in rare cases a parent chain
  that returns to a span it already passed. Consumers that walk parents
  must therefore bound the walk (§6).

One case is prevented in the writer, because it would make a write its own
parent:

- **A new span would equal its cause's span.** The writer takes the next
  counter value instead. A write never stores `span == parent`. Without
  this, a restarted instance that ingests its own retained value `tag‖1` as
  its first write would store `tag‖1` with parent `tag‖1`.

### 4.2.1 Detecting duplicate tags

Two instances with the same tag break condition 1 silently in general. The
framework reports what it can see, without changing any write:

- **An inbound span carries the instance's own tag**
  (`TraceEvent::OwnTagInbound`, counted with `observability`). It means one
  of three things: another instance uses the same tag; the instance's own
  write from before a restart has come back (a retained message after a
  reboot); or the instance receives its own publishes (it subscribes to a
  topic it also publishes on). It is a diagnostic, not an error: the last
  two are normal operation. Every occurrence is counted; it is logged once
  per inbound route per connection, so an echoing topic does not fill the
  log.
- **An AimX peer announces the same tag** (`TraceEvent::DuplicateTag`).
  `hello` and `welcome` gain an optional `instance_tag` field, sent by
  instances with `trace-context` enabled (a CLI or MCP client sends none).
  Whichever side sees its own tag reports it and keeps the connection: the
  trace is advisory and never costs a link.
- **Over WebSocket there is no `hello` and no `welcome`** (the client runs
  `sends_hello: false`, the server `reads_hello: false`), so the tag travels
  in the upgrade URL, next to the version: `…/ws?v=4.2&tag=0000a1f3`.
  `ws_url_with_version`, which every first-party dialer already uses, adds
  it when `trace-context` is enabled. Only the server checks, at the
  upgrade; it reports `DuplicateTag` and accepts the connection. A browser
  cannot read the upgrade response, so the client side learns of a
  duplicate only through `OwnTagInbound`.

MQTT and Zenoh peers announce nothing, so a duplicate tag behind a broker
shows up only as `OwnTagInbound`.

### 4.2.2 Counter

The counter is a `portable_atomic::AtomicU64`: a native 64-bit atomic where
the target has one, and a `critical-section` lock elsewhere (Cortex-M has
no native 64-bit atomics). `trace-context` enables `portable-atomic`'s
`fallback` and `critical-section` features, exactly as the `observability`
feature already does, so no new dependency or lock type is needed.
It must be a critical section, not a spin lock: a record written from an
interrupt handler while the main context holds a spin lock would deadlock a
single-core MCU. (The prototype used a spin lock; it is not acceptable for
the implementation.) Every write touches the counter; there is no RNG call,
per write or at `build()`, so no runtime adapter needs an entropy source.

### 4.3 Memory and time

`WriteMetadata` grows from 8 to 48 bytes (8 stamp + 40 trace: 16 trace ID,
8 span, 8 parent, 1 depth, padded to 8) with `trace-context` enabled. Every
buffer slot then pays it:

| `T` | `Stamped<T>` (058) | `Stamped<T>` (060) | Added per slot |
|---|---|---|---|
| `u8`, `f32`, `f64` | 16 | 56 | 40 |
| 12 B, align 4 | 24 | 64 | 40 |
| 24 B, align 8 | 32 | 72 | 40 |

A 16-slot `f32` ring on Embassy goes from 256 to 896 bytes. 32-bit write
identities would not save space: with the 8-byte stamp, the struct pads to
48 bytes either way once a parent carries its instance. With
`trace-context` off, the default (§2.6), nothing changes. The 058 `size_of` assertions
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
(`aimdb-bench` B0 suites), nor on AimX subscription events. In the
prototype `record.get` gained one allocation per call; the implementation
must not (§5). The Embassy benchmark
(`embassy-bench-stm32h5`) runs in phase 1, with the feature enabled, to
document the MCU cost for applications that opt in.

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
| Zenoh, `zenoh://` | the publication's attachment (§5.4) |
| Zenoh, `ros2://` | not carried (§5.4) |
| KNX | not carried |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "trace":"5a07b3f1000000000000000000000002/3/9c41e2d000000a1f/5a07b3f100000004","data":{"celsius":21.5}}
```

- **The carrier can grow.** A decoder reads the first four `/`-separated
  fields and ignores any that follow, so a later version can append fields
  (flags, for example) without older peers treating the value as malformed.
- The field is optional: an older peer or a link with tracing off sends
  none. AimX decoders ignore unknown fields, so this is a minor version.
  `PROTOCOL_VERSION` is 4.0 today and 059, which claims 4.1, is not merged:
  060 takes the next free minor when it lands, 4.2 after 059 or 4.1 before
  it (059 then takes 4.2). This document writes 4.2. Nothing else on the
  wire changes.
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

`record.set` takes the same optional field in its parameters:
`{"name":…,"value":…,"trace":"…"}`. A client that sets a record because of
a value it read passes that value's trace and the write continues it;
without the field the write starts a trace. This is what lets an agent's
action stay in the chain that prompted it (§7). A session with tracing off
(§5.0) ignores the field on both frames.

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

### 5.4 Zenoh

Not prototyped; phase 3 starts with a prototype of this section.

- **`zenoh://`:** a Zenoh 1.x publication has one attachment, an opaque
  byte string with no keys. The connector sets it to the carrier value as
  ASCII bytes (68 to 70 bytes). Inbound, an attachment that does not parse
  as a carrier is treated as absent, like any malformed trace (§5): the
  value is kept. Publications from non-AimDB peers that use the attachment
  for something else are therefore ingested untraced.
- **`ros2://`:** not carried. The attachment belongs to rmw_zenoh, which
  requires its fixed 33-byte layout (sequence number, source timestamp,
  GID) on every publication, and ROS 2 peers reject any other. A value
  ingested from ROS 2 starts a trace; a value published to ROS 2 ends its
  trace there.
- **Embedded backend:** the `zenoh-nostd` backend does not exist yet; it
  carries the trace the same way once it does.

## 6. Rerun taps and agents

A Rerun tap is a read-only `tap` on a Tokio instance. For each value it logs
the value, the trace ID, its span, its parent and the depth, and the stamp on
that instance's own timeline. Each instance also logs its dependency graph
once. MCUs run no Rerun taps; their values show up at the hub with their
trace intact.

**Parents are exact, and there is one per write.** A write's parent is the
write in the same trace whose span equals its `parent`. That holds in the diamond case (`Raw → A`,
`Raw → B`, `A + B → C`): each write of `C` names the write of `A` or `B`
that triggered it. It does not name the other one: a chain shows what
triggered each write, not every value that went into it (§2.3).

**Walking parents.** A consumer that follows parent links stops when the
next write's depth is not lower than the current one's (below 255), or when
it meets a span it has already visited. Span IDs can repeat across a restart
(§4.2), so a chain is not guaranteed to be free of cycles; with this rule a
walk always ends.

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
- `aimdb set <record> … --cause <trace>` and MCP `set_record` with an
  optional `cause` pass a trace to `record.set` (§5.1), so a write made in
  response to a value continues that value's trace.

## 8. What breaks

| Area | Change |
|---|---|
| Join handlers | `on_triggers` receives `(ctx, rx, DerivedProducer<O>)` instead of `(rx, Producer<O>)` |
| `WriteMetadata` | 48 bytes; `trace() -> Option<TraceContext>`; `from_wire_stamp` becomes `from_wire(stamp, Option<TraceContext>)` |
| Session engine | new fields and parameters (§5.2); every `Session` and `EnvelopeCodec` implementation updates |
| WebSocket connector | its public `Snapshot` type gains the trace; its fan-out carries it (§5.2) |
| Serial connector | tracing off by default; `.trace_default(bool)` on the server (§5.0) |
| `AimDbBuilder` | `instance_tag(NonZeroU32)`; with `trace-context` enabled, `build()` fails without it (§4.2) |
| Cargo features | `trace-context` on `aimdb-core`, off by default; every crate that depends on core forwards it (§2.6) |
| `InboundDispatch` | `dispatch` takes `Option<TraceContext>`; no untraced variant (§5.2) |
| AimX handshake | `hello` and `welcome` gain an optional `instance_tag` (§4.2.1) and `trace` (§5.0); the WebSocket upgrade URL gains an optional `tag` parameter |
| AimX `record.set` | optional `trace` parameter (§5.1) |
| Zenoh connector | `zenoh://` publications carry the trace as their attachment (§5.4) |
| MQTT, Tokio backend | MQTT 5 instead of 3.1.1; 3.1.1-only brokers are no longer supported (§5.3) |
| `TransformBuilder`, `Producer<T>`, `TopicMatch` | additive: `on_stream`, `produce_from`, `trace()` |
| `aimdb-client` | `RecordUpdate` gains `trace` |
| AimX | optional `trace` field, minor version; graph shapes unchanged |

In the prototype, the user code that had to change was the join handlers
(three weather-mesh stations, the Tokio and WASM join tests:
`produce` → `produce_from(…, trigger.metadata())`) and the codegen join stub
(`Producer<O>` → `DerivedProducer<O>`). The `ctx` argument adds one name to
each of those closures. An application that enables
`trace-context` also adds one `instance_tag(…)` line to its builder; the
prototype derived tags from a seed and did not measure that.

### 8.1 Release

060 breaks Rust APIs, so it ships as a major release:

- `aimdb-core`: 4.0.0. The adapter and connector crates take their next
  major version with it.
- `aimdb-client`: 0.9.0 (a breaking bump for a 0.x crate).
- AimX protocol: the next free minor, 4.2 if 059 has landed (§5). The wire
  change is additive; the protocol version is independent of the crate
  version.
- **One release for all three phases.** The phases of §9 are a merge order,
  not releases: the session engine and `InboundDispatch` changes land in
  phase 2 and the MQTT change in phase 3, and all of them are breaking. 4.0.0
  is published only after phase 3 is merged, so no second major release is
  needed.

The CHANGELOG lists every row of the table above under "Breaking changes".

## 9. Phases

The phases are merged in this order and released together (§8.1).

1. **Core:** envelope with span and parent, IDs and the required instance tag, automatic
   propagation in `map` / `with_state`, the single-input `on_stream` body,
   `DerivedProducer` in joins, `Producer::produce_from`, the epoch from
   `unix_time()`, the `trace-context` feature, off by default, and its
   forwarding feature in every dependent crate (§2.6), tests on Tokio,
   Embassy and WASM. Enough for Rerun taps on one instance.
2. **AimX:** `trace` field on values and on mirror `write` frames, session
   engine (WebSocket fan-out included), client, CLI, MCP; Serial off by
   default with the per-session request in `hello` (§5.0); the `record.get`
   reply without an added allocation; `record.set` with a cause; tags in the
   handshake and in the WebSocket upgrade URL. Enough for
   chains across AimX-linked instances in both directions.
3. **MQTT v5** on both backends, with the Tokio oversize check, and
   **Zenoh**, starting with a prototype of §5.4.

## 10. Tests

- Source → `map` → `with_state` → `on_stream` → join keeps one trace ID; depth
  grows by one per write; each write's `parent` is its cause's `span`.
- Every write gets a new span; within one run and the §4.2 bound (2³²
  writes) spans in one instance do not repeat, and they are never zero.
- **Counter wrap:** with the counter set to 2³² − 1 through a test-only
  hook, the next two writes get `tag‖ffffffff` and `tag‖00000000`; neither
  is the zero span.
- **Tag required:** with `trace-context` enabled, `build()` without
  `instance_tag` fails; with it off, it succeeds, and `instance_tag(…)` is
  accepted and ignored. The `test_support` builders build without one.
- **Origin trace ID:** without a wall clock, an origin write with tag 7 and
  counter 5 has trace ID `00000007 00000000 0000000000000005`; with
  `unix_time()` at 1 700 000 000 s it has `00000007 6553f100 0000000000000005`.
- **Epoch:** it is read once: a later clock change does not change the trace
  IDs of the run. On a clock that gains wall time after three origin writes,
  those three have epoch 0 and the following ones the stored epoch; all
  trace IDs of the run are distinct.
- **Restart with a wall clock:** an instance with tag 7 writes, is rebuilt
  with a clock one second later and writes again; no trace ID of the second
  run equals one of the first. Without a wall clock the IDs repeat.
- **Parent walk ends:** on a recording with a repeated span that closes a
  parent cycle, a walk stops at the repeated span (§6).
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
  sides report `DuplicateTag`, and values still flow. Over WebSocket the
  client's upgrade URL carries `tag`, and only the server reports it. A CLI
  client without a tag triggers nothing.
- **Own publishes:** an instance subscribed to a topic it publishes on
  counts `OwnTagInbound` for every message and logs it once.
- `produce_origin` and `Producer::produce` start new traces with no parent;
  `Producer::produce_from` continues one.
- `on_stream`: typed receiver; zero, one and several writes per input; the graph
  shows `Transform { input }`.
- **`on_stream` after lag:** a burst larger than the input ring makes the
  receiver lag; it reports `Lagged`, continues, and a later overheat still
  emits an alarm. `recv()` returns `Err` only when the input closes.
- With `trace-context` enabled, every locally written value has `trace()` =
  `Some`.
- The same tag, the same writes and a clock without wall time (or a fixed
  one) give identical IDs across runs.
- `trace-context` off (the default): `size_of::<WriteMetadata>() == 8`, `trace()` is
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
- `record.set` without `trace` starts a trace; with one, the write continues
  it with the given span as parent; a malformed one is treated as absent.
- **Carrier growth:** a carrier with a fifth field decodes to the same trace
  as the first four alone.
- **Join, one parent:** a dew point triggered by a temperature write has the
  temperature's trace ID and span as parent, and nothing of the humidity
  write.
- **Zenoh:** a `zenoh://` publication carries the carrier as its attachment
  and the receiver continues the trace; a foreign attachment is ingested
  untraced; a `ros2://` publication keeps rmw_zenoh's attachment and the
  receiver starts a trace.
- **MQTT v5:** a publish carries `aimdb-trace`; a receiving instance continues
  the trace; a publish without the property starts one. A publish that fits
  the broker's maximum packet size without the property but not with it is
  delivered untraced and counted as `trace_omitted`; one that does not fit
  without the property is rejected as oversize.
- `graph.nodes` and `graph.edges` replies are byte-identical to AimX 4.1 for a
  fixture with a transform, an `on_stream` transform and a join.
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
- **Feature reach:** a default build of every crate has tracing off
  (`size_of::<WriteMetadata>() == 8`). Enabling `trace-context` on any one
  adapter, connector or `aimdb-client` turns it on in core. An Embassy
  example builds with it on, and a benchmark that measures tracing fails if
  it builds without it.
- **Counter on Cortex-M:** writing a record from an interrupt handler while
  the main context writes does not deadlock.
- **MQTT oversize (Tokio):** a publish over the broker's maximum packet size
  is rejected before sending, and the connection survives it.

## 11. Alternatives considered

1. **Merge `transform` and `transform_join` into one builder.** Changes the
   graph shapes on the wire and every transform call, and makes a
   single-input caller downcast its own declared type through the boxed join
   path. Rejected: a typed `on_stream` body on the existing single-input builder
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
    application handles correctly. Rejected: the trace ID takes an epoch
    from the wall clock the instance already has (§4.2), which needs neither.
    Spans have no room for it, so span repeats across a restart are
    accepted; `span == parent` is prevented and parent walks are bounded
    (§6).
15. **Tracing on by default.** Every application would pay 40 bytes per
    buffer slot and need an instance tag without asking for either, and a
    feature that makes `build()` fail without new configuration must not be
    reachable through a dependency's defaults. Rejected: off by default,
    forwarded by every dependent crate (§2.6).
16. **A timestamp in every span or on every write.** A span would need more
    than 64 bits to hold tag, time and counter, which grows the envelope and
    leaves the W3C span size; reading the wall clock per write costs time on
    the hot path. Rejected: one epoch per run, in the trace ID only.

## 12. Decisions

All earlier open questions are settled:

| Question | Decision |
|---|---|
| Tokio MQTT backend | moves to MQTT 5 and requires it; no 3.1.1 mode |
| Serial links | trace off by default; a client turns it on per session in `hello`, the server's configuration changes the default (§5.0) |
| `record.set` | continues a trace when the caller passes one, otherwise starts one (§5.1) |
| Single-input async transform | ships now, as `on_stream`, with the join handler's shape (§2.2) |
| Causes per write | one: the triggering write; a join's other inputs are not recorded (§2.3) |
| Zenoh | `zenoh://` carries the trace as the attachment; `ros2://` does not (§5.4) |
| WebSocket tag check | tag in the upgrade URL, checked by the server only (§4.2.1) |
| Parent identity | exact, across instances: 64-bit span ID = instance tag ‖ counter (§4.2) |
| Instance tag | assigned by the application, required with `trace-context`; no seed, no entropy source |
| Restarts | trace IDs carry a wall-clock epoch and do not repeat across a restart when the instance has a wall clock; span IDs may repeat; `span == parent` is prevented and parent walks are bounded |
| Default | `trace-context` is off by default; every crate that depends on core forwards the feature (§2.6) |
| Release | `aimdb-core` 4.0.0, published once after all three phases; AimX takes the next free minor (4.2 after 059) |
| Envelope size | 48 bytes for applications that enable `trace-context`; 8 bytes otherwise |

No open questions remain. Two parts are decided but not prototyped, and
their phases start by checking them: the tag exchange in the handshake and
the upgrade URL (§4.2.1), and the Zenoh attachment (§5.4).

## 13. Prototype results

The second throwaway prototype implemented §2 to §5 except Zenoh and the
CLI/MCP display. It was not merged. It derived tags from a per-boot seed and
had `counter_start` for restarts; both have since been replaced (§4.2,
alternatives 12 and 14), and the rows below for them are kept as history.
The handshake tag exchange (§4.2.1) was not prototyped, nor were the
wall-clock epoch (§4.2), `record.set` with a cause (§5.1) and the `ctx`
argument of join handlers (§2.3); the prototype's single-input body was
called `on_values` and returned `Option` from `recv`.

| Claim | Result |
|---|---|
| Propagation through `map`, `with_state`, `on_stream` and joins; parent = cause's span | holds |
| Exact parents in the diamond | holds, over 20 diamonds |
| Cross-instance parents over AimX (UDS), both directions | holds |
| Cross-instance parents over MQTT 5, both backends, real broker | holds |
| Review example `0x10 ⊕ 1` / `0x13 ⊕ 2` | distinct spans |
| Same tag, aligned counters | collision reported, no self-parent |
| Retained value after reboot | collision reported, no self-parent |
| Counter wrap, tag zero | no zero span |
| `on_stream` after lag | skips, reports, a later value still produces |
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
