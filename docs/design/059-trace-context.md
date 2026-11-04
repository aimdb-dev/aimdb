# 059 — Trace context in the record envelope

**Status:** 📝 Proposed

**Scope:** Correlation ID for record values (issue TBD). A W3C
`traceparent`-compatible trace context in `WriteMetadata` (058), propagated
along every derivation inside an instance and across connectors that can
carry it; the user-facing write and read API for it; an optional span sink
for recording; AimX 4.1, which carries it on every value; and `aimdb-client`,
`aimdb` CLI and `aimdb-mcp` support. Exporters (Rerun, OTEL) and the `probe`
primitive are separate designs built on this one.

---

## 1. The rule

> Every successful record write carries a trace context. A write either
> **starts** a trace (an origin) or **continues** the trace of the value that
> caused it. The cause is stated by the writer; it is never guessed.

> The trace identity (`trace_id`, flags) is inherited from the cause. The
> span identity (`span_id`) is new for every write. The stamp (058) stays
> local and is never inherited.

A trace context means **"this value belongs to the causal chain that started
at that origin, and this write is one step in it."**

| Part | Comes from | New per write |
|---|---|---|
| `stamp` (058) | local instance clock | yes |
| `span_id` | local ID source | yes |
| `trace_id` | the cause, or new at an origin | no |
| `flags` (sampled, forced) | the cause, or the record's policy at an origin | no |
| `depth` | cause's depth + 1, or 0 at an origin | — |

The two halves of `WriteMetadata` therefore answer different questions: the
stamp says **when** a value was written here, the trace context says **why**.
The 058 rule "timestamps are never inherited or imported" is unchanged; this
design adds a field that is inherited, explicitly.

What a trace context does **not** say:

- That a value was *computed from* every value of its trace. It records one
  parent per write; other inputs of a join are recorded as links, bounded
  (§4.5).
- Anything about time across instances. Per-hop times exist only as each
  instance's own stamps on its own spans.
- That a trace that starts at a record started in the physical world. A trace
  also starts at a connector that cannot carry context (§6.2), and at a
  hand-written write that did not state its cause (§3.2). Tooling makes both
  visible (§7).

## 2. Requirements

| # | Requirement |
|---|---|
| R1 | Every write carries a trace context. Propagation is always on; sampling only decides recording (§5). |
| R2 | Declared derivations (single transforms) propagate with no user code. |
| R3 | Hand-written derivations state their cause with one call; plain `produce()` keeps working and means "origin". |
| R4 | Fixed size, `Copy`, `no_std`, no allocation added. No RNG call per write. |
| R5 | Tokio, Embassy and WASM, in the shared conformance suite. Under Tokio paused time with a fixed seed, IDs are reproducible. |
| R6 | W3C `traceparent` / `tracestate` at every external boundary, without misusing reserved flag bits. |
| R7 | A connector either carries the context or is declared a trace boundary; a boundary is visible in tooling. |

**Non-goals:**

- exporters (Rerun, OTEL) and the `probe` primitive
- per-consumer events (tap reads, outbound sends) as spans
- MCU span forwarding (an MCU propagates and stamps; it records nothing)
- storing trace context in `aimdb-persistence`
- tail-based sampling
- clock synchronisation or cross-instance time arithmetic

## 3. User-facing API

### 3.1 What does not change

Sources, single transforms, links and taps that only read look exactly as
they do today. The values they produce and receive are traced.

```rust
builder.configure::<Temperature>(TempKey::Raw, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
       .source(|ctx, producer| async move {
           loop {
               producer.produce(read_sensor().await);  // origin: new trace
               ctx.time().sleep_secs(1).await;
           }
       })
       .link_to("mqtt://sensors/temp/raw");            // carries the context
});

builder.configure::<Temperature>(TempKey::Calibrated, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
       .transform(TempKey::Raw, |t| t.map(|raw| Some(calibrate(raw))));
       // each output continues its input's trace, automatically
});
```

### 3.2 Writing from your own code: `produce_from`

A tap or task that reads one record and writes another is a derivation the
framework cannot see. It states the cause:

```rust
async fn overheat_watch(
    ctx: RuntimeContext,
    temps: Consumer<Temperature>,
    alarm: Producer<Alarm>,
) {
    let mut reader = temps.subscribe();
    while let Ok(t) = reader.recv().await {
        if t.value.celsius > 80.0 {
            alarm.produce_from(Alarm::Overheat, &t);   // continues t's trace
        }
    }
}
```

`Producer<T>` gains:

