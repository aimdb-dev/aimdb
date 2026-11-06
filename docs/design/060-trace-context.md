# 059 — Trace context in the record envelope

**Status:** 📝 Proposed

**Scope:** Correlation ID for record values (issue TBD). A W3C
`traceparent`-compatible trace context in `WriteMetadata` (058), propagated
along every derivation inside an instance and across connectors that can
carry it; `tap_write`, which makes tap-driven writes edges of the dependency
graph; a write API in which every derived write states its cause; an optional
span sink for recording; AimX 4.1, which carries the context on every value;
and `aimdb-client`, `aimdb` CLI and `aimdb-mcp` support. Breaking for Rust
APIs (join handlers, connector dispatch, graph types) and additive on the
wire.

Rerun taps (§8) are the first consumer: trace context on every value plus
the dependency graph is enough to reconstruct causal chains, with no
recording infrastructure. The Rerun integration itself, OTEL export and the
`probe` primitive are separate designs built on this one.

---

## 1. The rule

> Every successful record write carries a trace context. A write either
> **starts** a trace (an origin) or **continues** the trace of the value that
> caused it. The cause is stated by the writer, and every derivation is an
> edge of the dependency graph.

> The trace identity (`trace_id`, flags) is inherited from the cause. The
> span identity (`span_id`) is new for every write. `depth` is the cause's
> depth plus one. The stamp (058) stays local and is never inherited.

A trace context means **"this value belongs to the causal chain that started
at that origin, and this write is step `depth` in it."**

| Part | Comes from | New per write |
|---|---|---|
| `stamp` (058) | local instance clock | yes |
| `span_id` | local ID source | yes |
| `trace_id` | the cause, or new at an origin | no |
| `flags` (sampled, forced) | the cause, or the record's policy at an origin | no |
| `depth` | cause's depth + 1, or 0 at an origin | — |

The two halves of `WriteMetadata` answer different questions: the stamp says
**when** a value was written here, the trace context says **why**. The 058
rule "timestamps are never inherited or imported" is unchanged; this design
adds a field that is inherited, explicitly.

Together with the dependency graph, `trace_id` and `depth` identify a write's
parent: the write in the same trace, at `depth − 1`, on one of the record's
graph inputs (§8.2). The parent span is therefore not stored with the value.

What a trace context does **not** say:

- That a value was *computed from* every value of its trace. Each write has
  one cause; a join's other inputs are not part of its trace.
- Anything about time across instances. Per-hop times exist only as each
  instance's own stamps.
- That a trace that starts at a record started in the physical world. A trace
  also starts at a connector that cannot carry context (§9.2) and at an
  explicit `produce_origin` in derived code (§3.2).

## 2. Requirements

| # | Requirement |
|---|---|
| R1 | Every write carries a trace context. Propagation is always on; sampling only decides recording (§7). |
| R2 | Single transforms propagate with no user code. |
| R3 | Every other derived write states its cause, enforced by the writer's type: no default, no silent origin. |
| R4 | Every derivation inside an instance is an edge of the dependency graph, so the graph plus `trace_id` and `depth` reconstruct parents (§8). |
| R5 | Fixed size, `Copy`, `no_std`, no allocation added. No RNG call per write. |
| R6 | Tokio, Embassy and WASM, in the shared conformance suite. Under Tokio paused time with a fixed seed, IDs are reproducible. |
| R7 | W3C `traceparent` / `tracestate` at every external boundary, without misusing reserved flag bits. |
| R8 | A connector either carries the context or is declared a trace boundary; a boundary is visible in the graph. |
| R9 | Breaking changes are accepted where they make the API state causality. No compatibility shims. |

**Non-goals:**

- the Rerun integration itself (crate, contract → archetype mapping, layout),
  OTEL export and the `probe` primitive
- per-consumer events (tap reads, outbound sends) as spans
- MCU span forwarding (an MCU propagates and stamps; it records nothing)
- storing trace context in `aimdb-persistence`
- tail-based sampling
- clock synchronisation or cross-instance time arithmetic

## 3. User-facing API

### 3.1 What does not change

Sources, single transforms, links and read-only taps look exactly as they do
today. The values they produce and receive are traced.

