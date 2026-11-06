# 060 — Trace context

**Status:** 📝 Proposed

**Scope:** A trace ID and a depth on every record value, carried in
`WriteMetadata` (058) and propagated along every derivation and across
connectors that can carry them; `tap_write`, which makes taps that write other
records edges of the dependency graph. Together with the graph, this is enough
to reconstruct causal chains, which is what Rerun taps need (§6). Breaking for
Rust APIs; additive on the wire.

---

## 1. The rule

> Every record write carries a trace context. A write either **starts** a
> trace or **continues** the trace of the value that caused it. The writer
> states the cause, and every derivation is an edge of the dependency graph.

| Field | Origin write | Derived write |
|---|---|---|
| `trace_id` (16 bytes) | new | the cause's |
| `depth` (`u8`) | 0 | the cause's + 1, saturating at 255 |

The stamp (058) says **when** a value was written here. The trace context
says **why**. The stamp stays local; the trace context is inherited.

A write's parent is not stored. It is the write in the same trace, at
`depth − 1`, on one of the record's inputs in the dependency graph (§6).

## 2. API

### 2.1 Unchanged

Sources, transforms, links and `tap` look as they do today.

```rust
reg.source(|ctx, producer| async move {
    loop {
        producer.produce(read_sensor().await);   // starts a trace
        ctx.time().sleep_secs(1).await;
    }
});

reg.transform(TempKey::Raw, |t| t.map(|raw| Some(calibrate(raw))));
// each output continues its input's trace, done by the framework
```

### 2.2 `tap_write`

`tap` stays read-only. A tap that writes another record names it:

```rust
reg.tap_write::<Alarm>(AlarmKey::Overheat, |ctx, temps, alarm| async move {
    let mut reader = temps.subscribe();
    while let Ok(t) = reader.recv().await {
        if t.value.celsius > 80.0 {
            alarm.produce_from(Alarm::Overheat, t.trace());
        }
    }
});
```

- `build()` checks that the target exists and holds the right type.
- The graph gets an edge from the tapped record to the target.
- One target per `tap_write`.

### 2.3 `DerivedWriter`

`tap_write` and `transform_join` handlers receive a `DerivedWriter<O>`. It has
no plain `produce`, so a derived write cannot forget its cause:

```rust
impl<O> DerivedWriter<O> {
    pub fn produce_from(&self, value: O, cause: TraceContext);
    pub fn produce_origin(&self, value: O);   // timer, timeout: an explicit choice
}
```

A join names the trigger it reacts to:

```rust
.on_triggers(|mut rx, writer| async move {
    while let Ok(trigger) = rx.recv().await {
        // … update state …
        writer.produce_from(dew_point(&t, &h), trigger.trace());
    }
})
```

`Producer<T>` (sources, `db.producer()`) keeps only `produce`, which starts a
trace.

### 2.4 Reading

```rust
let t = reader.recv().await?;
t.trace().trace_id();   // TraceId, Display = 32 hex chars
t.trace().depth();      // u8
```

`.trace()` exists on `Stamped<T>`, `WriteMetadata` and `JoinTrigger`.
`aimdb-client` values carry it too.

## 3. Dependency graph

Today a tap is an edge without a target, and a record written from a tap
through `db.producer()` is `Passive`. `tap_write` adds:

```rust
RecordOrigin::TapWrite { inputs: Vec<String> }   // all records that tap_write into it
EdgeType::TapWrite { index: usize }              // from: tapped record, to: target
```

- Several `tap_write`s may target one record.
- A `tap_write` target has no other origin (source, transform, join, inbound
  link); `build()` rejects the mix.
- `aimdb graph` and MCP show the new edges.

## 4. Inside the database