```rust
impl<T> Producer<T> {
    /// Origin: starts a new trace. Unchanged.
    pub fn produce(&self, value: T);

    /// Continues the trace of `cause`.
    pub fn produce_from(&self, value: T, cause: &impl AsCause);

    /// Origin of a forced trace: recorded at every hop, regardless of
    /// sampling (§5). For experiments and agents.
    pub fn produce_forced(&self, value: T);

    // try_produce / try_produce_from mirror the above.
}

/// Anything that names the write that caused this one.
pub trait AsCause { fn trace_context(&self) -> TraceContext; }
// Implemented for: &Stamped<T>, WriteMetadata, JoinTrigger, TraceContext.
```

`TraceContext` as a cause covers bridging from outside AimDB, for example an
HTTP request that already has a `traceparent`:

```rust
let tc = TraceContext::from_w3c(req.header("traceparent"), req.header("tracestate"))?;
setpoint.produce_from(new_setpoint, &tc);
```

`AimDb::produce(key, value)` and `aimdb-sync` gain the same `_from` and
`_forced` variants.

### 3.3 Joins

`transform_join` handlers are user code: the handler owns the loop and the
`Producer<O>`. They state the cause with the trigger they are reacting to:

```rust
.transform_join(|b| {
    b.input::<Temperature>(TempKey::Gamma)
     .input::<Humidity>(HumidityKey::Gamma)
     .on_triggers(|mut rx, producer| async move {
         let (mut t, mut h) = (None, None);
         while let Ok(trigger) = rx.recv().await {
             match trigger.index() {
                 0 => t = trigger.as_input::<Temperature>().cloned(),
                 1 => h = trigger.as_input::<Humidity>().cloned(),
                 _ => {}
             }
             if let (Some(t), Some(h)) = (&t, &h) {
                 producer.produce_from(dew_point(t, h), &trigger);  // ← one change
             }
         }
     })
})
```

The other inputs are recorded as links without user code: `JoinEventRx`
already sees every input's metadata, and keeps the latest per input (§4.5).

### 3.4 Reading the context

```rust
let t = reader.recv().await?;
let tc: TraceContext = t.trace();       // like t.stamp()

tc.trace_id();     // TraceId, Display = 32 hex chars
tc.span_id();      // SpanId,  Display = 16 hex chars
tc.depth();        // u8: writes since the origin
tc.is_sampled();
tc.is_forced();
tc.is_valid();     // false only with the `trace-context` feature off (§4.4)

log.info(&format!("{} {}", t.value, tc));  // 00-4bf92f…-00f067aa…-01
```

The parent span is not on the value (§4.2); it is on the span record (§5),
which is what tooling reads to walk a chain.

`aimdb-client` returns the same `Stamped<T>`, decoded from the wire (§6.1),
so `.trace()` works on remote values.

### 3.5 Per-record recording policy

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

The policy applies only where a trace starts. Downstream records follow the
`sampled` flag they inherit. It never affects propagation: every value has a
valid context whatever the policy.

### 3.6 What users have to learn

| Who | New |
|---|---|
| Sources, transforms, links, read-only taps | nothing |
| Code that reads one record and writes another | `produce_from(value, &cause)` |
| Join handlers | `produce_from(value, &trigger)` |
| Debugging | `.trace()`, `aimdb get --trace`, `aimdb trace` |
| Experiments | `produce_forced`, `aimdb set --force-trace` |
| Tuning | `.trace(TracePolicy::…)` |

## 4. Inside the database

### 4.1 Types

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

/// AimDB's own flags. Mapped to W3C only at the boundary (§6).
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
`tracestate` (§6.1).

### 4.2 The envelope

`WriteMetadata` holds the stamp and the trace context; nothing else changes
in `Stamped<T>`. The parent span is deliberately not in the slot: it is only
useful together with recorded spans, and the span sink receives it at write
time (§5). Keeping it out saves 8 bytes per slot in every buffer.

`WriteMetadata::from_wire_stamp(stamp)` stays for tests and gains a sibling:

```rust
impl WriteMetadata {
    /// Metadata decoded from a peer's wire value. Never reaches a buffer;
    /// a write takes the context through `Cause::Remote` (§4.3).
    pub fn from_wire(stamp: Stamp, trace: Option<TraceContext>) -> Self;
    pub fn trace(&self) -> TraceContext;
}
```

### 4.3 Writing

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
write: one clock read (058), one ID from the instance's ID source (§4.6),
and, if the result is sampled or forced and a sink is installed, one
`SpanSink::record` call (§5).

| Writer | Cause |
|---|---|
| `Producer::produce` | `Origin { forced: false }` |
| `Producer::produce_forced` | `Origin { forced: true }` |
| `Producer::produce_from(v, c)` | `Derived(c.trace_context())` |
| single transform (`map`, stateful) | `Derived(input)`, by the framework |
| join handler | whatever the handler's producer call states |
| inbound connector, context present | `Remote(ctx)` |
| inbound connector, no carrier or no context | `Boundary` |
| AimX `record.set` with `tp` | `Remote(ctx)` |
| AimX `record.set` without `tp` | `Origin` |
| `aimdb-persistence` restore | `Restore` (an origin) |

