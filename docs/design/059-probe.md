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
- `response_budget()`, the number of bytes the serialized `Output` may take
  in this invocation, with envelope and transport limits already deducted
  (§5.3).

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

A developer's own probe is an async function with its own parameter and
output types. `probe_fn` turns it into a `Probe`, so no trait implementation
is needed:

```rust
#[derive(Deserialize, JsonSchema)]
struct ScanParams {
    /// I²C bus to scan, 0 or 1.
    bus: u8,
}

#[derive(Serialize, JsonSchema)]
struct ScanReport {
    bus: u8,
    devices: Vec<I2cDevice>,
}

#[derive(Serialize, JsonSchema)]
struct I2cDevice {
    address: u8,
    /// Best guess from the address, e.g. "BME280".
    likely: Option<&'static str>,
}

let i2c = board.i2c_handle();   // whatever the application already has

builder.probe(
    ProbeSpec::new(
        "board.i2c_scan",
        probe_fn(move |p: ScanParams, ctx: &ProbeCtx| {
            let i2c = i2c.clone();
            async move {
                if p.bus > 1 {
                    return Err(ProbeError::invalid_params("bus must be 0 or 1"));
                }
                let devices = i2c
                    .scan(p.bus, ctx.remaining())
                    .await
                    .map_err(|e| ProbeError::unavailable(e))?
                    .into_iter()
                    .map(|address| I2cDevice { address, likely: guess(address) })
                    .collect();
                Ok(ScanReport { bus: p.bus, devices })
            }
        }),
    )
    .description("Addresses that answer on an I²C bus")
    .max_duration(Duration::from_secs(5)),
);
```

```
$ aimdb probe run board.i2c_scan --param bus=0
{ "bus": 0, "devices": [ { "address": 118, "likely": "BME280" } ] }
```

`probe_fn` accepts any `Fn(P, &ProbeCtx) -> impl Future<Output =
Result<O, ProbeError>>` with `P: DeserializeOwned` and `O: Serialize`.
With the `schemars` feature, the spec takes both schemas from `P` and `O`.
Implementing `Probe` directly is for reusable probes that carry
configuration, like the ready-made ones:

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
built connector has, such as its running KNX tunnel (§6.3). Core erases the types
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
`Drop` releases them completely, without awaiting and without anything that
can fail:

- A dedicated connection is owned by the probe's future. Dropping it closes
  the socket; the broker or gateway ends the session on its side. Nothing of
  the running instance is involved.
- An observer on a running session (the KNX tunnel, §6.3) is a slot the
  session reads on each telegram. Dropping the observer empties the slot.
  This is a plain store, with no queue that could be full and nothing to
  undo on the session.

The first release has no probe that changes a running session. Changing
live subscriptions needs a cleanup mechanism that this pattern cannot
provide; §9 lists what it requires.

### 5.3 Limits: response size versus memory inside the probe

These are two different limits with two different owners.

**Response size: the budget comes from core, and the probe stays within it.**

Core computes the budget once per invocation and exposes it as
`ctx.response_budget()`:

```
budget = min(probe max_response_bytes,      // registration, optional
             instance max_response_bytes,   // 256 KB std, 8 KB embedded
             connection reply limit)        // from the transport
         - envelope reserve
```

- **Connection reply limit:** each AimX transport states the largest reply
  frame its peer accepts. Serial: the frame cap of the receiving
  accumulator, `DEFAULT_MAX_FRAME` (8 KB) unless configured. TCP: the
  framer's `max_frame`. UDS: none. The CLI and MCP connect with the same
  defaults, so the server's figure matches what the client accepts.
- **Envelope reserve:** the bytes of the reply frame around `output`:
  `{"t":"reply","id":…,"ok":{"name":"…","started_ns":…,"elapsed_ms":…,"output":`
  and the closing braces. Numbers are counted at their maximum width, and
  the name at its actual length. `output` is spliced into the frame
  verbatim (`RawValue`, as record values are today), so its bytes count
  one for one.