```rust
builder.configure::<Temperature>(TempKey::Raw, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
       .source(|ctx, producer| async move {
           loop {
               producer.produce(read_sensor().await);  // origin: new trace
               ctx.time().sleep_secs(1).await;
           }
       })
       .tap(temperature_logger)                        // read-only: unchanged
       .link_to("mqtt://sensors/temp/raw");            // carries the context
});

builder.configure::<Temperature>(TempKey::Calibrated, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
       .transform(TempKey::Raw, |t| t.map(|raw| Some(calibrate(raw))));
       // each output continues its input's trace, automatically
});
```

### 3.2 Taps that write: `tap_write`

`.tap()` stays read-only. A tap that writes another record declares that
record as its target and receives a writer for it:

```rust
builder.configure::<Temperature>(TempKey::Calibrated, |reg| {
    reg.tap_write::<Alarm>(AlarmKey::Overheat, overheat_watch);
});

async fn overheat_watch(
    ctx: RuntimeContext,
    temps: Consumer<Temperature>,
    alarm: DerivedWriter<Alarm>,
) {
    let mut reader = temps.subscribe();
    while let Ok(t) = reader.recv().await {
        if t.value.celsius > 80.0 {
            alarm.produce_from(Alarm::Overheat, &t);   // continues t's trace
        }
    }
}
```

```rust
impl<T> RecordRegistrar<'_, T> {
    pub fn tap<F, Fut>(&mut self, f: F) -> &mut Self
    where F: FnOnce(RuntimeContext, Consumer<T>) -> Fut;            // unchanged

    pub fn tap_write<O, F, Fut>(&mut self, target: impl RecordKey, f: F) -> &mut Self
    where F: FnOnce(RuntimeContext, Consumer<T>, DerivedWriter<O>) -> Fut;
}
```

- `build()` checks that `target` exists and holds `O`, as it does for
  transform inputs.
- The graph gains the edge input → target (§4).
- One target per `tap_write`. A tap that writes several records is several
  `tap_write`s, or a compound record (open question 4).

**`DerivedWriter<O>`** is the writer of derived code. It has no plain
`produce`; every write names its cause:

```rust
impl<O> DerivedWriter<O> {
    /// Continues the trace of `cause`.
    pub fn produce_from(&self, value: O, cause: &impl AsCause);

    /// Starts a new trace from derived code: a timer, a timeout, a
    /// heartbeat. Explicit, so it is a decision and not an accident.
    pub fn produce_origin(&self, value: O);

    pub fn try_produce_from(&self, value: O, cause: &impl AsCause) -> Result<(), TryProduceError<O>>;
    pub fn try_produce_origin(&self, value: O) -> Result<(), TryProduceError<O>>;
}

/// Anything that names the write that caused this one.
pub trait AsCause { fn trace_context(&self) -> TraceContext; }
// Implemented for: &Stamped<T>, WriteMetadata, JoinTrigger, TraceContext.
```

### 3.3 Joins

`transform_join` handlers receive a `DerivedWriter<O>` instead of a
`Producer<O>` (breaking). They state the cause with the trigger they react
to:

```rust
.transform_join(|b| {
    b.input::<Temperature>(TempKey::Gamma)
     .input::<Humidity>(HumidityKey::Gamma)
     .on_triggers(|mut rx, writer| async move {
         let (mut t, mut h) = (None, None);
         while let Ok(trigger) = rx.recv().await {
             match trigger.index() {
                 0 => t = trigger.as_input::<Temperature>().cloned(),
                 1 => h = trigger.as_input::<Humidity>().cloned(),
                 _ => {}
             }
             if let (Some(t), Some(h)) = (&t, &h) {
                 writer.produce_from(dew_point(t, h), &trigger);
             }
         }
     })
})
```

A join that emits on its own schedule (for example, a timeout with stale
inputs) uses `produce_origin`.

### 3.4 Sources and external writers

`Producer<T>`, handed to `.source()` and returned by `db.producer()`, keeps
`produce` as the origin call and gains the explicit variants:

```rust
impl<T> Producer<T> {
    pub fn produce(&self, value: T);                                // origin
    pub fn produce_forced(&self, value: T);                         // origin, forced (§7)
    pub fn produce_from(&self, value: T, cause: &impl AsCause);     // bridge an external cause
    // try_* variants mirror these.
}
```

