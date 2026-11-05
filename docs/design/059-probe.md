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

`ProbeCtx` is a cheap, cloneable handle (an `Arc` inside), so a probe can
move a copy into the futures it creates. It gives the probe:

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
        probe_fn(move |p: ScanParams, ctx: ProbeCtx| {
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

`probe_fn` takes the context **by value**:

```rust
pub fn probe_fn<F, Fut, P, O>(f: F) -> ProbeFn<F, P, O>
where
    F: Fn(P, ProbeCtx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, ProbeError>> + Send + 'static,
    P: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static;
```

A closure that took `&ProbeCtx` and returned a future borrowing it cannot
be expressed with a plain `Fn(P, &ProbeCtx) -> Fut` bound, because `Fut`
would have to depend on the reference's lifetime. Passing an owned handle
avoids this, and the returned future is `'static`. `ProbeFn` implements
`Probe` by cloning the context into the call. This signature, the example
above and core's type erasure have been checked to compile and run with
`serde` and `serde_json`: valid params produce the report, a rejected bus
and a mistyped field both give `InvalidParams`, and the erased future is
`Send`.
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

Connectors are built after the database exists: `AimDbBuilder::build()`
calls each `ConnectorBuilder::build(&db)` with the shared `&AimDb`. So
`register_probe(&self, …)` needs a registry with interior mutability. It
holds the builder's specs, accepts further specs while connectors build, and
is **sealed when `build()` returns**. A later `register_probe` returns an
error. A duplicate name fails `build()` whether it comes from the builder or
from a connector. `probe.list` and `probe.run` read the sealed registry on
every call, so the order in which connectors are built (including the
connector that serves AimX) does not matter.

## 4. Invocation

### 4.1 AimX 4.1

Two one-shot methods:

- **`probe.list`** returns `[{ name, description, params_schema,
  output_schema, max_duration_ms, concurrency }]`.
- **`probe.run`** takes `{ name, params, timeout_ms }`.
  - On success it returns `{ name, started_ns, elapsed_ms, output }`.
  - On failure it returns an error reply with code `probe_<kind>` (for
    example `probe_refused`) and a one-line `msg` (see *Error replies*
    below).
  - `not_found` means there is no probe by that name.

Both methods are allowed under the `ReadOnly` security policy (§7).

**Version detection.** A 4.0 server also answers `not_found`, for the
method itself, so the error alone cannot tell "no such probe" from "server
too old". The client already receives the server's protocol version in the
handshake reply. `aimdb-client::probe` checks that version first and reports
"probes need AimX 4.1" without sending the call; after that, `not_found`
always means an unknown probe name.

**Error replies.** Today an AimX error reply carries only a code string,
`{"t":"reply","id":…,"err":"not_found"}`, and `RpcError` is a set of
data-free variants (`NotFound`, `Denied`, `Internal`, `VersionMismatch`).
AimX 4.1 adds:

- an optional `msg` field on the error frame, next to `err`, holding the
  probe's one-line message;
- an `RpcError::Probe { kind, message }` variant, encoded as
  `err: "probe_<kind>"` plus `msg`, and decoded back by the 4.1 client.

The change is additive. A 4.0 client maps every unknown code to
`RpcError::Internal` and ignores the unknown field, so it degrades to a
generic error instead of failing to decode. That holds even when `msg`
contains escaped characters. The existing codes keep their spelling.

The frame decoder borrows its string fields (`err`, `method`, `topic`) as
`&str` straight from the frame, which works only for strings without
escapes. That is fine for codes, but a probe message can contain quotes
or line breaks. So `msg` is decoded as an owned string (or `Cow<str>`).

**The connection stays live while a probe runs.** Today the per-connection
loop in `session/server.rs` awaits every call inline. While it waits, it
reads no further requests and forwards no subscription events. Those queue
in the bounded event funnel (`EVENT_BUFFER`, 256) and are dropped once it is
full. That is harmless for calls that take microseconds, but a probe can
take minutes, and it would affect every transport, not only serial.

So `probe.run` is not awaited inline. `Session::call` borrows the session
`&mut` for as long as its future runs, so the loop cannot keep serving the
connection while it holds that future. The `Session` trait therefore gains a
defaulted method, in the same way `subscribe` already returns a `'static`
stream:

```rust
/// A call whose reply may take long. `Some` takes the call over: the
/// engine drives the returned future next to the subscription pumps and
/// sends its result as the reply. `None` (the default) falls back to `call`.
fn call_deferred(
    &mut self,
    method: &str,
    params: Payload,
) -> Option<BoxFut<'static, Result<Payload, RpcError>>> {
    None
}
```

The AimX session returns `Some` for `probe.run`. The future owns what it
needs (the probe's `Arc`'d spec, a clone of the database handle and the
`ProbeCtx`) and borrows nothing from the session. The connection loop
drives it in a `FuturesUnordered`, as it does the subscription pumps, and
encodes and sends the result when it completes. Meanwhile the loop keeps reading
frames, answering other calls and forwarding events. Other methods stay
inline. A connection has at most one probe in flight; a second `probe.run`
on the same connection gets `probe_busy`. A new handshake on the connection,
or its close, drops the in-flight probe like the subscription pumps, which
cancels it (§5.2).

With this, a long probe no longer holds a serial link either. The CLI still
prints a "waiting up to N s" line, because its own call waits for the
reply.

### 4.2 CLI

```
aimdb probe list [--format text|json]
aimdb probe describe <name>                      # description and both schemas
aimdb probe run <name> [--params <json> | --param k=v ...]
                       [--timeout <secs>] [--format json|pretty]
```

`--param k=v` builds the params object, with values typed by the probe's
params schema when one is present. The output is the `output` JSON (pretty
printed on a TTY). Exit codes: `0` success; `4` (the existing timeout
code) for `probe_timeout`; `5` for any other probe error. The kind and
message go to stderr. Codes 1–4 keep their meaning.

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
| Concurrency | Per probe: at most `concurrency` invocations at a time, otherwise `Busy`. Per instance: at most `max_concurrent_probes` (default 2), otherwise `Busy`. Per connection: at most one in flight (§4.1), otherwise `Busy`. No queueing; the caller retries. |
| Deadlines | §5.2. |
| Error transport | `ProbeError` → AimX error reply `probe_<kind>` with `msg` (§4.1) → CLI exit code `4` or `5` and stderr, or an MCP tool error. |
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
- An observer on a running session (the KNX tunnel engine, §6.3) is a slot
  the session reads on each telegram. Dropping the observer empties the slot.
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
             instance max_response_bytes,   // new: 256 KB std, 8 KB embedded
             connection reply limit)        // from the transport
         - framing overhead                 // from the transport
         - envelope reserve
```

`max_response_bytes` is a new instance setting; nothing bounds reply size
today apart from the transports' frame caps.

- **Connection reply limit:** each AimX transport states the largest reply
  frame it assumes its peer accepts. A frame cap is a *receive* limit: the
  server cannot see the client's, so it uses its own transport's default and
  relies on the client using the same one. That holds for the CLI and MCP,
  which connect with the transports' defaults; a client configured with a
  smaller cap gets the frame dropped. The figures:
  - Serial: `DEFAULT_MAX_FRAME` (8 KB) unless configured. The receiving
    `FrameAccumulator` counts **COBS-encoded** bytes before the delimiter,
    not the decoded payload. It drops a run only once it has buffered more
    than the cap without seeing a delimiter. So a frame whose encoded length
    is within the cap always arrives, however the reads are chunked. A
    longer frame can still arrive when it comes in one read, but nothing
    guarantees it. The budget therefore keeps the encoded length within the
    cap.
  - TCP: the length framer's `DEFAULT_MAX_FRAME` (64 KB) unless configured.
  - UDS: none.
- **Framing overhead:** what the transport adds to the reply bytes before
  the receiver's cap applies. Serial: COBS adds about one byte per 254
  bytes. Core computes the serial budget with
  `cobs::max_encoding_length`, so that the encoded frame still fits: at
  8 KB the largest payload is 8 159 bytes, 33 less. The delimiter is
  not counted against the cap. TCP: the length prefix is outside the
  counted payload, so nothing. The existing `instance_info_bounds` serial
  test measures replies the same way: envelope with the longest request
  id, COBS-encoded.
- **Envelope reserve:** the bytes of the reply frame around `output`:
  `{"t":"reply","id":…,"ok":{"name":"…","started_ns":…,"elapsed_ms":…,"output":`
  and the closing braces. Numbers are counted at their maximum width, and
  the name at its actual length. `output` is spliced into the frame
  verbatim (`RawValue`, as record values are today), so its bytes count
  one for one.

A successful observation must not be lost at the end, so the ready-made
probes **stop collecting before they exceed the budget**:

- They keep entries in `BudgetedVec<T>`, a core helper. Each entry type
  provides `max_serialized_len(&self)`: the **largest size the entry can
  reach at any point during the invocation**, not its current size. It is
  computed when the entry is admitted:
  - fields fixed at admission (the topic, the group address) count at
    their actual JSON length, including escaping;
  - counters and times count at the full width of their type (10 digits
    for the `u32` counts and milliseconds the ready-made probes use);
  - collections that can grow count at their capacity, filled with
    maximum-length elements. A KNX entry's `sources` counts as four
    individual addresses of the longest form (`"15.15.255"`) with their
    separators, even if only one has been seen so far;
  - optional fields count as present, with their largest value.

  A field that can change after admission must have such a bound. An entry
  type with an unbounded growing field cannot go into a `BudgetedVec`. The
  entry can then grow in any way it is allowed to without outgrowing the
  space reserved for it. In debug builds, `BudgetedVec` checks at
  serialization that each entry fits its reservation.
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
budget. The error carries the budget, so the developer can see what to fit
into. Custom probes can use `ctx.response_budget()` and `BudgetedVec` the
same way. How core serializes depends on the target:

- **`std`:** core serializes into a writer that stops at the limit
  (`serde_json::Serializer` over a bounded `io::Write`), so an oversized
  result is never held in full.
- **`no_std`:** the workspace builds `serde_json` with `alloc` only, which
  has `to_vec` but no streaming `Serializer` or `to_writer`. Core serializes
  with `to_vec`, checks the length and drops the buffer if it is over the
  budget. The result is then held in full once, and the reply frame is
  built from it. Within budget that is at most the instance's
  `max_response_bytes` (8 KB by default). A result far over budget is
  allocated before it is rejected, which is one more reason the ready-made
  probes stay within the budget themselves. A streaming bounded serializer
  for `no_std` is an open question (§10).

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

On Embassy the dedicated connection cannot borrow anything from the running
session, and its memory has to be provided at registration:

- **Buffers:** its own MQTT packet buffer (the session uses 4 KB,
  `BUFFER_SIZE`) and, for `mqtts`, its own TLS resources. The session's
  `TlsOptions` hold `&'static mut` read and write buffers and a
  `&'static mut` RNG, handed over once and not shareable. So
  `MqttDiscover::dedicated` takes its own `TlsOptions`, with its own
  buffers and RNG (or plain-TCP buffers). Only one invocation can use them
  at a time, which matches its `concurrency(1)`.
- **Socket:** one more TCP socket while the probe runs. The application
  sizes the `embassy-net` `StackResources<N>` (the demos use 4 or 5) and
  raises `N` by one.
- **Connection settings:** the session leaks its client ID and
  credentials to `'static` once at startup. The probe builds its settings
  once at registration, with its own client ID, and reuses them on every
  run, so repeated invocations do not leak.

The Embassy demo (§8) documents the RAM this costs.

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
  tunnel already receives every group telegram the gateway forwards. This
  needs no extra tunnel slot and sends nothing on the bus. It observes
  through an observer slot (§5.2) and changes nothing on the tunnel. This
  is the first release's KNX probe.

  **Where the observer sits.** Today `parse_telegram` in `tunnel.rs`
  reduces each cEMI frame to a group address and a payload before anything
  else sees it. It keeps only `GroupValue_Write`, so reads and responses
  are dropped. It discards the source individual address. A 6-bit value
  becomes a one-byte payload. The probe's `telegrams: {write, response,
  read}`, `sources` and `data_len` cannot be computed from what comes out
  of it, so the observer cannot sit at `TunnelIo::forward` or at the
  `InboundDispatch` either. It sits in the tunnel engine, where the
  `TunnellingRequest` is handled. For every `L_Data` frame with a group
  destination, before the write-only filter, it records the destination,
  the APCI kind (read, response, write), the source address and the data
  length on the wire (`0` for a 6-bit value). knx-pico's `LData` already
  exposes `source` and `apci`, so nothing new is parsed. The observer slot
  holds a
  plain summary struct, not the raw frame. The path that delivers writes to
  records is unchanged.
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

- **Only registered code runs.** The registry is sealed when `build()`
  returns (§3).
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
   with `response_budget()`, `ProbeError`, `ProbeSpec`, the registry
   (interior-mutable, sealed at the end of `build()`), type erasure,
   `BudgetedVec`, the bounded serializer (streaming on `std`, `to_vec` and
   a length check on `no_std`), the instance's `max_response_bytes`, and
   `probe.list` / `probe.run`.
2. **AimX 4.1 wire and session** (`aimdb-core/src/session/`): the optional
   `msg` field on error frames, `RpcError::Probe { kind, message }` and
   its `probe_<kind>` codes, `Session::call_deferred` with deferred replies
   driven by the connection loop and at most one probe in flight per
   connection (§4.1), and each
   transport's reply limit and framing overhead (§5.3).
3. **Client, CLI, MCP:** `aimdb-client::probe::{list, run}` with the
   version check from the handshake, `aimdb probe`, and the `probe_list` /
   `probe_run` tools.
4. **`MqttDiscover::dedicated`** on Tokio and Embassy, with its own
   buffers, TLS resources and connection settings on Embassy (§6.2).
5. **`KnxConnector::discover_probe()`** on the existing tunnel, with an
   observer slot in the tunnel engine that receives a summary of every
   group telegram, before the write-only filter, only while a probe runs
   (§6.3).
6. **Demos:** a `probe_fn` example (`board.info`) in an Embassy demo, with
   the RAM cost of a dedicated MQTT probe noted.

**Testing:**

- Core: params errors, `Busy`, deadline and grace period, the budget
  computation per transport (serial 8 KB measured COBS-encoded, TCP
  `max_frame`, UDS unbounded), `BudgetedVec` never letting a complete
  result exceed the budget even as counters grow to full width and KNX
  `sources` fill to four, `ResponseTooLarge` (without full buffering on
  `std`), registration after `build()` refused, and schema listing.
- Session: while a probe runs on a connection, the same connection answers
  `record.get` and keeps delivering subscription events without gaps; a
  second `probe.run` on it gets `probe_busy`; a new handshake or a closed
  connection cancels the probe.
- Wire compatibility: a 4.1 error frame with `msg` decodes as
  `RpcError::Internal` on a 4.0 client; the 4.1 client reports "probes need
  AimX 4.1" against a 4.0 server without sending `probe.run`.
- MQTT: dedicated discovery against a local broker in CI. With many long
  topics and an 8 KB budget, it returns a valid result marked incomplete;
  nothing reaches a record.
- KNX: the tunnel-engine observer against the fake gateway used by the
  current client tests, fed group reads, responses, writes and 6-bit
  values from several sources; nothing is sent; writes still reach their
  records; dropping the probe mid-window leaves the tunnel unchanged.
- End to end: the CLI `probe run` exit codes (`4` on timeout, `5` on other
  probe errors) and JSON output, including over a serial link at the 8 KB
  frame limit.

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
2. **Progress for long probes:** the connection stays live during a probe
   (§4.1), but the caller sees nothing until the reply. Streaming progress
   or an async job (`probe.start` / `probe.result`) could come later if
   this hurts.
3. **Payload samples:** the ready-made probes return no payload bytes,
   only names, counts, sizes and times. Optional samples, to help write a
   data contract for a discovered signal, can be added later as an opt-in
   at registration and in the call. Until then, a developer who needs them
   writes their own probe.
4. **Bounded serialization on `no_std`:** `serde_json` with `alloc` has no
   streaming serializer, so v1 checks the length after `to_vec` (§5.3).
   Options for a true bound: `serde-json-core` into a fixed buffer, or a
   size-counting pass before encoding.
5. **Generating configuration from results:** turning a discovered,
   `linked: false` topic into a `link_from` declaration and a data contract
   draft is a natural next step for the CLI and the architecture agent.
