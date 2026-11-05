# 059 — Probes

**Status:** 📝 Proposed

**Scope:** Named, developer-defined async inspection functions that are
registered on an AimDB instance and invoked on demand through the CLI or MCP.
This covers the `Probe` trait, registration with a description and schemas,
invocation over AimX, concurrency, deadlines, error transport, result
serialization and limits. It also covers the first ready-made probes: MQTT
and KNX discovery.

---

## 1. What a probe is

> A probe is a named, developer-defined async inspection function,
> registered on an AimDB instance and invoked on demand through the CLI or
> MCP.

Its main purpose is to **discover and understand the signals available in
the instance's environment**, including signals that have no configured
record or link yet: which topics a broker carries, which group addresses
occur on a KNX bus, what a device reports about itself.

- **Bounded:** a probe has one invocation with a deadline and gives a
  temporary answer. A source runs continuously; a probe does not.
- **Direct:** the result goes back to the caller. Nothing is written into a
  record.
- **Developer-defined:** a probe is application code registered when the
  instance is built. Callers choose a registered probe and pass it
  parameters. They cannot submit code.
- **Typed:** each probe has its own parameter and result types, published as
  JSON schemas. There is no shared `name + facts` shape.

```
$ aimdb probe list
mqtt.discover   Topics seen on the broker during a window         (max 120 s)
knx.discover    Group addresses seen on the bus during a window   (max 300 s)

$ aimdb probe run mqtt.discover --param filter='plant/#' --param duration_s=15
{
  "coverage": {
    "scope": "mqtt://broker:1883, filter plant/#",
    "connection": "dedicated",
    "window": { "started_ns": 81234000000, "duration_ms": 15000 },
    "complete": true,
    "limits": [],
    "note": "Only topics published, or retained, during the window are listed."
  },
  "topics": [
    { "topic": "plant/boiler/temp", "messages": 15, "retained": true,
      "bytes": { "min": 4, "max": 5, "last": 5 }, "linked": false },
    { "topic": "plant/boiler/state", "messages": 1, "retained": true,
      "bytes": { "min": 2, "max": 2, "last": 2 }, "linked": true }
  ]
}
```

## 2. The trait

```rust
pub trait Probe: Send + Sync + 'static {
    /// What the caller supplies. Deserialized by core from the request.
    type Params: DeserializeOwned + Send;
    /// What the probe returns. Serialized by core into the reply.
    type Output: Serialize + Send;

    fn run<'a>(
        &'a self,
        params: Self::Params,
        ctx: &'a ProbeCtx,
    ) -> impl Future<Output = Result<Self::Output, ProbeError>> + Send + 'a;
}
```

`ProbeCtx` gives the probe:

- `deadline()` and `remaining()`, on the instance clock. A probe should end
  its observation window so that it can return before the deadline (§5.2).
- `cancelled()`, a future that resolves when core is about to cancel the
  invocation, so a probe can clean up cooperatively.
- `db()`, read-only access to the instance (for example, to mark which
  observed topics already have a configured link).
- `clock()` and `time()`, the instance's clock and sleep, so probes are
  runtime-neutral.

```rust
pub struct ProbeError {
    pub kind: ProbeErrorKind,
    pub message: String,   // one line, for a person or an agent
}

pub enum ProbeErrorKind {
    InvalidParams,      // core: params do not deserialize; or the probe rejects them
    Unavailable,        // e.g. no broker connection, gateway unreachable
    Refused,            // e.g. broker refused the subscription, no free tunnel slot
    Timeout,            // core: the deadline passed
    Busy,               // core: concurrency limit reached
    ResponseTooLarge,   // core: serialized result exceeds the bound (§5.3)
    Internal,
}
```

## 3. Registration

```rust
builder.probe(
    ProbeSpec::new("mqtt.discover", MqttDiscover::dedicated(mqtt_config))
        .description("Topics seen on the broker during a window")
        .max_duration(Duration::from_secs(120))
        .concurrency(1),
);
```

- **Name:** unique per instance (a duplicate fails `build()`). Dotted names
  are a convention (`mqtt.discover`, `knx.discover`, `board.info`).