From the cause:

- **Origin / Boundary / Restore:** `trace_id` = instance salt ‖ this
  write's `span_id` (§4.6); `depth = 0`; *sampled* from the record's
  `TracePolicy`; *forced* from the cause.
- **Derived / Remote:** `trace_id` and flags from the cause;
  `depth = cause.depth + 1`.

### 4.4 Feature flag

`trace-context`, default on in every adapter. Off makes `TraceContext` a
zero-sized type: `WriteMetadata` is back to 8 bytes, `.trace()` returns an
invalid context, the AimX encoder omits `tp`, and inbound contexts are
dropped (every inbound write is a `Boundary`). The API is the same either
way, so no code is written twice (058 R6).

### 4.5 Depth and links

**Depth.** `depth` saturates at `TRACE_DEPTH_LIMIT` (64). A write whose cause
is at the limit starts a new trace instead, with kind `DepthReset` and a link
to the cause. This bounds software feedback loops (A → B → A …) without
cutting real pipelines, which are far shorter.

**Links.** A join keeps the latest `WriteMetadata` per input in
`JoinEventRx` (it already receives it with every trigger). When a write's
cause is a `JoinTrigger`, the span record (§5) carries the other inputs'
latest span references as links: up to `MAX_LINKS` (4) in a fixed array,
plus a dropped count. Links exist only on span records, never in the slot,
so they cost nothing for unsampled traces.

### 4.6 IDs

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

### 4.7 Memory

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
cannot afford it turn off `trace-context` (§4.4).

## 5. Recording

Propagation is always on; recording is opt-in. An instance may install one
span sink:

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
  the AimX `trace.get` method (§6.1) and `aimdb trace`.
- Exporters (Rerun, OTEL) are further `SpanSink` implementations, in their
  own designs.
- Embassy installs none. An MCU's values arrive at the first Tokio hop with
  their context, so the hop's spans name the MCU span as parent; the MCU span
  itself is not recorded (non-goal).

## 6. Wire

### 6.1 AimX 4.1

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

**Compatibility.** The fields are additive and the AimX decoders do not deny
unknown fields, so this is a minor version: `PROTOCOL_VERSION` becomes
`"4.1"`. A 4.0 peer ignores `tp`; a value from a 4.0 peer has none, and the
receiving write is a `Boundary`. (`record.set` params on 4.0 servers: open
question 7.)

**Cost.** About 62 bytes per value for `tp`, plus about 20 for `tst`. On a
115 200 baud serial link that is roughly 7 ms per value. A link can opt out
with `?trace=off`; it is then a trace boundary (§6.2).

### 6.2 Connectors

A connector either carries `traceparent` / `tracestate` or is a declared
boundary. The inbound dispatch path gains an `Option<TraceContext>` argument
(`None` → `Cause::Boundary`); the outbound path already receives the value's
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

## 7. Client and tools

- **`aimdb-client`:** values decode `tp`/`tst` into `WriteMetadata::from_wire`.
- **CLI:**
  - `aimdb get <record> --trace` and `aimdb watch <record> --trace` print
    the context with each value.
  - `aimdb set <record> <value> --trace <traceparent>` continues a trace;
    `--force-trace` starts a forced one and prints its trace ID.
  - `aimdb trace <trace_id>` lists the server's span records for that trace,
    in causal order.
  - `aimdb graph` marks boundary links (`⟂ trace restarts here`).
- **MCP:** `get_record` includes the context; `set_record` accepts
  `force_trace` and returns the trace ID; new `get_trace` tool. Together that
  is the loop `probe` will automate: force a write, then read its trace.

```text
$ aimdb trace 4bf92f3577b34da6a3ce929d0e0e4736
instance  record                  kind      depth  parent
mcu-07    sensor.temp.raw         origin    0      —            (not recorded)
edge-01   sensor.temp.raw         remote    1      00f067aa…    mqtt://sensors/temp/raw
edge-01   hub.dew_point           derived   2      a3ce929d…    join; link: humidity
```

**Unattributed origins.** With `observability`, each record counts origin
writes made through a producer handle that is not its `.source()`. `aimdb
graph` and MCP flag records where that count is non-zero: usually a missing
`produce_from`, sometimes a legitimate origin (a timer, a command). This is a
hint, not an error.

## 8. What breaks

| Area | Change |
|---|---|
| Rust API | additive: `produce_from`, `produce_forced`, `try_*` variants, `Stamped::trace`, `TracePolicy`, `SpanSink` |
| `WriteMetadata` | grows to 40 bytes; gains `trace()` and `from_wire` |
| Connector implementations | inbound dispatch takes `Option<TraceContext>` |
| Embassy applications | `build()` requires `.id_seed(…)` while `trace-context` is on |
| AimX | 4.1, additive; 4.0 peers interoperate |
| Memory | +32 bytes per buffer slot (§4.7) |