### 4.1 Envelope

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId([u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceContext { trace_id: TraceId, depth: u8 }

pub struct WriteMetadata { stamp: Stamp, trace: TraceContext }
```

`RecordWriter::push(value, cause: Option<TraceContext>)` stays the one place
that creates `WriteMetadata`. `None` starts a trace; `Some(c)` continues `c`.

| Writer | Cause |
|---|---|
| `Producer::produce`, `DerivedWriter::produce_origin` | `None` |
| transform | the input's trace |
| `DerivedWriter::produce_from` | the given trace |
| inbound connector with a trace on the wire | the wire trace |
| inbound connector without one, AimX `record.set`, persistence restore | `None` |

### 4.2 IDs

The trace ID is W3C-sized (16 bytes), so a later OTEL exporter can use it
unchanged. Only origin writes need a new one:

```text
trace_id = salt (8 bytes) ‖ counter (8 bytes)
```

The salt is taken once at `build()`: `getrandom` on Tokio,
`crypto.getRandomValues` on WASM, and `.id_seed(u64)` on Embassy (a hardware
RNG, or the chip ID mixed with a boot counter). Without a seed, an Embassy
`build()` fails. The counter is a `portable_atomic::AtomicU64`. No RNG call
per write.

### 4.3 Memory

`WriteMetadata` grows from 8 to 32 bytes, so every buffer slot grows by 24
bytes. A 16-slot `f32` ring on Embassy goes from 256 to 640 bytes. The 058
`size_of` assertions are updated; the allocation gates must show no added
allocation.

## 5. Wire

One carrier value everywhere: `<trace_id as 32 hex>/<depth>`, e.g.
`4bf92f3577b34da6a3ce929d0e0e4736/3`.

| Connector | Carries it as |
|---|---|
| AimX (TCP, UDS, Serial, WebSocket) | optional `trace` field next to `ts` on every value |
| MQTT v5 (Embassy backend) | user property `aimdb-trace` |
| Zenoh | attachment key `aimdb-trace` |
| MQTT 3.1.1 (Tokio backend), KNX | not carried: the receiving write starts a trace |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "trace":"4bf92f3577b34da6a3ce929d0e0e4736/3","data":{"celsius":21.5}}
```

- The AimX field is optional because absence has a meaning: no context, the
  receiver starts a trace. AimX decoders ignore unknown fields, so this is
  the next minor version after 059's 4.1.
- About 45 bytes per value on AimX.
- A receiver continues the wire trace: same ID, depth + 1.
- No connector wraps the payload.

## 6. Rerun taps

A Rerun tap is a read-only `tap` on a Tokio instance. For each value it logs
the value, the trace ID, the depth, and the stamp on that instance's own
timeline. Each instance also logs its dependency graph once. MCUs run no Rerun
taps; their values show up at the hub with their trace intact.

**Parent of a write** with trace *T*, depth *d*, on record *R*: the write in
*T* at depth *d − 1* on one of *R*'s inputs:

| *R*'s origin | Inputs |
|---|---|
| `Transform` | its input |
| `TransformJoin` | its inputs (only the trigger is in *T*) |
| `TapWrite` | its inputs |
| `Link` | the record behind the same address on another instance |
| `Source`, `Passive` | none: an origin |

If several writes match, take the latest one at or before *W*'s stamp on the
same instance.

- **Exact:** chains, fan-out, joins, `tap_write`, links between instances.
- **Approximate:** the same input written twice in one trace at one depth
  (diamonds); stamp order is wrong only while a transform lags.
- **Gaps:** a record without a Rerun tap, or a tap that skips values. The
  graph still names the missing hop.

The Rerun integration itself (crate, mapping contracts to Rerun types,
layout) is a separate design.

## 7. Tools

- `aimdb get <record> --trace` and `aimdb watch <record> --trace` show the
  trace ID and depth.
- MCP `get_record` includes them.
- `aimdb graph` shows `tap_write` edges and links that don't carry traces.

## 8. What breaks

| Area | Change |
|---|---|
| Join handlers | receive `DerivedWriter<O>` instead of `Producer<O>` |
| Taps that write records | move to `tap_write`; the old way still compiles but leaves the graph without the edge |
| Graph types | `RecordOrigin::TapWrite`, `EdgeType::TapWrite` |
| `WriteMetadata` | 32 bytes; `from_wire_stamp` becomes `from_wire(stamp, Option<TraceContext>)` |
| Connectors | inbound dispatch takes `Option<TraceContext>` |
| Embassy | `build()` requires `.id_seed(…)` |
| AimX | optional `trace` field, minor version |

## 9. Phases

1. **Core:** envelope, IDs, `DerivedWriter`, `tap_write` and graph edges,
   transforms and joins, tests on Tokio, Embassy and WASM. Enough for Rerun
   taps on one instance.
2. **AimX:** `trace` field, client, CLI, MCP. Enough for chains across
   AimX-linked instances.
3. **MQTT v5 and Zenoh** carriers.

## 10. Tests

- Source → transform → join → `tap_write` keeps one trace ID; depth grows by
  one per write.
- `produce_origin` and `Producer::produce` start new traces.
- `tap_write`: graph edge present; `build()` rejects a missing target, a
  wrong type, and a target with another origin.
- Fixed seed gives identical IDs across runs; no all-zero ID.
- AimX round-trip; a value without `trace` starts a trace at the receiver.
- A value that is migrated on the inbound path keeps its trace.
- Reconstruction: for a fixture with a chain, a fan-out, a join, a
  `tap_write` and a link between two instances, the §6 rule finds the
  expected parents.
- `size_of::<WriteMetadata>() == 32`; allocation gates unchanged.

## 11. Alternatives considered

1. **Implicit "current trace"** (task-local). Not available on Embassy,
   unclear across `.await`. The cause is an argument instead.
2. **Store the parent span on each value.** Costs memory per slot; the
   graph identifies the parent.
3. **Full W3C span model** (span IDs, sampling, a span sink). No consumer
   needs it yet. An OTEL exporter can add a span ID to `WriteMetadata` later;
   the trace ID already has the W3C size.
4. **Plain `produce()` in derived code.** A forgotten cause would silently
   start a new trace. `DerivedWriter` makes it a compile error.
5. **Carry the trace inside the payload** where a protocol has no headers.
   Breaks every non-AimDB consumer.

## 12. Open questions

1. **Cycles through `tap_write`** (a controller writing a setpoint it also
   reads). Accept them, or reject them like transform cycles?
2. **Tokio MQTT backend.** Move to `rumqttc::v5` so it can carry traces?
3. **Serial links.** Is about 45 bytes per value acceptable, or should Serial
   leave the trace off by default?
4. **`record.set` with a trace.** Should a remote client be able to continue
   a trace, or does every remote set start one?
5. **Opt-out for small MCUs.** Is 24 bytes per slot acceptable everywhere, or
   is a feature flag needed?