`produce_from` on a `Producer` bridges a cause from outside AimDB, for example
an HTTP request that already has a `traceparent`:

```rust
let tc = TraceContext::from_w3c(req.header("traceparent"), req.header("tracestate"))?;
setpoint.produce_from(new_setpoint, &tc);
```

A `db.producer()` handle is not an edge of the graph: its record stays
`Passive` (§4). Code that reads a record and writes another should be a
`tap_write`; passing a record value to `db.producer().produce_from` works,
but its parent cannot be reconstructed from the graph (§8.3).

`AimDb::produce(key, value)` and `aimdb-sync` gain the same `_from` and
`_forced` variants.

### 3.5 Reading the context

```rust
let t = reader.recv().await?;
let tc: TraceContext = t.trace();       // like t.stamp()

tc.trace_id();     // TraceId, Display = 32 hex chars
tc.span_id();      // SpanId,  Display = 16 hex chars
tc.depth();        // u8: writes since the origin
tc.is_sampled();
tc.is_forced();
tc.is_valid();     // false only with the `trace-context` feature off (§5.4)

log.info(&format!("{} {}", t.value, tc));  // 00-4bf92f…-00f067aa…-01
```

`aimdb-client` returns the same `Stamped<T>`, decoded from the wire (§9.1),
so `.trace()` works on remote values.

### 3.6 Per-record recording policy

```rust
reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
   .trace(TracePolicy::one_in(100))     // record 1 in 100 traces started here
   .source(sensor_task);
```

| Policy | Meaning |
|---|---|
| `TracePolicy::never()` | default; traces started here are not sampled |
| `TracePolicy::always()` | every trace started here is sampled |
| `TracePolicy::one_in(n)` | integer, no floats; decided from the origin's `span_id` |

The policy applies only where a trace starts, and only to recording (§7).
It never affects propagation: every value has a valid context, which is all
a Rerun tap needs (§8).

### 3.7 What users have to learn

| Who | New |
|---|---|
| Sources, transforms, links, read-only taps | nothing |
| Taps that write other records | `tap_write` + `produce_from(value, &cause)` |
| Join handlers | `DerivedWriter`: `produce_from(value, &trigger)` |
| Debugging | `.trace()`, `aimdb get --trace`, `aimdb trace` |
| Experiments | `produce_forced`, `aimdb set --force-trace` |
| Tuning | `.trace(TracePolicy::…)` |

## 4. Dependency graph

Today a tap is an edge with no target (`to: None`), and a record written
from a tap through `db.producer()` is `Passive`: the graph does not know the
data flow exists. `tap_write` closes that gap.

```rust
pub enum RecordOrigin {
    Source,
    Link { protocol: String, address: String },
    Transform { input: String },
    TransformJoin { inputs: Vec<String> },
    TapWrite { inputs: Vec<String> },     // new: every record that tap_writes into this one
    Passive,
}

pub enum EdgeType {
    Source,
    Link { protocol: String },
    Transform,
    TransformJoin,
    Tap { index: usize },                 // read-only, `to: None`, unchanged
    TapWrite { index: usize },            // new: `from: input`, `to: Some(target)`
    LinkOut { protocol: String },
}
```

- Several `tap_write`s may target one record (fan-in); their inputs are
  collected in `TapWrite { inputs }`.
- A `tap_write` target cannot also have a source, transform, join or inbound
  link: one kind of origin per record, as today. `build()` rejects the mix.
- `tap_write` edges may close a cycle (a controller writing a setpoint it
  also reads). Transforms keep cycle rejection; a cycle through a
  `tap_write` is accepted and bounded at runtime by the depth limit (§5.5).
  Open question 8.
- `aimdb graph`, AimX graph introspection and MCP show the new edges, and
  mark boundary links (§9.2).

## 5. Inside the database