- **Description:** one or two sentences, shown by `probe list` and in the
  MCP tool listing.
- **Schemas:** JSON Schema for `Params` and `Output`, stored as JSON text.
  On `std`, a `schemars` feature fills them from the types
  (`T: JsonSchema`). On embedded targets, the schemas are `&'static str`
  constants that the probe crate generates at build time or writes by hand.
  An instance never generates a schema at runtime. A probe without schemas
  is allowed; clients then see `"schema": null` and rely on the
  description.
- **Limits:** `max_duration` (the cap on what a caller may ask for),
  `concurrency` (default 1), and optionally `max_response_bytes`, which
  overrides the instance default (§5.3).

Registration can happen on the builder, or from a connector's `build()`
through `db.register_probe(spec)` when the probe needs handles that only the
built connector has, such as its live session (§6.3). Core erases the types
behind the spec: it keeps a closure that deserializes the params, runs the
probe and serializes the output.

## 4. Invocation

### 4.1 AimX 4.1

Two one-shot methods:

- **`probe.list`** returns `[{ name, description, params_schema,
  output_schema, max_duration_ms, concurrency }]`.
- **`probe.run`** takes `{ name, params, timeout_ms }`.
  - On success it returns `{ name, started_ns, elapsed_ms, output }`.
  - On failure it returns an AimX error with code `PROBE_<KIND>` (for
    example `PROBE_REFUSED`), and `details: { kind, message }`.
  - `NOT_FOUND` means there is no probe by that name. A 4.0 server answers
    `NOT_FOUND` for the method itself, and the client reports
    "probes need AimX 4.1".

Both methods are allowed under the `ReadOnly` security policy (§7).

The call is awaited inside the AimX session. On a connection where calls
are handled one at a time (for example a serial link with one client), a
long probe holds that connection for its duration. The CLI prints a "waiting
up to N s" line. Over TCP and UDS, the CLI and MCP use a separate
connection.

### 4.2 CLI

```
aimdb probe list [--format text|json]
aimdb probe describe <name>                      # description and both schemas
aimdb probe run <name> [--params <json> | --param k=v ...]
                       [--timeout <secs>] [--format json|pretty]
```

`--param k=v` builds the params object, with values typed by the probe's
params schema when one is present. The output is the `output` JSON (pretty
printed on a TTY). Exit codes: `0` success; `5` probe error, with the kind
and message on stderr. Codes 1–4 keep their meaning.

### 4.3 MCP

- `probe_list` returns the list, schemas included.
- `probe_run` takes `{ name, params, timeout_s }` and returns the output
  JSON.

Both are read-only tools and available without `--allow-writes`. The tool
description tells an agent to check what a result says about its scope and
window before drawing conclusions from an absence (§6.1).

## 5. What AimDB is responsible for

| Responsibility | How |
|---|---|
| Registration | Unique names, description, schemas, limits (§3). |
| Invocation | `probe.list` / `probe.run`, CLI, MCP (§4). |
| Concurrency | Per probe: at most `concurrency` invocations at a time, otherwise `Busy`. Per instance: at most `max_concurrent_probes` (default 2), otherwise `Busy`. No queueing; the caller retries. |
| Deadlines | §5.2. |
| Error transport | `ProbeError` → AimX error code and details → CLI exit code `5` and stderr, or an MCP tool error. |
| Result serialization | `serde_json` into a bounded writer (§5.3). |

### 5.1 Parameters

Core deserializes `params` into `Probe::Params` before calling the probe. A
failure is `InvalidParams` with serde's message, and the probe never runs.
Core does not validate against the schema; deserialization is the check.

### 5.2 Deadlines and cancellation

- The effective deadline is `min(timeout_ms, max_duration)`. When the
  caller gives no timeout, it is `max_duration`.
- `ctx.cancelled()` fires at the deadline. The probe then has a grace
  period (default 2 s) to return, either an `Ok` with partial results
  marked incomplete or an error.
- After the grace period, core drops the future and replies `Timeout`.

