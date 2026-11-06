# 060 — Trace context

**Status:** 📝 Proposed

**Scope:** A trace ID and a depth on every record value written by an
instance, carried in `WriteMetadata` (058) and propagated along derivations
and across connectors that can carry them. Propagation goes through the
existing transform, join and producer APIs. Together with the dependency
graph, trace ID and depth let Rerun taps correlate writes and name parent
candidates (§6). Breaking for Rust APIs (join handlers, `WriteMetadata`,
session engine, runtime adapters); additive on the wire (the graph shapes do
not change). Ships as a 3.x minor release despite the breaks (§8.1).

A throwaway prototype of phases 1 and 2 passed every test in §10 that it
covered; its measurements are quoted in §4.3 and §5.

---

## 1. The rule

> Every record write carries a trace context. A write either **starts** a
> trace or **continues** the trace of the value that caused it. A trace
> context is created only when a value is written locally; a value read from
> a peer carries exactly what the peer sent.

| Field | Origin write | Derived write |
|---|---|---|
| `trace_id` (16 bytes) | new | the cause's |
| `depth` (`u8`) | 0 | the cause's + 1, saturating at 255 |

The stamp (058) says **when** a value was written here. The trace context
says **why**. The stamp stays local; the trace context is inherited.

What trace ID and depth guarantee is **correlation**: writes with the same
trace ID belong to one causal chain, and depth orders them along it. They do
not identify a write's parent. The dependency graph narrows the parent to a
set of candidates, usually one (§6).

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

### 2.2 Joins

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

`DerivedWriter` has no plain `produce`, so a join cannot forget its cause.

### 2.3 Producers

`Producer<T>` keeps `produce`, which starts a trace, and gains
`produce_from(value, &WriteMetadata)`. A tap that writes another record
through `db.producer()` can continue the trace of the value it read:

```rust
async fn overheat_watch(ctx: RuntimeContext, temps: Consumer<Temperature>, alarm: Producer<Alarm>) {
    let mut reader = temps.subscribe();
    while let Ok(t) = reader.recv().await {
        if t.value.celsius > 80.0 {
            alarm.produce_from(Alarm::Overheat, t.metadata());
        }
    }
}
```

Its writes are correlated with their cause, but the graph has no edge for
them (the record stays `Passive`), so Rerun cannot name their parent (§6). A
typed single-input async transform that would make this a graph edge is
deferred (open question 4).

### 2.4 Reading

```rust
impl WriteMetadata {
    pub fn trace(&self) -> Option<TraceContext>;
}

let t = reader.recv().await?;
let tc = t.metadata().trace().expect("local writes always carry a trace");
tc.trace_id();   // TraceId, Display = 32 hex chars
tc.depth();      // u8
```

- **In process:** every value read from a local record has `Some`.
- **`aimdb-client`:** `Some` when the peer sent a `trace`, `None` when it did
  not (an older peer, or a link without trace support). The client never
  invents an ID, so two reads of the same stored value report the same thing.

`produce_from` takes `&WriteMetadata`. If its trace is `None` (metadata
decoded from a peer and passed back in), the write starts a trace.

## 3. Dependency graph

No change. `RecordOrigin`, `EdgeType` and the AimX `graph.*` replies keep
their 4.x shapes (`{"transform":{"input":…}}`,
`{"transform_join":{"inputs":[…]}}`). Rust builder changes, if any later,
are kept separate from the wire shapes.

## 4. Inside the database