### 5.1 Types

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId([u8; 16]);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId([u8; 8]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceContext {
    trace_id: TraceId,
    span_id: SpanId,
    depth: u8,
    flags: TraceFlags,
}

/// AimDB's own flags. Mapped to W3C only at the boundary (§9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceFlags(u8);   // bit 0 = sampled, bit 1 = forced

pub struct WriteMetadata {
    stamp: Stamp,
    trace: TraceContext,     // new
}
```

All fields are private behind accessors, as 058 §4.1 requires. W3C defines
an all-zero trace or span ID as invalid; AimDB never generates one, which is
what `is_valid()` checks.

**Why AimDB flags, not W3C flags.** W3C Trace Context Level 2 assigns
`trace-flags` bit 1 to *random-trace-id*. Using it for *forced* would make
every forced trace claim a random ID. Internally both bits are AimDB's; at
the boundary, *sampled* maps to W3C bit 0 and *forced* travels in
`tracestate` (§9.1).

### 5.2 The envelope

`WriteMetadata` holds the stamp and the trace context; nothing else changes
in `Stamped<T>`. The parent span is not in the slot: the graph plus
`trace_id` and `depth` identify it (§8.2), and the span sink receives it at
write time (§7). Keeping it out saves 8 bytes per slot in every buffer.

`WriteMetadata::from_wire_stamp(stamp)` is replaced by:

```rust
impl WriteMetadata {
    /// Metadata decoded from a peer's wire value, or built in tests. Never
    /// reaches a buffer; a write takes the context through `Cause::Remote`.
    pub fn from_wire(stamp: Stamp, trace: Option<TraceContext>) -> Self;
    pub fn trace(&self) -> TraceContext;
}
```

### 5.3 Writing

`RecordWriter::push` takes the cause:

```rust
pub(crate) enum Cause {
    Origin { forced: bool },
    Derived(TraceContext),
    Remote(TraceContext),     // restored from a carrier
    Boundary,                 // inbound without a carrier, or without a context
    Restore,                  // aimdb-persistence on startup
}

impl RecordWriter<T> {
    pub(crate) fn push(&self, value: T, cause: Cause);
}
```

It remains the one place that creates `WriteMetadata` for a buffer. Per
write: one clock read (058), one ID from the instance's ID source (§5.6),
and, if the result is sampled or forced and a sink is installed, one
`SpanSink::record` call (§7).

| Writer | Cause |
|---|---|
| `Producer::produce` | `Origin { forced: false }` |
| `Producer::produce_forced` | `Origin { forced: true }` |
| `Producer::produce_from(v, c)` | `Derived(c)` |
| single transform (`map`, stateful) | `Derived(input)`, by the framework |
| `DerivedWriter::produce_from(v, c)` (`tap_write`, join) | `Derived(c)` |
| `DerivedWriter::produce_origin(v)` | `Origin { forced: false }` |
| inbound connector, context present | `Remote(ctx)` |
| inbound connector, no carrier or no context | `Boundary` |
| AimX `record.set` with `tp` | `Remote(ctx)` |
| AimX `record.set` without `tp` | `Origin` |
| `aimdb-persistence` restore | `Restore` (an origin) |

From the cause:

- **Origin / Boundary / Restore:** `trace_id` = instance salt ‖ this
  write's `span_id` (§5.6); `depth = 0`; *sampled* from the record's
  `TracePolicy`; *forced* from the cause.
- **Derived / Remote:** `trace_id` and flags from the cause;
  `depth = cause.depth + 1`.

### 5.4 Feature flag

`trace-context`, default on in every adapter. Off makes `TraceContext` a
zero-sized type: `WriteMetadata` is back to 8 bytes, `.trace()` returns an
invalid context, the AimX encoder omits `tp`, and inbound contexts are
dropped (every inbound write is a `Boundary`). The API is the same either
way, so no code is written twice (058 R6).

### 5.5 Depth

`depth` saturates at `TRACE_DEPTH_LIMIT` (64). A write whose cause is at the
limit starts a new trace instead, with kind `DepthReset`; the span sink
records a link to the cause. This bounds feedback loops through `tap_write`
cycles without cutting real pipelines, which are far shorter.

### 5.6 IDs

No RNG call per write. Once at `build()`, the instance takes a 64-bit
**salt**; every write takes the next value of a 64-bit counter:

```text
span_id  = mix64(salt ⊕ counter.fetch_add(1))     // bijective finalizer (splitmix64)
trace_id = salt ‖ span_id(origin write)
```

- `mix64` is a bijection, so span IDs are unique within an instance for 2⁶⁴
  writes, and look random across instances.
- A trace ID's first half names the instance where the trace started, which
  tooling can show.
- IDs are not random in the W3C sense, so the W3C *random-trace-id* bit is
  never set.
- The counter is a `portable_atomic::AtomicU64` (the `critical-section`
  fallback already used by `observability` covers Cortex-M without 64-bit
  atomics).

The salt comes from the runtime:

| Adapter | Salt |
|---|---|
| Tokio | `getrandom` |
| WASM | `crypto.getRandomValues` |
| Embassy | supplied by the application: `.id_seed(u64)` on the builder |
| Tokio paused-time tests | `.id_seed(u64)` overrides, for reproducible IDs |

On Embassy, a hardware RNG or the chip's unique ID mixed with a persisted boot
counter are both fine seeds; the docs show both. Without a seed, `build()`
fails with `DbError::MissingIdSeed` rather than silently producing IDs that
collide across devices (open question 1).

### 5.7 Memory

`WriteMetadata` grows from 8 to 40 bytes (8 + 16 + 8 + 1 + 1, padded to 8).
Every slot pays it, as 058 §4.5 anticipated.

| `T` | `Stamped<T>` (058) | `Stamped<T>` (059) | Added per slot |
|---|---|---|---|
| `u8`, `bool` | 16 | 48 | 32 |
| `f32`, `u32` | 16 | 48 | 32 |
| `f64`, `u64` | 16 | 48 | 32 |
| 12 B, align 4 | 24 | 56 | 32 |
| 24 B, align 8 | 32 | 64 | 32 |

- 16-slot `EmbassyBuffer<f32>` ring: 256 → 768 bytes of element storage.
- `SingleLatest` / `Mailbox`: 32 bytes more per record.

The 058 `size_of` assertions are updated to these values, and the
`aimdb-bench` allocation gates must show zero added allocations. Targets that
cannot afford it turn off `trace-context` (§5.4).

## 6. Joins and links

A join's output continues the trace of the trigger the handler names. The
other inputs belong to other traces; they are not parents. Where they are
needed (to show *which* humidity value a dew point used), they are:

- recorded by the span sink as links: `JoinEventRx` keeps the latest
  `WriteMetadata` per input (it already receives it with every trigger) and
  passes up to `MAX_LINKS` (4) span references plus a dropped count with the
  write; or
- reconstructed from the graph: the latest write of the other input with a
  stamp at or before the output's, on the same instance (§8.2).

Links never live in the slot.

## 7. Recording

Propagation is always on; recording is opt-in. An instance may install one
span sink. Rerun taps (§8) do not need one; it serves OTEL export, `aimdb
trace` and cases the graph cannot resolve.

```rust
pub trait SpanSink: Send + Sync {
    /// Called in the write path for sampled or forced writes. Must not block
    /// or allocate per call; a full sink drops and counts.
    fn record(&self, span: &SpanRecord);
}

pub struct SpanRecord {
    pub trace: TraceContext,
    pub parent: Option<SpanId>,          // the cause's span, if any
    pub record: RecordId,
    pub stamp: Stamp,
    pub kind: SpanKind,                  // Origin, Derived, Remote, Boundary,
                                         // Restore, DepthReset, Set
    pub links: [Option<SpanRef>; MAX_LINKS],
    pub links_dropped: u16,
}
```

```rust
builder.span_sink(MemorySpanSink::with_capacity(4096));
```

- `MemorySpanSink` (`std`) is a bounded ring with a dropped counter, and backs
  the AimX `trace.get` method (§9.1) and `aimdb trace`.
- Embassy installs none. An MCU's values arrive at the first Tokio hop with
  their context, so the hop's spans name the MCU span as parent; the MCU span
  itself is not recorded (non-goal).

## 8. Consumer: Rerun taps

### 8.1 Shape

A Rerun tap is a plain read-only `.tap()` on a Tokio instance that logs each
value to a Rerun recording shared by all instances of a run. It needs nothing
beyond phase 1 (plus phases 2 and 4 for chains across instances):

- the value, under `<instance>/<record>`, and under
  `traces/<trace_id>/<instance>/<record>` so one chain can be isolated by
  entity path;
- a per-instance `stamp` timeline (058; stamps are not comparable across
  instances);
- a `depth` sequence timeline, the one ordering that holds across instances
  within a trace;
- each instance's dependency graph, once, as static data.

Because propagation does not depend on sampling, the tap sees `trace_id` and
`depth` on every value. MCUs do not run Rerun taps; their values appear at
the hub records they arrive in, with their context intact.

### 8.2 Reconstructing parents

The parent of write *W* (trace *T*, depth *d*, record *R*, instance *I*) is
the write in *T* at depth *d − 1* on a graph input of *R*:

| *R*'s origin | Graph inputs |
|---|---|
| `Transform { input }` | `input` |
| `TransformJoin { inputs }` | `inputs` (only the trigger is in *T*) |
| `TapWrite { inputs }` | `inputs` |
| `Link { address }` | the record behind the matching outbound address on another instance |
| `Source`, `Passive` | none: *W* is an origin, or its cause is outside the graph |

If several writes match, take the latest with a stamp at or before *W*'s on
the same instance. Cross-instance graphs are joined by link address
(`mqtt://sensors/temp/raw` out on the MCU, in on the hub).

| Case | Result |
|---|---|
| Linear chains, fan-out, joins, `tap_write` | exact |
| Across instances via links | exact; an upstream record without a tap shows as a known hop with no value |
| Same input written twice in one trace at one depth (diamond, burst) | stamp order; wrong only while a transform lags behind its input |
| `db.producer().produce_from(…)` | no graph edge: falls back to "the only write in *T* at *d − 1*", else unresolved |
| Untapped or lagging intermediate record | gap; the graph names the missing hop |
| Depth reset (§5.5) | chain continues as a new trace; the link is in the span sink only |

The guarantees this relies on — every in-instance derivation is a graph
edge (R4), depth grows by exactly one per write, a remote write is one
deeper than the sender's — are tested in the conformance suite (§12).

## 9. Wire

### 9.1 AimX 4.1

Two optional fields on every value, next to `ts`:

| Field | Content |
|---|---|
| `tp` | W3C `traceparent`, version `00`: `00-<trace_id>-<span_id>-<flags>` |
| `tst` | W3C `tracestate`, only when AimDB state is not default: `aimdb=d:<depth>;f:<0|1>` |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "tp":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
 "tst":"aimdb=d:3;f:0","data":{"celsius":21.5}}