**Cleanup belongs to the probe implementation.** Core only promises the
signal and then the drop. A probe holds its resources in values whose
`Drop` releases them without awaiting:

- A dedicated connection closes when its socket is dropped.
- A change to a live session is held as a lease. Dropping the lease
  synchronously queues the undo, such as an `UNSUBSCRIBE`, on the session's
  command queue (§6.3), so the undo happens even when the future is
  dropped.

### 5.3 Limits: response size versus memory inside the probe

These are two different limits with two different owners.

- **Response size, enforced by core.** The serialized `output` must fit
  `max_response_bytes` (instance default: 256 KB on `std`, 8 KB on
  embedded targets; it can be set per probe and per instance). Core
  serializes into a writer that stops at the limit, so an oversized result
  never sits in memory in full twice. If it does not fit, the reply is
  `ResponseTooLarge` with the limit, never a silently truncated JSON value.
  On serial links the AimX frame size also applies and may be smaller.
- **Memory inside the probe, owned by the probe.** Core cannot see or limit
  what a probe allocates while it observes. Every probe bounds its own
  state, for example `max_topics` or `max_addresses` with a sensible
  default, and says in its output when it hit a bound (§6.1). The
  ready-made probes take these bounds as parameters with caps set at
  registration, so a caller cannot ask for unbounded memory.

A probe that respects its own entry limits should not hit
`ResponseTooLarge`. That error is a safety net, not the way to page a
result.

## 6. Ready-made discovery probes

These are the first probes. They answer what configured links cannot: which
signals exist that the instance does not consume yet.

### 6.1 Describing what was observed

Core has no coverage type. Each probe's output type says what it observed,
in its own terms. The ready-made discovery probes include a `coverage`
object in their output: scope, connection kind, the actual window, whether
the result is complete, and which limits cut it short. That object is part
of their `Output`, so it is documented by their schema. A custom probe
decides for itself; if a shared shape proves useful across many probes, it
can move into core later without breaking anything.

**The guideline for every discovery probe:** absence during a window does
not prove that a signal does not exist. A topic that nobody published, and
no broker retained, during the window, or a group address with no telegram
during the window, is simply not observed. A probe should say what its
window and scope were, and whether it hit a limit, so a reader does not
read too much into a short list. The MCP tool description repeats this
caveat.

### 6.2 `MqttDiscover`

**Params:**

| Field | Default | Meaning |
|---|---|---|
| `filter` | `#`, or the registered default | Topic filter to observe. Must lie under the registered `allowed_filter` if one is set. |
| `duration_s` | 10 | Observation window. |
| `max_topics` | 200 (cap set at registration) | Memory bound. |
| `unlinked_only` | false | List only topics that no configured inbound link matches. |

**Output:** `{ coverage, topics: [ { topic, messages, retained, qos_seen,
bytes: {min, max, last}, first_seen_ms, last_seen_ms, linked } ] }`.
`linked` says whether a configured inbound link matches the topic. That is
the bridge from discovery to configuration.

**How it observes** is chosen at registration and always shown in
`coverage.connection`:

- `MqttDiscover::dedicated(config)` opens its own client connection (own
  client ID, clean session, no will message), subscribes to `filter` at
  QoS 0 for the window, then disconnects. It is independent of the running
  session and can see everything its credentials allow. It needs a second
  socket, and for `mqtts` a second TLS session. This is the default on
  `std`.
- `MqttConnector::discover_probe()` is registered by the connector and uses
  the **existing session**:
  - *Passive* (the default): it reports topics the session already
    receives, including matches of wildcard links. It needs no extra socket,
    but it sees only what is already subscribed, and coverage says so.
  - *Live subscription*: only with `.allow_live_subscribe(true)` at
    registration and `live_subscribe: true` in the params. It adds a
    temporary subscription under a lease (§5.2). It refuses a filter that
    overlaps an application subscription, because some brokers would then
    deliver a message twice and a record could get a duplicate. Messages on
    the temporary subscription never reach a record.

Changes to live subscriptions are therefore always explicit, at
registration and in the call.

### 6.3 `KnxDiscover`