## 9. Phases

1. **Core.** Types, `WriteMetadata`, ID source and seeds, `Cause` through
   `RecordWriter`, `produce_from` / `produce_forced`, automatic propagation in
   single transforms, `JoinTrigger: AsCause` and per-input metadata in
   `JoinEventRx`, depth limit, `Stamped::trace`, `trace-context` feature.
   Conformance suite on Tokio, Embassy and WASM; size assertions; allocation
   gates.
2. **AimX 4.1.** `tp`/`tst` on values and `record.set`, client decoding,
   CLI `--trace` / `--force-trace`, MCP `get_record` / `set_record`.
3. **Recording.** `SpanSink`, `MemorySpanSink`, links, `trace.get`, `aimdb
   trace`, MCP `get_trace`, graph boundaries and unattributed-origin hint.
4. **Carriers.** MQTT v5 user properties (Embassy), Zenoh attachment; decision
   on the Tokio MQTT backend.

Phase 1 is useful on its own: every in-process value can be correlated.

## 10. Tests

- **Conformance (all adapters):** source → transform → join → tap keeps one
  `trace_id`; span IDs differ per write; depth increments by one per write.
- `produce_from` across records continues the trace; `produce` starts a new
  one; `produce_forced` sets *forced*, and every descendant has it.
- Depth limit: a loop of two records resets at 64 with a link to the cause.
- Joins: the output's parent is the trigger's span; links name the other
  inputs' latest spans; more than `MAX_LINKS` inputs count as dropped.
- IDs: fixed seed under paused time gives identical IDs across runs (snapshot);
  `mix64` round-trip; no all-zero ID is ever generated.
- AimX: `tp`/`tst` encode/decode round-trip; a 4.0 decoder fixture ignores
  them; a value without `tp` produces a `Boundary` write.
- Inbound migration: a value decoded, migrated (`aimdb-data-contracts`) and
  written keeps its `trace_id`.
- W3C: parse/format against the spec examples; *forced* never sets W3C bit 1.
- `size_of::<WriteMetadata>() == 40`; slot sizes as §4.7; `aimdb-bench`
  allocation gates unchanged; bytes per AimX message documented.

## 11. Alternatives considered

1. **Implicit ambient context** (task-local "current span"). Not available on
   Embassy, ambiguous across `.await` and fan-out. Rejected: the cause is an
   argument.
2. **Mandatory cause on every write** (no plain `produce`). Prevents silent
   chain breaks at compile time, but changes every source in every
   application. Rejected for `produce` = origin plus the unattributed-origin
   hint (§7).
3. **Join producer that defaults to the last trigger.** Saves one line in
   join handlers, but makes `produce()` mean different things in different
   places. Rejected: `produce_from(v, &trigger)`.
4. **A span per pipeline stage** (buffer, tap, outbound). A slot is shared by
   all readers and cannot hold per-reader state. Rejected: a span is a write;
   per-consumer events are a non-goal.
5. **Parent span in the slot.** +8 bytes per slot for information that is
   only useful with recorded spans. Rejected: the sink gets it at write time.
6. **W3C flag bit 1 for *forced*.** Collides with Level 2 *random-trace-id*.
   Rejected: AimDB flags internally, `tracestate` on the wire.
7. **Per-hop timestamps in the envelope.** Variable-length and mixes clocks.
   Rejected: each hop's stamp lives on its own span.
8. **Trace context as a contract field.** Contracts describe values, not
   their provenance, and migrations would have to carry it. Rejected: the
   envelope.
9. **RNG per write.** Costly on MCUs and unnecessary for uniqueness. Rejected:
   seed once, counter plus bijective mix.
10. **Payload wrapper for MQTT 3.1.1.** Breaks every non-AimDB subscriber.
    Rejected: boundary.
11. **Sampling gates propagation.** Would make "why did this change?"
    unanswerable for most values. Rejected: always propagate, sample
    recording.

## 12. Open questions

1. **Embassy without a seed.** Fail `build()` (proposed), or fall back to a
   zero salt with a warning (IDs unique only per device)?
2. **Depth limit.** Fixed 64, or configurable per instance?
3. **Tokio MQTT.** Move the backend to `rumqttc::v5` to carry context, or
   keep 3.1.1 as a boundary?
4. **Serial bandwidth.** Should Serial links default to `?trace=off`?
5. **Consumer events.** Record outbound sends as span events in phase 3, or
   leave all per-consumer events to the exporters?
6. **Persistence.** Store the trace context with persisted values, so a
   restore continues the trace instead of starting one?
7. **`record.set` params on 4.0 servers.** Confirm that unknown params are
   ignored, or gate `tp` on the negotiated version.