A successful observation must not be lost at the end, so the ready-made
probes **stop collecting before they exceed the budget**:

- They keep entries in `BudgetedVec<T>`, a core helper. Each entry type
  provides `max_serialized_len(&self)`: its JSON length with every counter
  at the full width of its type (10 digits for the `u32` counts and
  milliseconds the ready-made probes use) and strings at their actual
  length.
  An entry can therefore grow (more messages, a later `last_seen_ms`)
  without ever outgrowing the space reserved for it.
- The probe first reserves room for the parts of its output that are not
  entries, such as the `coverage` object. Its strings are cut to fixed
  maximum lengths, so its size has a known bound.
- `try_push(entry)` admits a new entry only if its reserved length still
  fits. When it does not, the probe keeps counting entries it has already
  admitted, stops adding new names, counts the messages it could not list,
  and marks the result incomplete: `complete: false`, `limits: ["response
  budget (7 900 bytes) reached after 58 topics; 214 messages on unlisted
  topics"]`.
- The entry caps (`max_topics`, `max_addresses`) remain the memory bound.
  Whichever limit is reached first marks the result incomplete.

On an embedded target with the 8 KB default, this lists roughly 30–50
topics or group addresses (about 160 bytes per typical entry, up to about
250 reserved), depending on topic length, and says plainly when there were
more.
That is a valid, useful answer, rather than a `ResponseTooLarge` after a
full window.

`ResponseTooLarge` remains as the safety net for probes that ignore the
budget. Core serializes into a writer that stops at the limit, so an
oversized result is never held in full twice. The error carries the
budget, so the developer can see what to fit into. Custom probes can use
`ctx.response_budget()` and `BudgetedVec` the same way.

**Memory inside the probe: owned by the probe.** Core cannot see or limit
what a probe allocates while it observes. Every probe bounds its own state,
for example `max_topics` or `max_addresses`, and says in its output when it
hit a bound (§6.1). The ready-made probes take these bounds as parameters,
capped at registration, so a caller cannot ask for unbounded memory. Their
embedded defaults are chosen to match the response budget, so memory is not
spent on entries that could never be returned.

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
| `max_topics` | 200 on `std`, 48 on embedded (cap set at registration) | Memory bound. The response budget may end the list earlier (§5.3). |
| `unlinked_only` | false | List only topics that no configured inbound link matches. |

**Output:** `{ coverage, topics: [ { topic, messages, retained, qos_seen,
bytes: {min, max, last}, first_seen_ms, last_seen_ms, linked } ] }`.
`linked` says whether a configured inbound link matches the topic. That is
the bridge from discovery to configuration.

**How it observes:** `MqttDiscover::dedicated(config)` opens its own
client connection (own client ID, clean session, no will message),
subscribes to `filter` at QoS 0 for the window, then disconnects. It is
independent of the running session and can see everything its credentials
allow. Nothing it receives reaches a record. It needs a second socket, and
for `mqtts` a second TLS session, while it runs. It runs on Tokio and
Embassy, and `coverage.connection` is `"dedicated"`.

Observing the running MQTT session, passively or with a temporary
subscription, is not part of the first release (§9).

### 6.3 `KnxDiscover`

**Params:** `ga_prefix` (for example `1/2/`, default all), `duration_s`
(default 30), `max_addresses` (500 on `std`, 48 on embedded; the response
budget may end the list earlier, §5.3), `unlinked_only`.

**Output:** `{ coverage, group_addresses: [ { ga, telegrams: {write,
response, read}, sources: [individual addresses, up to 4], data_len: {min,
max}, first_seen_ms, last_seen_ms, linked } ] }`.

**How it observes:**