```

- Present on `event`, `snap`, and on the value objects of `record.get`,
  `record.drain` and `record.set` replies.
- Accepted as optional params on `record.set`, so a client can continue a
  trace or force one (`aimdb set --force-trace`).
- New method `trace.get {trace_id}` returns the span records the server's
  sink holds for that trace, or `NoSpanSink`.

**Optional by meaning, not for compatibility.** An absent `tp` means "no
context": the receiving write is a `Boundary`. That is the state of a peer
built without `trace-context` or a link opted out with `?trace=off`, so the
field stays optional even though breaking changes are acceptable. Because it
is additive and the AimX decoders do not deny unknown fields,
`PROTOCOL_VERSION` becomes `"4.1"`, and 4.0 peers interoperate as
boundaries.

**Cost.** About 62 bytes per value for `tp`, plus about 20 for `tst`. On a
115 200 baud serial link that is roughly 7 ms per value. A link can opt out
with `?trace=off`; it is then a trace boundary.

### 9.2 Connectors

A connector either carries `traceparent` / `tracestate` or is a declared
boundary. The inbound dispatch path takes an `Option<TraceContext>` (`None` →
`Cause::Boundary`); the outbound path already receives the value's
`WriteMetadata` (for `ts`) and reads the context from it.

| Connector | Carrier | Status |
|---|---|---|
| TCP, UDS, Serial, WebSocket (AimX) | `tp` / `tst` fields | phase 2 |
| MQTT, Embassy (`mountain-mqtt`, v5) | user properties `traceparent`, `tracestate` | phase 4; properties are not surfaced by the connector today |
| MQTT, Tokio (`rumqttc`, 3.1.1 client) | none: boundary | until the backend moves to `rumqttc::v5` (open question 3) |
| Zenoh | attachment keys `traceparent`, `tracestate` | phase 4 |
| KNX | none: boundary | frame size |

No connector ever wraps the payload to carry context: that would break every
non-AimDB consumer of the same topic.

## 10. Client and tools

- **`aimdb-client`:** values decode `tp`/`tst` into `WriteMetadata::from_wire`.
- **CLI:**
  - `aimdb get <record> --trace` and `aimdb watch <record> --trace` print
    the context with each value.
  - `aimdb set <record> <value> --trace <traceparent>` continues a trace;
    `--force-trace` starts a forced one and prints its trace ID.
  - `aimdb trace <trace_id>` lists the server's span records for that trace,
    in causal order (needs a span sink).
  - `aimdb graph` shows `tap_write` edges and marks boundary links
    (`⟂ trace restarts here`).
- **MCP:** `get_record` includes the context; `set_record` accepts
  `force_trace` and returns the trace ID; new `get_trace` tool; the graph
  tools show `tap_write` edges. Together that is the loop `probe` will
  automate: force a write, then follow its trace.

```text
$ aimdb trace 4bf92f3577b34da6a3ce929d0e0e4736
instance  record                  kind      depth  parent
mcu-07    sensor.temp.raw         origin    0      —            (not recorded)
edge-01   sensor.temp.raw         remote    1      00f067aa…    mqtt://sensors/temp/raw
edge-01   hub.dew_point           derived   2      a3ce929d…    join; link: humidity
```

## 11. What breaks

| Area | Change |
|---|---|
| Join handlers | `on_triggers` receives `DerivedWriter<O>` instead of `Producer<O>`; every write names its cause or is `produce_origin` |
| Taps that write other records | move from a captured `db.producer()` to `tap_write` (the old way compiles, but leaves the graph without the edge) |
| Graph types | `RecordOrigin::TapWrite`, `EdgeType::TapWrite`; consumers of the graph JSON (CLI, MCP, codegen) update |
| `WriteMetadata` | grows to 40 bytes; `from_wire_stamp` replaced by `from_wire`; gains `trace()` |
| Connector implementations | inbound dispatch takes `Option<TraceContext>` |
| Embassy applications | `build()` requires `.id_seed(…)` while `trace-context` is on |
| `Producer<T>` | additive: `produce_from`, `produce_forced`, `try_*` |
| AimX | 4.1, additive; 4.0 peers interoperate as boundaries |
| Memory | +32 bytes per buffer slot (§5.7) |

## 12. Phases

1. **Core.** Types, `WriteMetadata`, ID source and seeds, `Cause` through
   `RecordWriter`, `Producer` variants, `DerivedWriter`, `tap_write` with its
   graph edges and build checks, join handlers on `DerivedWriter`, automatic
   propagation in single transforms, depth limit, `Stamped::trace`,
   `trace-context` feature. Conformance suite on Tokio, Embassy and WASM;
   size assertions; allocation gates. Enough for single-instance Rerun taps.
2. **AimX 4.1.** `tp`/`tst` on values and `record.set`, client decoding, CLI
   `--trace` / `--force-trace`, MCP `get_record` / `set_record`. Enough for
   Rerun taps across AimX-linked instances.
3. **Recording.** `SpanSink`, `MemorySpanSink`, join links, `trace.get`,
   `aimdb trace`, MCP `get_trace`, graph boundary marks.
4. **Carriers.** MQTT v5 user properties (Embassy), Zenoh attachment; decision
   on the Tokio MQTT backend.

## 13. Tests

- **Conformance (all adapters):** source → transform → join → `tap_write`
  keeps one `trace_id`; span IDs differ per write; depth grows by exactly one
  per write.
- `DerivedWriter::produce_from` continues the trace; `produce_origin` starts
  a new one; `Producer::produce_forced` sets *forced*, and every descendant
  has it.
- `tap_write`: the graph has the input → target edge and the target's origin
  is `TapWrite`; `build()` rejects a missing target, a type mismatch, and a
  target that also has another origin kind.
- Depth limit: a `tap_write` cycle of two records resets at 64.
- Joins: the output continues the trigger's trace; with a sink, links name
  the other inputs' latest spans; more than `MAX_LINKS` inputs count as
  dropped.
- **Reconstruction:** for a fixture pipeline (chain, fan-out, join,
  `tap_write`, a link between two instances), the §8.2 rule applied to the
  logged values and graphs yields the same parents the span sink records.
- IDs: fixed seed under paused time gives identical IDs across runs
  (snapshot); `mix64` round-trip; no all-zero ID is ever generated.
- AimX: `tp`/`tst` encode/decode round-trip; a 4.0 decoder fixture ignores
  them; a value without `tp` produces a `Boundary` write with depth 0.
- Inbound migration: a value decoded, migrated (`aimdb-data-contracts`) and
  written keeps its `trace_id`.
- W3C: parse/format against the spec examples; *forced* never sets W3C bit 1.
- `size_of::<WriteMetadata>() == 40`; slot sizes as §5.7; `aimdb-bench`
  allocation gates unchanged; bytes per AimX message documented.

## 14. Alternatives considered

1. **Implicit ambient context** (task-local "current span"). Not available on
   Embassy, ambiguous across `.await` and fan-out. Rejected: the cause is an
   argument.
2. **Plain `produce()` everywhere, with a lint for unattributed origins.**
   Avoids breaking join handlers, but a forgotten cause fails silently and is
   only caught after the fact. Rejected: derived code gets `DerivedWriter`,
   which has no default.
3. **A cause on every write, including sources.** Sources are origins by
   definition; making them say so adds noise without catching anything.
   Rejected: `Producer::produce` stays the origin call.
4. **`tap` with an optional target** instead of a separate `tap_write`.
   Mixes read-only and writing taps in one signature and makes the writer an
   `Option`. Rejected: `tap` stays read-only; `tap_write` declares its
   target.
5. **Parent span in the slot.** +8 bytes per slot; the graph plus `trace_id`
   and `depth` identify the parent for every declared derivation, and the
   sink records it exactly where needed. Rejected.
6. **A span per pipeline stage** (buffer, tap, outbound). A slot is shared by
   all readers and cannot hold per-reader state. Rejected: a span is a write.
7. **W3C flag bit 1 for *forced*.** Collides with Level 2 *random-trace-id*.
   Rejected: AimDB flags internally, `tracestate` on the wire.
8. **Per-hop timestamps in the envelope.** Variable-length and mixes clocks.
   Rejected: each hop's stamp is its own write's.
9. **Trace context as a contract field.** Contracts describe values, not
   their provenance, and migrations would have to carry it. Rejected: the
   envelope.
10. **RNG per write.** Costly on MCUs and unnecessary for uniqueness.
    Rejected: seed once, counter plus bijective mix.
11. **Payload wrapper for MQTT 3.1.1.** Breaks every non-AimDB subscriber.
    Rejected: boundary.
12. **Sampling gates propagation.** Would leave most values without a
    context, and Rerun taps with nothing to correlate. Rejected: always
    propagate, sample recording.
13. **Mandatory `tp` (AimX 5.0).** Breaking is acceptable, but absence has a
    meaning (boundary) that a mandatory field would have to encode another
    way. Rejected: optional field, minor version.

## 15. Open questions

1. **Embassy without a seed.** Fail `build()` (proposed), or fall back to a
   zero salt with a warning (IDs unique only per device)?
2. **Depth limit.** Fixed 64, or configurable per instance?
3. **Tokio MQTT.** Move the backend to `rumqttc::v5` to carry context, or
   keep 3.1.1 as a boundary?
4. **Multi-target taps.** One target per `tap_write` (proposed), or a
   variant that writes several records with one writer each?
5. **Serial bandwidth.** Should Serial links default to `?trace=off`?
6. **Consumer events.** Record outbound sends as span events in phase 3, or
   leave all per-consumer events to exporters?
7. **Persistence.** Store the trace context with persisted values, so a
   restore continues the trace instead of starting one?
8. **Cycles through `tap_write`.** Accept them (proposed, bounded by depth),
   or reject them like transform cycles?
9. **`record.set` params on 4.0 servers.** Confirm that unknown params are
   ignored, or gate `tp` on the negotiated version.