**Params:** `ga_prefix` (for example `1/2/`, default all), `duration_s`
(default 30), `max_addresses` (default 500), `unlinked_only`.

**Output:** `{ coverage, group_addresses: [ { ga, telegrams: {write,
response, read}, sources: [individual addresses, up to 4], data_len: {min,
max}, first_seen_ms, last_seen_ms, linked } ] }`.

**How it observes:**

- `KnxConnector::discover_probe()` observes the **existing tunnel**. The
  tunnel already receives every group telegram the gateway forwards,
  including those that no route matches, which today are dropped. This
  needs no extra tunnel slot and sends nothing on the bus. It is the
  default.
- `KnxDiscover::dedicated(gateway)` opens its own tunnel for the window,
  for instances without a KNX connector. It is refused if the gateway has
  no free tunnel slot.

Its note: only telegrams that occur during the window and pass the
gateway's and line couplers' filter tables are observed. Addresses that are
only read rarely, or only used on another line, may be missing.

### 6.4 Developer probes

Anything else is application code using the same mechanism, for example:

- `board.info`: device capabilities (firmware, MAC address, sensors found
  at boot).
- `button.edges`: presses seen by the button input during a window. The
  input exposes a passive tap that the probe subscribes to for the window,
  so the probe never competes with the input for the pin.

Passive taps are an optional technique for probes like these, not a
requirement of the mechanism.

## 7. Safety

- **Only registered code runs.** The registry is fixed at `build()`.
- **Read-only from AimDB's point of view.** A probe has no record writers in
  `ProbeCtx`. What it does in the environment is the developer's code; the
  ready-made probes only listen and send protocol frames (connect,
  keep-alive, and subscribe/unsubscribe only when live subscription is
  explicitly allowed).
- **The developer controls exposure.** Registration sets the allowed filter
  or prefix and whether live subscriptions may change. A probe whose results are sensitive should
  not be registered on an instance that untrusted agents can reach.
- **Bounded:** deadline, grace period, concurrency, response size (§5).

## 8. Implementation outline

1. **Core** (`aimdb-core/src/probe/`): `Probe`, `ProbeCtx`, `ProbeError`,
   `ProbeSpec`, the registry, type erasure, the bounded
   serializer, the AimX `probe.list` / `probe.run` methods, and AimX 4.1.
2. **Client, CLI, MCP:** `aimdb-client::probe::{list, run}`, `aimdb probe`,
   and the `probe_list` / `probe_run` tools.
3. **`MqttDiscover`:** dedicated mode on Tokio, then on Embassy; then
   passive existing-session mode; then live subscription with the lease.
4. **`KnxDiscover`:** existing-tunnel mode (passing unmatched telegrams to
   the probe while it runs), then dedicated mode.
5. **Demos:** `board.info` and `button.edges` in the Embassy demos, showing
   developer probes.

**Testing:**

- Core: params errors, `Busy`, deadline and grace period, a dropped future
  runs the lease's undo, `ResponseTooLarge` without full buffering, and
  schema listing.
- MQTT: dedicated and existing-session modes against a local broker in CI;
  an overlapping filter is refused; nothing reaches a record.
- KNX: the existing-tunnel tap against the in-process gateway used by the
  current client tests; nothing is sent.
- End to end: the CLI `probe run` exit codes and JSON output.

## 9. Open questions

1. **`schemars` on `no_std`:** check whether build-time schema generation
   for embedded probe crates can use `schemars`, or needs a small build
   script.
2. **Long probes on a shared serial link:** v1 holds the connection for the
   probe's duration. Streaming progress or an async job (`probe.start` /
   `probe.result`) could come later if this hurts.
3. **Payload samples:** the ready-made probes return no payload bytes,
   only names, counts, sizes and times. Optional samples, to help write a
   data contract for a discovered signal, can be added later as an opt-in
   at registration and in the call. Until then, a developer who needs them
   writes their own probe.
4. **Generating configuration from results:** turning a discovered,
   `linked: false` topic into a `link_from` declaration and a data contract
   draft is a natural next step for the CLI and the architecture agent.