- `KnxConnector::discover_probe()` observes the **existing tunnel**. The
  tunnel already receives every group telegram the gateway forwards,
  including those that no route matches, which today are dropped. This
  needs no extra tunnel slot and sends nothing on the bus. It observes
  through an observer slot (§5.2) and changes nothing on the tunnel. This
  is the first release's KNX probe.
- Later: `KnxDiscover::dedicated(gateway)` opens its own tunnel for the
  window, for instances without a KNX connector. It is refused if the
  gateway has no free tunnel slot.

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
  ready-made probes only listen. The MQTT probe sends only the frames of
  its own connection (connect, subscribe, keep-alive, disconnect). The KNX
  probe sends nothing.
- **The developer controls exposure.** Registration sets the allowed filter
  or prefix. A probe whose results are sensitive should not be registered on
  an instance that untrusted agents can reach.
- **Bounded:** deadline, grace period, concurrency, response size (§5).

## 8. First release

1. **Core** (`aimdb-core/src/probe/`): `Probe`, `probe_fn`, `ProbeCtx`
   with `response_budget()`, `ProbeError`, `ProbeSpec`, the registry, type
   erasure, `BudgetedVec`, the bounded serializer, the reply limits of the
   AimX transports, `probe.list` / `probe.run`, and AimX 4.1.
2. **Client, CLI, MCP:** `aimdb-client::probe::{list, run}`, `aimdb probe`,
   and the `probe_list` / `probe_run` tools.
3. **`MqttDiscover::dedicated`** on Tokio and Embassy.
4. **`KnxConnector::discover_probe()`** on the existing tunnel, with an
   observer slot that receives unmatched telegrams only while a probe runs.
5. **Demos:** a `probe_fn` example (`board.info`) in an Embassy demo.

**Testing:**

- Core: params errors, `Busy`, deadline and grace period, the budget
  computation per transport (serial 8 KB, TCP `max_frame`, UDS unbounded),
  `BudgetedVec` never letting a complete result exceed the budget even as
  counters grow to full width, `ResponseTooLarge` without full buffering,
  and schema listing.
- MQTT: dedicated discovery against a local broker in CI. With many long
  topics and an 8 KB budget, it returns a valid result marked incomplete;
  nothing reaches a record.
- KNX: the existing-tunnel observer against the in-process gateway used by
  the current client tests; nothing is sent; dropping the probe mid-window
  leaves the tunnel unchanged.
- End to end: the CLI `probe run` exit codes and JSON output, including over
  a serial link at the 8 KB frame limit.

## 9. Later: probes that use the running MQTT session

Not in the first release. Observing the running session **passively**
(topics it already receives) needs only an observer slot, like KNX, and can
follow soon.

Adding a **temporary subscription** to the running session is different. A
lease whose `Drop` queues an `UNSUBSCRIBE` does not guarantee cleanup: the
command queue can be full, the session can reconnect while the lease is
held, and another probe, or the application, can hold the same filter. A
design for it must provide at least:

- **Session-owned subscription state.** The session keeps the desired set
  of subscriptions with an owner per entry: the application's routes, or
  probe lease *n*. Probes never send `SUBSCRIBE` or `UNSUBSCRIBE`
  themselves.
- **Reconciliation instead of queued commands.** Dropping a lease only
  marks its entry released, which is a plain store that cannot fail. The
  session loop compares the desired set with the broker's set and sends
  what differs, retrying until they match.
- **Reconnect from the desired set.** After a reconnect the session
  subscribes only what is desired, so a released lease is not restored, and
  a lease still held is.
- **Shared filters are reference-counted.** An `UNSUBSCRIBE` goes out only
  when no owner is left. A probe's release can never remove a subscription
  the application or another probe still holds.
- **No overlap with application subscriptions,** or duplicate deliveries
  that can be told apart (MQTT v5 subscription identifiers), so that a
  probe never causes a record to receive a message twice.
- **Explicit opt-in** at registration and in each call.

This is mostly work on the MQTT session, not on the probe mechanism, and
it can ship separately.

## 10. Open questions

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