### 4.1 Envelope

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId([u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceContext { trace_id: TraceId, depth: u8 }

pub struct WriteMetadata { stamp: Stamp, trace: Option<TraceContext> }
```

`RecordWriter::push(value, cause: Option<TraceContext>)` stays the one place
that creates `WriteMetadata` for a buffer, and always stores `Some`: `None`
as the cause starts a trace, `Some(c)` continues `c`. Only
`WriteMetadata::from_wire(stamp, Option<TraceContext>)` (replacing
`from_wire_stamp`) can hold `None`, and it never reaches a buffer.

| Writer | Cause |
|---|---|
| `Producer::produce`, `DerivedWriter::produce_origin` | `None` |
| `map`, `with_state` | the input's trace |
| `Producer::produce_from`, `DerivedWriter::produce_from` | the given metadata's trace |
| inbound connector with a trace on the wire | the wire trace |
| AimX `write` from a peer's outbound mirror, with a trace | the wire trace (§5.1) |
| inbound connector or AimX `write` without one, AimX `record.set`, persistence restore | `None` |

An inbound value without a trace therefore gets a new ID when, and only
when, it is inserted into a local record.

### 4.2 IDs

The trace ID is W3C-sized (16 bytes), so a later OTEL exporter can use it
unchanged. Only origin writes need a new one:

```text
trace_id = salt (8 bytes) ‖ counter (8 bytes)
```

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
section elsewhere (Cortex-M has no native 64-bit atomics). Only origin writes
touch it; there is no RNG call per write.

### 4.3 Memory

`WriteMetadata` grows from 8 to 32 bytes, so every buffer slot grows by 24
bytes. A 16-slot `f32` ring on Embassy goes from 256 to 640 bytes. The 058
`size_of` assertions are updated; the allocation gates must show no added
allocation.

Measured in the prototype: no allocation added on the produce, consume,
connector or AimX paths; `record.get` allocates 93 more bytes per call for
its longer reply. On the host, an origin write takes about 14 ns longer
(the ID counter) and a continued write about 3 ns.

## 5. Wire

One carrier value everywhere: `<trace_id as 32 hex>/<depth>`, e.g.
`4bf92f3577b34da6a3ce929d0e0e4736/3`.

| Connector | Carries it as |
|---|---|
| AimX (TCP, UDS, Serial, WebSocket) | optional `trace` field next to `ts` on every value |
| MQTT v5 (Embassy backend) | user property `aimdb-trace` |
| Zenoh | attachment key `aimdb-trace` |
| MQTT 3.1.1 (Tokio backend), KNX | not carried |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "trace":"4bf92f3577b34da6a3ce929d0e0e4736/3","data":{"celsius":21.5}}
```

- The field is optional: an older peer or a link without trace support
  sends none. AimX decoders ignore unknown fields, so this is the next minor
  version after 059's 4.1. Nothing else on the wire changes.
- Absence means different things on the two paths, with the same frame:
  - **observing** (client `get`, `watch`, `drain`): the value has no trace,
    `trace()` is `None`;
  - **ingesting** (inbound connector writing locally): the local write starts
    a trace.
- A malformed field (bad hex, depth above 255, all-zero ID) is treated as
  absent: the value is kept and its trace is `None`. The trace is advisory
  and must never cost a value.
- A receiver that ingests a traced value continues it: same ID, depth + 1.
- 45 bytes per value on AimX at depth 0–9, 46 at 10–99, 47 above (measured:
  an `event` with a small value grows from 73 to 118 bytes). No connector
  wraps the payload.

### 5.1 AimX outbound mirror writes

The `trace` field on values covers the subscription direction (`event`,
`snap`, replies). The opposite direction, a local record mirrored to a
peer, uses the fire-and-forget `write` frame, which today carries only
`topic` and `payload`: the client's mirror pump drops `msg.metadata`, and the
receiving dispatch passes only the value to `set_record_from_json`.

- The `write` frame gains the same optional `trace` field:
  `{"t":"write","topic":…,"trace":"<32 hex>/<depth>","payload":…}`. It gains
  no `ts`: the receiver stamps the write locally (058).
- The mirror pump sends `msg.metadata.trace()`.
- `Inbound::Write` and `Session::write` carry an `Option<TraceContext>`
  through to the record write, which continues it (depth + 1) or, without
  one, starts a trace.

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

This is independent of open question 3, which is about `record.set` issued
by a user or tool, not about mirror writes.

## 6. Rerun taps

A Rerun tap is a read-only `tap` on a Tokio instance. For each value it logs
the value, the trace ID, the depth, and the stamp on that instance's own
timeline. Each instance also logs its dependency graph once. MCUs run no Rerun
taps; their values show up at the hub with their trace intact.

**What Rerun promises:** correlation (all writes of a trace) and order
(depth). Parents are reported as **candidates**, never guessed.

**Parent candidates** of a write *W* with trace *T*, depth *d*, on record
*R*: the writes in *T* at depth *d − 1* on *R*'s graph inputs.

| *R*'s origin | Graph inputs |
|---|---|
| `Transform { input }` | `input` |
| `TransformJoin { inputs }` | `inputs` |
| `Link` | the record behind the same address on another instance |
| `Source`, `Passive` | none |

| Candidates | Rerun shows |
|---|---|
| exactly one observed | that write as the **only observed candidate**; it is the parent only if the recording holds every write of *T* at *d − 1* |
| several | all of them, marked ambiguous |
| none | no parent: an origin, a `Passive` record written with `produce_from`, or a hop without a tap |
| *W* at depth 255 | parentage **indeterminate**: depth saturates, so the parent may also be at 255 |

Rerun only sees the writes its taps captured, so "one observed candidate"
is never presented as proof. In the diamond below, if `A` triggered a write
of `C` but the tap missed that write of `A`, the only observed candidate is
`B`, which is not the parent.

Stamps are never used to choose between candidates. Ambiguity arises even
when every write is captured:

- **Diamond:** `Raw` fans out to `A` and `B`, and both feed the join `C`. `A`
  and `B` both carry `Raw`'s trace at depth 1, so each write of `C` at depth
  2 has two candidates. Which trigger caused which `C` depends on the join's
  queue, not on stamps.
- **Repeated writes:** an input written twice in one trace at the same
  depth.

Chains, fan-out to single-input transforms, and joins whose inputs come from
different traces (the common case: independent sensors) have exactly one
candidate when every write is captured. Exact parentage in every case would need the child to carry the
identity of the write that triggered it (a parent span ID, 8 more bytes per
slot); that is deferred until a consumer needs it (open question 5).

The Rerun integration itself (crate, mapping contracts to Rerun types,
layout) is a separate design.

## 7. Tools

- `aimdb get <record> --trace` and `aimdb watch <record> --trace` show the
  trace ID and depth, or `no trace` for a value the peer sent without one.
- MCP `get_record` includes the trace, or `null`.

## 8. What breaks

| Area | Change |
|---|---|
| Join handlers | `on_triggers` receives `DerivedWriter<O>` instead of `Producer<O>` |
| `WriteMetadata` | 32 bytes; `trace() -> Option<TraceContext>`; `from_wire_stamp` becomes `from_wire(stamp, Option<TraceContext>)` |
| Session engine | new fields and parameters (§5.2); every `Session` and `EnvelopeCodec` implementation updates |
| Runtime adapters | `RuntimeOps::id_seed()`; Embassy takes its seed in the adapter configuration |
| `build()` | fails when neither the adapter nor `.id_seed()` supplies a seed |
| `Producer<T>`, `InboundDispatch`, `TopicMatch` | additive: `produce_from`, `dispatch_traced`, `trace()` |
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

1. **Core:** envelope, IDs, automatic propagation in `map` / `with_state`,
   `DerivedWriter` in joins, `Producer::produce_from`, tests on Tokio,
   Embassy and WASM. Enough for Rerun taps on one instance.
2. **AimX:** `trace` field on values and on mirror `write` frames (§5.1),
   client, CLI, MCP. Enough for chains across AimX-linked instances in both
   directions.
3. **MQTT v5 and Zenoh** carriers.

## 10. Tests

- Source → `map` → `with_state` → join keeps one trace ID; depth grows by
  one per write.
- `produce_origin` and `Producer::produce` start new traces;
  `Producer::produce_from` continues one.
- Every locally written value has `trace()` = `Some`.
- Fixed seed gives identical IDs across runs; no all-zero ID.
- Seeds: each adapter supplies one; `.id_seed()` overrides it; `build()`
  fails without either; the `test_support` runtimes build without
  `.id_seed()` (core's no_std test suites, `aimdb-data-contracts`).
- A malformed `trace` field on a value or `write` frame keeps the value and
  decodes as `None`.
- **Legacy frame, both paths:** the same AimX value frame without `trace`
  - decoded twice by `aimdb-client` gives `None` both times;
  - ingested by an inbound connector gives a local write with a new trace at
    depth 0.
- AimX round-trip of a traced value; ingesting it continues the trace at
  depth + 1.
- **Mirror push:** instance A mirrors a record to instance B over AimX; B
  stores A's trace ID at A's depth + 1. A `write` frame without `trace`
  starts a trace on B. The same over the WebSocket connector.
- `graph.nodes` and `graph.edges` replies are byte-identical to 4.x for a
  fixture with a transform and a join.
- A value that is migrated on the inbound path keeps its trace.
- **Candidates:** a chain and a fan-out give one candidate per write; the
  diamond `Raw → A, Raw → B, A + B → C` gives two candidates for each write
  of `C` and is reported as ambiguous; the same diamond with the write of
  `A` dropped from the recording reports `B` as the only observed candidate,
  not as the parent; a write at depth 255 is reported as indeterminate; a
  `Passive` record written with `produce_from` gives none.
- `size_of::<WriteMetadata>() == 32`; allocation gates unchanged.

## 11. Alternatives considered

1. **Merge `transform` and `transform_join` into one builder** (with
   `tap_write` or a single-input async body). Changes the graph shapes on the
   wire and every transform call, and makes a single-input caller downcast
   its own declared type through the boxed join path. Not needed for
   propagation: deferred.
2. **Pick a parent by stamp order.** Wrong in the diamond case even with
   every write captured. Rejected: report candidates.
3. **Mint a trace ID in the client for untraced values.** Two reads of the
   same stored value would report different IDs. Rejected: `None` on read,
   new ID only on local insertion.
4. **Implicit "current trace"** (task-local). Not available on Embassy,
   unclear across `.await`. The cause is an argument instead.
5. **Full W3C span model** (span IDs, sampling, a span sink). No consumer
   needs it yet. The trace ID already has the W3C size.
6. **Plain `produce()` in joins.** A forgotten cause would silently start a
   new trace. `DerivedWriter` makes it a compile error.
7. **Carry the trace inside the payload** where a protocol has no headers.
   Breaks every non-AimDB consumer.
8. **Defaulted `*_traced` session methods** to keep the session engine
   additive. A session that does not override them drops traces silently,
   and the surface doubles. Rejected: new fields and parameters (§5.2).
9. **Seed from a `std` feature switch in core.** In the prototype it failed
   37 existing tests: crates such as `aimdb-data-contracts` and the WASM
   adapter build core without `std` on hosted targets. Rejected: the
   adapter supplies the seed (§4.2).

## 12. Open questions

1. **Tokio MQTT backend.** Move to `rumqttc::v5` so it can carry traces?
2. **Serial links.** Is 45 bytes per value (about 3.9 ms at 115,200 baud)
   acceptable, or should Serial leave the trace off by default?
3. **`record.set` with a trace.** Should a remote client be able to continue
   a trace, or does every remote set start one?
4. **Typed single-input async transform.** Would make taps that write other
   records graph edges, with a typed receiver instead of the boxed join path.
   Worth its own API, and when?
5. **Exact parentage.** Does any consumer need the parent in the diamond
   case badly enough to pay 8 bytes per slot for a parent span ID?
6. **Opt-out for small MCUs.** Is 24 bytes per slot acceptable everywhere, or
   is a feature flag needed? (A 16-slot `f32` ring: 256 → 640 bytes.)
