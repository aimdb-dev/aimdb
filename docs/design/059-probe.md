# 059 — Probes

**Status:** 📝 Proposed

**Scope:** Named, developer-defined async inspection functions that are
registered on an AimDB instance and invoked on demand through the CLI or MCP.
This covers the `Probe` trait, registration with a description and schemas,
invocation and cancellation over AimX, concurrency, deadlines, error
transport, result
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

$ aimdb probe run mqtt.discover --params '{"filter":"plant/#","duration_s":15}'
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

- `deadline()` and `remaining()`, on the instance clock. A probe must return
  before the deadline; at the deadline core drops it (§5.2).
- `db()`, a read-only view of the instance. It returns a new `ProbeDb`
  type, not the `AimDb` handle, because `AimDb` can produce and set
  records. `ProbeDb` wraps the handle and exposes only reads: the record
  metadata, a record's latest value as JSON, and
  `inbound_links(scheme)`, the topics and patterns of the configured
  inbound links of one connector scheme. The discovery probes use the
  last to mark results as `linked`. Core has no such listing today; it is
  new, and built from the same link declarations the connectors read.
- `clock()` and `time()`, the instance's clock and sleep, so probes are
  runtime-neutral.
- `response_budget()`, the number of bytes the serialized `Output` may take
  in this invocation, with envelope and transport limits already deducted
  (§5.3).

```rust
pub struct ProbeError {
    pub kind: ProbeErrorKind,
    pub message: String,   // for a person or an agent; bounded by core (§4.1)
}

pub enum ProbeErrorKind {
    InvalidParams,      // core: params do not deserialize; or the probe rejects them
    Unavailable,        // e.g. no broker connection, gateway unreachable
    Refused,            // e.g. broker refused the subscription, no free tunnel slot
    Timeout,            // core: the deadline passed
    Busy,               // core: concurrency limit reached
    Cancelled,          // core: the caller cancelled the invocation (§4.1)
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
#[serde(deny_unknown_fields)]   // a misspelled field is an error (§5.1)
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
$ aimdb probe run board.i2c_scan --params '{"bus":0}'
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
configuration, like the ready-made ones. The application registers those
too, and gives them a name, a description and limits like any other
probe:

```rust
// MQTT: the probe dials its own connection (§6.2).
builder.probe(
    ProbeSpec::new("mqtt.discover", MqttDiscover::dedicated(probe_dialer, &broker_url))
        .description("Topics seen on the broker during a window")
        .max_duration(Duration::from_secs(120)),
);

// KNX: the connector hands out a tap on its tunnel before it is moved
// into the builder; the probe is built on the tap (§6.3).
let knx_connector = KnxConnector::new(binder, delay, &gateway_url);
let tap = knx_connector.tap();
let knx = builder.connect(knx_connector);
builder.probe(
    ProbeSpec::new("knx.discover", KnxDiscover::new(tap))
        .description("Group addresses seen on the bus during a window")
        .max_duration(Duration::from_secs(300)),
);
```

A connector never registers a probe. It exposes what only it can provide,
such as the tap, and the application decides which probes exist.

- **Name:** unique per instance (a duplicate fails `build()`). At most
  `PROBE_NAME_MAX` (64) bytes of `[a-z0-9._-]`, so it never needs JSON
  escaping. Dotted names are a convention (`mqtt.discover`, `knx.discover`,
  `board.info`).
- **Description:** one or two sentences, shown by `probe list` and in the
  MCP tool listing. At most `PROBE_DESCRIPTION_MAX` (256) bytes of UTF-8
  with no control characters, so its JSON form is at most twice that (only
  `"` and `\` are escaped).
- **Schemas:** JSON Schema for `Params` and `Output`, stored as JSON text.
  On `std`, a `schemars` feature fills them from the types
  (`T: JsonSchema`). On embedded targets, the schemas are `&'static str`
  constants that the probe crate generates at build time or writes by hand.
  An instance never generates a schema at runtime. Registration checks that
  each schema is valid JSON. Schemas are documentation for people and
  agents; core does not interpret them (§5.1). A probe without schemas is
  allowed; clients then see `null` and rely on the description.
- **Describe size:** a probe's `probe.describe` reply (§4.1), with its
  description and both schemas, must fit `PROBE_DESCRIBE_MAX` (6 KB), the
  same bound `instance.info` uses. Registration computes the reply's exact
  length, so schemas count as stored, whitespace included.
- **Limits:** `max_duration` (the cap on what a caller may ask for),
  `concurrency` (default 1), and optionally `max_response_bytes`, which
  overrides the instance default (§5.3).

A spec that breaks one of these limits fails `build()` with an error that
names the probe, the limit and the actual size. The limits keep every
discovery reply within the smallest AimX frame (§4.1). The tradeoff is
explicit: a probe whose schemas do not fit is unsupported in v1. It can
register without schemas, or with shorter ones, and rely on its
description. Typical schemas are far below the bound: the full
`mqtt.discover` describe reply, with schemars-style schemas for its params
and output, is about 2 KB.

Registration happens on the builder only, before `build()`. Core erases
the types behind the spec: it keeps a closure that deserializes the
params, runs the probe and serializes the output.

`build()` checks the specs and puts them into the registry, which is
immutable from then on. It lives in the instance's existing `Extensions`,
so `AimDbInner` does not change, and it needs no lock for lookups and no
sealing step. A duplicate name fails `build()`. A probe that needs
something from a connector gets it as a handle the connector builder
hands out before `build()`, like the KNX tap. Such a handle must work
before the connector runs: until then it simply reports nothing.

## 4. Invocation

### 4.1 AimX 4.1

Four one-shot methods:

- **`probe.list`** takes `{ after, limit }`, both optional, and returns one
  page of compact entries: `{ probes: [{ name, description,
  max_duration_ms, concurrency }], next }`. Entries are sorted by name.
  `next` is the last name on the page when more follow, otherwise `null`,
  and the caller passes it back as `after`. Schemas are not included.
- **`probe.describe`** takes `{ name }` and returns one probe in full:
  `{ name, description, max_duration_ms, concurrency, params_schema,
  output_schema }`, the schemas spliced in as stored JSON (or `null`). Its
  size is checked at registration (§3).
- **`probe.run`** takes `{ name, params, timeout_ms }`.
  - On success it returns `{ name, started_ns, elapsed_ms, output }`.
  - On failure it returns an error reply with code `probe_<kind>` (for
    example `probe_refused`) and a bounded `msg` (see *Error replies*
    below).
  - `not_found` means there is no probe by that name.
- **`probe.cancel`** takes no parameters and returns `{ cancelled }`. It
  cancels the probe in flight on this connection; a connection has at most
  one, so no name or handle is needed. `cancelled` is `false` when no probe
  was in flight, for example because it finished just before. See
  *Cancellation* below.

All four are allowed under the `ReadOnly` security policy (§7).

**Discovery replies have a fixed bound.** `instance.info` already
guarantees its reply fits the smallest AimX frame (`INSTANCE_INFO_REPLY_MAX`,
6 KB, within serial's 8 KB). The probe discovery methods follow that rule, so
a large registry can never make listing unusable:

- A `probe.list` entry is at most `PROBE_LIST_ENTRY_MAX` bytes: its keys,
  a 64-byte name, a 512-byte escaped description and numbers at full width:
  668 bytes. Core puts entries on a page while they fit the connection's
  response budget (§5.3), computed for the page reply, and sets `next` for
  the rest. `limit` can only shorten a page. On serial that is at least
  twelve entries per page; over TCP and UDS, normally all of them. A page
  holds at least one entry, which always fits. Twelve worst-case entries
  fill the serial payload exactly, with no byte to spare, so an entry that
  gains a field brings the guaranteed page down to eleven.
- A `probe.describe` reply body is at most `PROBE_DESCRIBE_MAX` (6 KB),
  enforced at registration (§3).
- An error reply carries at most a 32-byte code and a 256-byte message
  (below).

Each bound fits serial's 8 KB frame with COBS overhead. Measured through
the AimX reply envelope with the longest request id and COBS: twelve
worst-case entries per page, a 6 KB describe body at 6 214 bytes and an
error reply at 604 bytes. A test checks them the way
`instance_info_bounds` checks `instance.info` (§8).

**Version detection.** A 4.0 server also answers `not_found`, for the
method itself, so the error alone cannot tell "no such probe" from "server
too old". The client already receives the server's protocol version in the
handshake reply. `aimdb-client::probe` checks that version first and reports
"probes need AimX 4.1" without sending the call; after that, `not_found`
always means an unknown probe name. The check compares major and minor;
the existing `version_compatible` compares the major only, so it needs a
second helper.

**Error replies.** Today an AimX error reply carries only a code string,
`{"t":"reply","id":…,"err":"not_found"}`, and `RpcError` is a set of
data-free variants (`NotFound`, `Denied`, `Internal`, `VersionMismatch`).
AimX 4.1 adds:

- an optional `msg` field on the error frame, next to `err`, holding the
  probe's message, bounded by core: before encoding, core replaces control
  characters (line breaks included) with spaces and truncates the message
  at a UTF-8 character boundary to `PROBE_MSG_MAX` (256) bytes, ending a
  cut message with `…`. Its JSON form is then at most 512 bytes. This
  covers messages from probes as well as core's own, such as serde's
  `InvalidParams` text;
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
    params: &Payload,
) -> Option<BoxFut<'static, Result<Payload, RpcError>>> {
    None
}
```

It borrows the params. If it took them by value, a session that returns
`None` would have consumed them, and the loop could not fall back to
`call`.

The AimX session returns `Some` for `probe.run`. The returned future does
**not** run the probe. It only waits for the result of an invocation that
the probe driver runs (below), so it holds no permit and no probe
resources. The connection loop keeps it in a `FuturesUnordered` and encodes
and sends the result when it resolves. Meanwhile the loop keeps reading
frames, answering other calls and forwarding events. Other methods stay
inline. A connection has at most one probe in flight; a second `probe.run`
on the same connection gets `probe_busy`. The connection's slot is free as
soon as the waiting future resolves, before its reply is written, so a
stalled writer does not keep the connection busy.

**The probe driver runs outside the connection loop.** The connection loop
cannot enforce a deadline on its own. It awaits each outbound `send`, and
the transports' `send` awaits `write_all` and `flush`. When a client stops
reading, for example during a busy subscription, a send blocks, and nothing
else the loop owns is polled. A probe future driven there would miss its
deadline and keep its permits and its connection or tap subscription for as long as
the writer is stalled. Polling it from one more arm of the same loop does
not help.

So probes run in one **probe driver** future per instance. `build()` adds
it to the runner's futures when the registry is not empty, as it does for
connectors, so it needs no task spawning and works on Tokio and Embassy
alike. The driver:

- receives invocations over a bounded channel. Its capacity is
  `max_concurrent_probes`, and an invocation is submitted only after its
  permits are taken, so submission never waits;
- owns each invocation's probe future, its `ProbeCtx`, its deadline timer
  and its concurrency permits, all in its own `FuturesUnordered`;
- enforces §5.2 on the instance clock: at the deadline it drops the probe
  future, and the permits go with it;
- hands the outcome (the serialized reply or a `ProbeError`) to a oneshot
  channel. Sending on a oneshot never waits, so the driver never depends
  on any connection's writer.

The driver awaits nothing but the invocations it owns and their timers. A
stalled, slow or closed connection therefore delays only the delivery of
its own reply. (A probe that blocks inside a poll can still delay the
driver; §5.2 states the contract.) The reply is then held in the oneshot until the connection
loop sends it or drops it, and it is at most the response budget (§5.3).

When the connection closes or a new handshake starts a new session, the
waiting future is dropped with the session's other futures. The driver sees
that the oneshot's receiver has gone and drops the invocation at once, as
at a deadline.

The subscription pumps share the loop's blocking on `send`, but they hold
no permits, and the bounded event funnel already drops events for a slow
client. Changing that is not part of this design.

With this, a long probe no longer holds a serial link either. The CLI still
prints a "waiting up to N s" line, because its own call waits for the
reply.

**Cancellation.** A caller that no longer wants a result should not have to
wait for the deadline, and the probe should not keep its permits until
then. Because the connection stays live, `probe.cancel` is an ordinary
inline call on the same connection:

- The session tells the waiting future of its `probe.run` to stop. That
  future drops its oneshot receiver and resolves with `Cancelled`. The
  driver sees that the receiver has gone and drops the invocation at once,
  exactly as at a deadline or a closed connection: the permits are released
  and the probe's `Drop` cleanup runs (§5.2). The probe gets no signal and
  no grace period.
- The pending `probe.run` is thus answered with `probe_cancelled`, and
  `probe.cancel` itself with `{ "cancelled": true }`. The reply to the
  cancel is sent first, then the `probe_cancelled`. The connection can
  start another probe as soon as the cancel is handled.
- With no probe in flight the reply is `{ "cancelled": false }` and nothing
  else happens. This covers the race where the probe finishes while the
  cancel is on its way; the caller then receives the probe's normal reply.

`aimdb-client` sends the cancel for the caller. `probe::run` returns a
future, and dropping that future before it resolves makes the client engine
send `probe.cancel` on the connection. `ClientHandle` gets a
fire-and-forget request for this, because a drop cannot await a call. So a
caller cancels by dropping the
call, which is how Ctrl-C in the CLI and an abandoned MCP tool call arrive
(§4.2, §4.3). A caller whose connection closes needs no cancel; the close
already drops the invocation.

### 4.2 CLI

```
aimdb probe list [--format text|json]
aimdb probe describe <name>                      # description and both schemas
aimdb probe run <name> [--params <json>]          # default {}
                       [--timeout <secs>] [--format json|pretty]
```

`probe list` follows `next` until the list ends. `probe describe` prints
one `probe.describe` reply. `--params` takes the params object as JSON and
sends it unchanged; the CLI does not read the schema to build or convert
parameters. serde checks the request on the instance (§5.1), and an error
names the field that is wrong, missing or unknown. The output is the `output` JSON (pretty printed on a
TTY). Exit codes: `0` success; `4` (the existing timeout
code) for `probe_timeout`; `5` for any other probe error. The kind and
message go to stderr. Codes 1–4 keep their meaning.

Ctrl-C during `probe run` cancels the probe: the CLI drops the call, the
client sends `probe.cancel` (§4.1), and the CLI exits once the cancel is
answered or after a short wait.

### 4.3 MCP

- `probe_list` returns the whole list, with every page fetched, and
  without schemas.
- `probe_describe` takes `{ name }` and returns the `probe.describe`
  reply: the description and both schemas.
- `probe_run` takes `{ name, params, timeout_s }` and returns the output
  JSON.

The MCP server keeps one connection per endpoint and makes one call on it
at a time. That is intended: a `probe_run` occupies the endpoint until it
returns, and other tool calls on that endpoint wait for it. Probes that an
agent runs should therefore be short, and `timeout_s` defaults to 30. When
the MCP host cancels or abandons the tool call, the server drops the call,
which sends `probe.cancel` (§4.1), so the endpoint is free again without
waiting for the deadline.

All three are read-only tools and available without `--allow-writes`. The tool
description tells an agent to check what a result says about its scope and
window before drawing conclusions from an absence (§6.1).

## 5. What AimDB is responsible for

| Responsibility | How |
|---|---|
| Registration | Unique names, description, schemas, limits (§3). |
| Invocation | `probe.list` / `probe.describe` / `probe.run` / `probe.cancel`, CLI, MCP (§4). Every discovery reply within the smallest frame (§4.1). |
| Concurrency | Per probe: at most `concurrency` invocations at a time, otherwise `Busy`. Per instance: at most `max_concurrent_probes` (default 2), otherwise `Busy`. Per connection: at most one in flight (§4.1), otherwise `Busy`. No queueing; the caller retries. |
| Deadlines | Enforced by the probe driver, independent of any connection, for probes that yield and do not block (§4.1, §5.2). |
| Cancellation | `probe.cancel`, sent by the client when the caller drops the call. The driver drops the invocation as at a deadline (§4.1, §5.2). |
| Error transport | `ProbeError` → AimX error reply `probe_<kind>` with `msg` (§4.1) → CLI exit code `4` or `5` and stderr, or an MCP tool error. |
| Result serialization | `serde_json`, then a final size check against the budget (§5.3). |

### 5.1 Parameters

Core deserializes `params` into `Probe::Params` before calling the probe. A
failure is `InvalidParams` with serde's message, and the probe never runs.
Core does not validate against the schema, and neither does the CLI;
deserialization is the check.

serde ignores unknown fields unless told otherwise. A misspelled optional
field would then be replaced by its default without a word, and a
misspelled required field would be reported as the missing one, not as the
typo. So a `Params` type should carry `#[serde(deny_unknown_fields)]`; the
error then names the unknown field. The ready-made probes do this, and the
`Probe` and `probe_fn` documentation says so. Core cannot enforce it.

### 5.2 Deadlines and cancellation

- The effective deadline is `min(timeout_ms, max_duration)`. When the
  caller gives no timeout, it is `max_duration`.
- `ctx.deadline()` is computed on the instance clock when the call is
  accepted. The driver enforces it with the runtime's sleep, which is
  relative and starts when the driver first polls the invocation. The two
  differ by the time until that first poll.
- The probe may return until the deadline. At the deadline core drops the
  future and replies `Timeout`. The probe itself gets no cancellation
  signal and no grace period in v1.
- A `probe.cancel` from the caller drops the invocation in the same way
  and replies `Cancelled` (§4.1).
- A closed connection, or a new handshake on it, also drops the
  invocation (§4.1).
- The probe driver enforces all of these (§4.1), independent of the connection
  that started the invocation. A stalled or slow connection does not delay
  the drop.

**The guarantee depends on executor progress.** Core can drop a probe
future only between its polls, and only when the executor polls the
driver. Deadline enforcement therefore holds only if every probe meets
this contract:

- **Yield, do not block.** A probe waits on I/O and timers through
  `.await` (on the instance's runtime, via `ctx.time()` and the
  connector's async I/O). It must not block inside a poll: no blocking
  socket or bus calls, no `std::thread::sleep`, no busy-waiting.
- **Keep synchronous work short.** Work between two `.await` points, such
  as building or trimming the final report, must stay small and bounded by
  the probe's own caps. Lengthy computation is split with yields or does
  not belong in a probe.

Within that contract, an invocation is dropped and its permits are
released at its deadline, plus the time until the executor next polls the
driver. A probe that breaks it delays its own drop by as long as it blocks.
It also stalls the rest of the instance, on every runtime: the runner
drives all of an instance's futures (connectors, the AimX server, the probe
driver) from one task, on Tokio as well as on Embassy. This is the same
rule as for any other code in an async application, and keeping it is the
probe author's responsibility. Core cannot detect or preempt a blocking
probe, so the contract is part of the `Probe` and `probe_fn` documentation.
The ready-made probes meet it.

A probe that observes for a while plans its window so that it returns in
time. The discovery probes observe for `min(duration_s, remaining - reserve)`,
where `ctx.remaining()` is read at the start and the reserve covers
closing the connection and building the report. A caller that asks for a
window longer than its timeout gets a shorter window, and `coverage` says
so, rather than a `Timeout`.

**Cleanup belongs to the probe implementation.** Core only promises the
drop. A probe holds its resources in values whose
`Drop` releases them completely, without awaiting and without anything that
can fail:

- A dedicated connection is owned by the probe's future. Dropping it closes
  the socket; the broker or gateway ends the session on its side. Nothing of
  the running instance is involved.
- A subscription to a tap on a running session (the KNX tunnel, §6.3) is
  owned by the probe's future. Dropping it removes the subscription and
  frees its queue. This is a plain store with nothing to undo on the
  session, and the session never waits for a subscriber.

The first release has no probe that changes a running session. Changing
live subscriptions needs a cleanup mechanism that this pattern cannot
provide; §9 lists what it requires. A cooperative cancellation signal with
a grace period can be added later, if a concrete resource needs
asynchronous cleanup (§10).

### 5.3 Limits: response size versus memory inside the probe

These are two different limits with two different owners.

**Response size: the budget comes from core, and the probe stays within it.**

Core computes the budget once per invocation and exposes it as
`ctx.response_budget()`:

```
budget = min(probe max_response_bytes,      // registration, optional
             instance max_response_bytes,   // new: 256 KB std, 8 KB embedded
             connection reply limit)        // transport and client, below
         - envelope reserve
```

`max_response_bytes` is a new instance setting; nothing bounds reply size
today apart from the transports' frame caps. It and
`max_concurrent_probes` are set on the builder
(`AimDbBuilder::max_response_bytes`, `AimDbBuilder::max_concurrent_probes`).

- **Connection reply limit:** the largest reply the client accepts,
  counted in **payload bytes**: the bytes of the reply frame before the
  transport's framing is applied. Counting payload bytes on both sides
  means the server only takes the smaller of two numbers and needs no
  per-transport overhead rule for a number that came from the client.
  - *What the transport states.* A frame cap is a receive limit, and the
    server cannot see the client's, so each transport states the largest
    payload its own default cap lets through and relies on the client
    using the same default. There is no path for this today: `Framer` has
    no size accessor and the AimX dispatch ignores the peer. So `Framer`
    gains `max_payload() -> Option<usize>`, the connection copies it into
    a new `PeerInfo::max_reply_payload` field, and `authenticate` passes
    it to the session through `SessionCtx`.
  - *What the client states.* AimX 4.1 adds an optional `max_frame` field
    to the `hello` params: the largest payload the client accepts, taken
    from its own transport's `max_payload()`. The server uses the smaller
    of the two for the session, and a new handshake sets it again. Without
    the field (a 4.0 client, or a client that does not state one) the
    transport's number stands alone; a client with a smaller cap that does
    not state it gets the frame dropped.
  - The discovery replies (§4.1) are sized for the smallest default,
    serial's, and do not shrink further: a client that states less than
    that may not receive them.

  The transports' numbers:
  - Serial: 8 159 bytes with the default `DEFAULT_MAX_FRAME` (8 KB). The
    receiving `FrameAccumulator` counts **COBS-encoded** bytes before the
    delimiter, not the decoded payload, and COBS adds about one byte per
    254. It drops a run only once it has buffered more than the cap
    without seeing a delimiter, so a frame whose encoded length is within
    the cap always arrives, however the reads are chunked. A longer frame
    can still arrive when it comes in one read, but nothing guarantees it.
    `max_payload()` is therefore the largest payload whose encoding fits
    the cap (`cobs::max_encoding_length`); the delimiter is not counted.
    Feeding the accumulator one byte at a time confirms 8 159 (§11).
  - TCP: the length framer's `DEFAULT_MAX_FRAME` (64 KB) unless
    configured. The length prefix is outside the counted payload.
  - UDS: none.
  - WebSocket: none set by the connector; each reply is one text message,
    and the WebSocket library's own message limit is far above any
    `max_response_bytes`. A browser client states no `max_frame`.

  The existing `instance_info_bounds` serial test measures replies the
  same way: envelope with the longest request id, COBS-encoded.
- **Envelope reserve:** the bytes of the reply frame around `output`:
  `{"t":"reply","id":…,"ok":{"name":"…","started_ns":…,"elapsed_ms":…,"output":`
  and the closing braces. Numbers are counted at their maximum width, and
  the name at its actual length. The outer part, `{"t":"reply","id":…,"ok":`
  and its closing brace, is 45 bytes with the longest request id. `output`
  is spliced into the frame
  verbatim (`RawValue`, as record values are today), so its bytes count
  one for one.

A successful observation must not be lost at the end, so the ready-made
probes **trim their final report to fit**. Core provides only
`ctx.response_budget()` and its final size check; how a probe fits its
report is up to the probe:

- During the window, each probe bounds its collection with its entry cap
  (`max_topics`, `max_addresses`). An entry beyond the cap is not stored;
  the probe counts what it could not store.
- At the end, it serializes its report and compares the length with the
  budget. When the report is too large, it drops entries, in its own order
  (for example, the topics with the fewest messages first), until the
  report fits, and marks the result incomplete: `complete: false`,
  `limits: ["response budget (7 900 bytes) reached: 58 of 272 topics
  listed; 214 messages on unlisted topics"]`.
- The `coverage` object's strings are cut to fixed maximum lengths, so it
  always fits next to at least an empty list.

Both probes do this the same way, in one pass: order the entries, add
them while the running serialized size fits the budget, and report what
was left out. Core therefore provides this loop as a small helper
(`probe::fit_to_budget`), which takes the entries in the probe's order and
the budget and returns the entries kept and the count dropped. Ordering,
the wording of the limit and the `coverage` object stay with the probe.
The helper sizes each entry with `serde_json`, so it lives in core, which
already depends on it.

On an embedded target with the 8 KB default, this lists roughly 30–50
topics or group addresses (about 160 bytes per typical entry), depending on
topic length, and says plainly when there were more. That is a valid,
useful answer, rather than a `ResponseTooLarge` after a full window.

`ResponseTooLarge` remains as the safety net for probes that ignore the
budget. The error carries the budget, so the developer can see what to fit
into. Custom probes can use `ctx.response_budget()` the same way. How core
serializes depends on the target:

- **`std`:** core serializes into a writer that stops at the limit
  (`serde_json::to_writer` over a bounded `io::Write`), so an oversized
  result is never held in full. `to_writer` and `Serializer` exist only
  with `serde_json`'s own `std` feature, which the workspace does not
  enable on any target today. Core's `std` feature therefore enables
  `serde_json/std`.
- **`no_std`:** the workspace builds `serde_json` with `alloc` only, which
  has `to_vec` but no streaming `Serializer` or `to_writer`. Core serializes
  with `to_vec`, checks the length and drops the buffer if it is over the
  budget. The result is then held in full once, and the reply frame is
  built from it. Within budget that is at most the instance's
  `max_response_bytes` (8 KB by default). A result far over budget is
  allocated before it is rejected, which is one more reason the ready-made
  probes trim to the budget themselves. A streaming bounded serializer
  for `no_std` is an open question (§10).

**Memory inside the probe: owned by the probe.** Core cannot see or limit
what a probe allocates while it observes. Every probe bounds its own state,
for example `max_topics` or `max_addresses`, and says in its output when it
hit a bound (§6.1). The ready-made probes take these bounds as parameters,
capped at registration, so a caller cannot ask for unbounded memory. Their
embedded defaults are chosen to match the response budget, so little memory
is spent on entries that would be trimmed anyway.

## 6. Ready-made discovery probes

These are the first probes. They answer what configured links cannot: which
signals exist that the instance does not consume yet.

They are reference implementations that ship in the connector crates, not
part of the probe mechanism. Core knows nothing about them, and an
application can register them, replace them with its own, or leave them
out. How each one collects, bounds and trims its data is that probe's own
business, as it is for any developer probe.

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
| `duration_s` | 10 | Observation window, shortened to end before the deadline (§5.2). |
| `max_topics` | 200 on `std`, 48 on embedded (cap set at registration) | Memory bound. The final report may list fewer to fit the response budget (§5.3). |
| `unlinked_only` | false | List only topics that no configured inbound link matches. |

**Output:** `{ coverage, topics: [ { topic, messages, retained, qos_seen,
bytes: {min, max, last}, first_seen_ms, last_seen_ms, linked } ] }`.
`linked` says whether a configured inbound link matches the topic. That is
the bridge from discovery to configuration.

**How it observes:** the probe opens its own client connection (own
client ID, clean session, no will message), subscribes to `filter` at
QoS 0 for the window, then disconnects. It is independent of the running
session and can see everything its credentials allow. Nothing it receives
reaches a record. It needs a second socket while it runs, and
`coverage.connection` is `"dedicated"`.

**Registration.** The application registers
`MqttDiscover::dedicated(dialer, &broker_url)` on the builder (§3). The
probe needs nothing from the MQTT connector: it has its own broker address
and credentials, and it computes `linked` from
`ctx.db().inbound_links("mqtt")` (§2). `dialer` is the probe's own
transport, a `StreamDialer` like the one the embedded session uses
(`TokioNet::tcp()`, `EmbassyNet::tcp(..)`), so one probe implementation
serves Tokio and Embassy.

**Protocol.** The probe has its own minimal MQTT 3.1.1 client: CONNECT,
SUBSCRIBE, DISCONNECT, and a reader for PUBLISH. It does not reuse the
session's client code, which is built around the session's buffers and
reconnect loop. Three points follow from that:

- It reads the stream byte-wise and counts and discards payloads, so it
  needs **no packet buffer**, whatever the message size.
- It sends no keep-alive pings. It asks the broker for a keep-alive longer
  than its window.
- Retained messages can arrive before the SUBACK, so the reader accepts
  both in any order. A refused subscription is `Refused`.

On Embassy the dedicated connection borrows nothing from the running
session, and its memory is provided at registration:

- **Socket buffers:** the dialer's own receive and send buffers. The spike
  ran with 1 024 and 512 bytes (§11.6).
- **Socket:** one more TCP socket while the probe runs. The application
  sizes the `embassy-net` `StackResources<N>` (the demos use 4 or 5) and
  raises `N` by one.
- **Release:** the Embassy dialer returns its socket to its slot when the
  stream is dropped, so a probe dropped at a deadline or by a cancel
  leaves the dialer ready for the next run.
- **`mqtts`:** the probe needs its own TLS resources. The session's
  `TlsOptions` hold `&'static mut` read and write buffers and a
  `&'static mut` RNG, handed over once and not shareable, so the probe
  takes a second set. Only one invocation can use them at a time, which
  matches its `concurrency(1)`. This has not been tried (§10).

The Embassy demo (§8) documents the RAM this costs.

Observing the running MQTT session, passively or with a temporary
subscription, is not part of the first release (§9).

### 6.3 `KnxDiscover`

**Params:** `ga_prefix` (for example `1/2/`, default all), `duration_s`
(default 30), `max_addresses` (500 on `std`, 48 on embedded; the final
report may list fewer to fit the response budget, §5.3), `unlinked_only`.

**Output:** `{ coverage, group_addresses: [ { ga, telegrams: {write,
response, read}, sources: [individual addresses, up to 4], data_len: {min,
max}, first_seen_ms, last_seen_ms, linked } ] }`.

**How it observes:**

- `KnxDiscover::new(tap)` observes the **existing tunnel** through a tap
  that the connector hands out (`KnxConnector::tap()`, §3). The tunnel
  already receives every group telegram the gateway forwards, so this
  needs no extra tunnel slot and sends nothing on the bus. This is the
  first release's KNX probe. `KnxDiscover` ships in the connector crate
  as a reference implementation with one tested report shape; an
  application registers it with one line, or builds its own probe on the
  same tap, for example one that watches a single group address. The
  connector itself carries no caps, no trimming and no report format.
  `linked` is computed from `ctx.db().inbound_links("knx")` (§2).

  **Where the tap is fed.** Today `parse_telegram` in `tunnel.rs`
  reduces each cEMI frame to a group address and a payload before anything
  else sees it. It keeps only `GroupValue_Write`, so reads and responses
  are dropped. It discards the source individual address. A 6-bit value
  becomes a one-byte payload. The probe's `telegrams: {write, response,
  read}`, `sources` and `data_len` cannot be computed from what comes out
  of it, so the tap cannot be fed at `TunnelIo::forward` or at the
  `InboundDispatch` either. It sits in the tunnel engine, where the
  `TunnellingRequest` is handled. For every `L_Data` frame with a group
  destination, before the write-only filter, it records the destination,
  the APCI kind (read, response, write), the source address and the data
  length on the wire (`0` for a 6-bit value). knx-pico's `LData` already
  exposes `source` and `apci`, so nothing new is parsed. The path that
  delivers writes to records is unchanged.

  **The tap's contract.** The tap is the one part an application cannot
  add from outside, so it is connector API:

  - `tap.subscribe(capacity)` returns a subscription with its own bounded
    queue of telegram summaries (destination, APCI kind, source, data
    length), not raw frames. The probe reads it with `next().await`.
  - The tunnel engine feeds every subscription for every group telegram,
    in arrival order, with a push that never waits. It runs no user code.
    A slow or stalled probe therefore cannot delay the acknowledgements
    to the gateway.
  - When a queue is full the telegram is not queued for that subscription
    and its `dropped()` count goes up. A probe reports that count, so a
    burst that outran it is visible in the result.
  - With no subscription the engine does nothing extra.
  - `tap.connected()` gives the tunnel's state, which the connection task
    keeps current. Without it, an empty result from a tunnel that is down
    looks like a quiet bus.
  - Dropping the subscription removes it (§5.2). A tap taken from a
    connector that is not running yet reports not connected.

  `KnxDiscover` folds each summary into its per-address table as it
  reads it. It returns `Unavailable` when the tunnel is not connected at
  the start, marks the result incomplete when the tunnel is not connected
  at the end or when telegrams were dropped, and lists group addresses in
  address order, not as strings.
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

- **Only registered code runs.** Probes are registered on the builder, and
  the registry does not change after `build()` (§3).
- **Read-only from AimDB's point of view.** A probe has no record writers in
  `ProbeCtx`: `ctx.db()` is the read-only `ProbeDb` view, not the `AimDb`
  handle (§2). A probe that captures an `AimDb` handle of its own at
  registration can still write; that is the developer's choice. What it does in the environment is the developer's code; the
  ready-made probes only listen. The MQTT probe sends only the frames of
  its own connection (connect, subscribe, keep-alive, disconnect). The KNX
  probe sends nothing.
- **The developer controls exposure.** Registration sets the allowed filter
  or prefix. Registration is per instance, not per transport: a registered
  probe can be listed, run and cancelled by every client of every AimX
  endpoint the instance serves (UDS, TCP, serial, WebSocket), including
  read-only clients. A probe whose results are sensitive should not be
  registered on an instance that untrusted clients can reach.
- **Bounded:** deadline, concurrency, response size, metadata size (§3, §5).

## 8. First release

1. **Core** (`aimdb-core/src/probe/`): `Probe`, `probe_fn`, `ProbeCtx`
   with `response_budget()` and the read-only `ProbeDb` view, `ProbeError`, `ProbeSpec`, the registry
   (filled by `build()`, immutable afterwards), `ProbeDb::inbound_links`,
   type erasure, the
   final size check (streaming on `std`, which enables `serde_json/std`;
   `to_vec` and a length check on `no_std`), the instance's
   `max_response_bytes` and `max_concurrent_probes` builder settings, the registration limits
   (`PROBE_NAME_MAX`, `PROBE_DESCRIPTION_MAX`, `PROBE_DESCRIBE_MAX`),
   message bounding (`PROBE_MSG_MAX`), the `fit_to_budget` helper
   (§5.3), the probe driver with deadline
   drop, and `probe.list` (paged) / `probe.describe` / `probe.run` /
   `probe.cancel`.
2. **AimX 4.1 wire and session** (`aimdb-core/src/session/`): the optional
   `msg` field on error frames, `RpcError::Probe { kind, message }` and
   its `probe_<kind>` codes, `Session::call_deferred` with replies that
   the connection loop waits for while the probe driver runs the
   invocation, at most one probe in flight per connection (§4.1),
   `probe.cancel`, the optional `max_frame` field in `hello`, and each
   transport's reply limit: `Framer::max_payload()`,
   `PeerInfo::max_reply_payload` and its way into the session (§5.3).
3. **Client, CLI, MCP:** `aimdb-client::probe::{list, describe, run}`
   with the version check from the handshake, page assembly, `max_frame`
   in `hello`, and `probe.cancel` sent when a `run` call is dropped,
   `aimdb probe` with JSON `--params`, and the `probe_list` /
   `probe_describe` / `probe_run` tools.
4. **`MqttDiscover::dedicated(dialer, url)`** on Tokio and Embassy: a
   dedicated connection over the given `StreamDialer`, its own minimal
   MQTT client, `linked` from `ProbeDb::inbound_links` (§6.2), and
   trimming to the response budget with core's helper (§5.3).
5. **`KnxConnector::tap()`** and **`KnxDiscover::new(tap)`**: a tap on the
   tunnel engine, fed before the write-only filter, with a bounded queue
   per subscription, a drop count and the tunnel's connected state
   (§6.3); `KnxDiscover` as the reference probe on it, trimming with
   core's helper.
6. **Demos:** a `probe_fn` example (`board.info`) in an Embassy demo, with
   the RAM cost of a dedicated MQTT probe noted.

**Testing:**

The deadline tests need a runtime whose sleep follows a clock the test
controls. Core's existing test runtimes (`NoopRuntimeOps`,
`ManualClockRuntimeOps`) complete every sleep at once, so they cannot show
that a probe is dropped *at* its deadline. The first release adds a test
`RuntimeOps` over Tokio's paused clock (Tokio's `test-util` feature as a
dev-dependency), which the spike used (§11).

- Core: params errors (a wrong type, a missing field, and an unknown field
  on a `deny_unknown_fields` type, each named in the message), `Busy`, the deadline (a probe that never returns is
  dropped at the deadline and the reply is `Timeout`), the budget
  computation per transport (serial 8 KB measured COBS-encoded, TCP
  `max_frame`, UDS unbounded), `ResponseTooLarge` (without full buffering
  on `std`), `inbound_links` per scheme, and registration limits
  (an over-long name or description, a control character in the
  description, invalid schema JSON, or a describe reply over 6 KB fails
  `build()` with the probe and its size named).
- Discovery bounds, in the style of `instance_info_bounds`: with the
  largest registration the limits allow (many probes with 64-byte names,
  256-byte descriptions full of `"` and `\`, and describe replies at
  exactly 6 KB), every `probe.list` page, every `probe.describe` reply and
  an error reply with a long, multi-line, multi-byte message fit the serial
  frame after COBS encoding. Paging through `next` returns every probe
  once.
- Session: while a probe runs on a connection, the same connection answers
  `record.get` and keeps delivering subscription events without gaps; a
  second `probe.run` on it gets `probe_busy`; a new handshake or a closed
  connection cancels the probe.
- Cancel: `probe.cancel` during a probe that never returns drops its
  future (a drop guard records it), frees its permits, answers the pending
  `probe.run` with `probe_cancelled` and the cancel with `cancelled: true`;
  the same connection can then run a probe again. With no probe in flight
  the reply is `cancelled: false`. Dropping the client's `run` future sends
  the cancel.
- Reply limit: a client that states `max_frame` in `hello` gets a budget
  computed from the smaller of that and the transport default; a client
  that states none gets the transport default.
- Stalled writer: a test connection whose `send` stops completing, after
  `probe.run` has been received, while a subscription keeps producing
  events. Two cases run on the paused test clock:
  - a probe that never returns. Its future is dropped (a drop guard
    records when), and its per-probe and instance permits are free again,
    no later than the deadline. A
    `probe.run` of the same probe on a second connection then succeeds
    instead of getting `probe_busy`;
  - a probe that finishes before its deadline. Its permits are released
    when it finishes, not when its reply is sent.

  Unblocking the writer afterwards delivers the waiting reply (or nothing,
  if the connection was closed in the meantime) without disturbing the
  driver.
- Wire compatibility: a 4.1 error frame with `msg` decodes as
  `RpcError::Internal` on a 4.0 client; the 4.1 client reports "probes need
  AimX 4.1" against a 4.0 server without sending `probe.run`.
- MQTT: dedicated discovery against a local broker in CI. With many long
  topics and an 8 KB budget, it returns a trimmed, valid result marked
  incomplete that fits the budget; a window longer than the timeout is
  shortened and still returns before the deadline; nothing reaches a
  record.
- KNX: the tap against the fake gateway used by the
  current client tests, fed group reads, responses, writes and 6-bit
  values from several sources; nothing is sent; writes still reach their
  records; many addresses with four sources each are trimmed to fit an
  8 KB budget; dropping the probe mid-window leaves the tunnel unchanged;
  a subscription that is not read fills its queue, counts the drops, and
  the tunnel's ACKs go out on time; a second probe on the same tap sees
  the same telegrams.
- End to end: the CLI `probe run` with `--params` JSON, its exit codes
  (`4` on timeout, `5` on other probe errors, including a params error that
  names the field) and JSON output, including over a serial link at the
  8 KB frame limit. Ctrl-C during `probe run` cancels the probe on the
  instance.

## 9. Later: probes that use the running MQTT session

Not in the first release. Observing the running session **passively**
(topics it already receives) needs only a tap, like KNX, and can
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
4. **Deferred to later versions:**
   - **Cooperative cancellation:** a `cancelled()` signal to the probe
     with a grace period, if a concrete resource needs asynchronous cleanup
     that `Drop` cannot do. `probe.cancel`, a deadline and a closed
     connection would all raise it.
   - **Large schemas:** schemas whose describe reply exceeds 6 KB, for
     example via chunked retrieval like `instance.post_link`, if real
     probes need them.
   - **Schema-driven CLI parameters:** `--param k=v` typed by the params
     schema, if JSON `--params` proves too awkward.
5. **Bounded serialization on `no_std`:** `serde_json` with `alloc` has no
   streaming serializer, so v1 checks the length after `to_vec` (§5.3).
   Options for a true bound: `serde-json-core` into a fixed buffer, or a
   size-counting pass before encoding.
6. **Generating configuration from results:** turning a discovered,
   `linked: false` topic into a `link_from` declaration and a data contract
   draft is a natural next step for the CLI and the architecture agent.
7. **`mqtts` for the MQTT probe on Embassy:** a second set of TLS buffers
   and a second RNG handle (§6.2). Not tried in either spike; it may be
   left out of the first release.
8. **Repeated KNX telegrams:** on a live bus many group addresses showed
   exactly two writes in a window (§11.6). The tunnel engine does not
   filter repeated sequence numbers, so a gateway retransmission would be
   counted, and delivered to a record, twice. Whether that is what
   happened is not known.

## 11. Spike findings

A throwaway prototype of this design was built on the current code base
(branch `spike/059-probe`, not for merging) to check the claims above
against the real session engine. It covered the core mechanism, the AimX
wire and session changes, and the client call. It did not cover the
discovery probes.

### 11.1 What was built

- `aimdb-core`: `Probe`, `probe_fn`, `ProbeCtx`, `ProbeError`, `ProbeSpec`,
  the registry with its registration limits, type erasure, bounded
  serialization, the probe driver, `probe.list` / `probe.describe` /
  `probe.run` / `probe.cancel`, `Session::call_deferred` and the change to
  the connection loop, `msg` on error frames, `RpcError::Probe`, and
  `max_frame` in `hello`. About 650 lines for the probe module and 250
  lines of changes to existing files.
- `aimdb-client`: `probe_run` with the version check and cancel on drop.
- Tests: ten against the real `run_session`, `AimxCodec` and `AimxDispatch`
  over an in-memory connection on a paused clock; one over the production
  UDS server with the Tokio adapter in real time; one against the serial
  `FrameAccumulator`.

### 11.2 Confirmed

| Claim | Result |
|---|---|
| The trait, `probe_fn` and the erasure compile; the erased future is `Send + 'static` (§2, §3) | Yes, for `probe_fn` and for a direct `impl Probe` that borrows `&self` and `&ctx` across an `.await`. Core also compiles for `thumbv7em-none-eabihf` with `alloc`, `remote` and `connector-session`. |
| The connection stays live while a probe runs (§4.1) | Yes. `probe.list` and `record.list` are answered during a 60 s probe. The loop change is about 25 lines, and the 20 existing session-engine tests still pass. |
| Three concurrency limits, each `probe_busy` (§5) | Yes, for the per-connection and per-probe limits, each with its own message. The instance limit uses the same code path as the per-probe limit and had no test of its own. |
| The driver drops a probe at its deadline and frees its permits (§5.2) | Yes. A probe that never returns is dropped after exactly `timeout_ms`, or after `max_duration` when the caller asks for more. |
| A closed connection and a new handshake drop the probe (§4.1) | Yes, both. No reply for the old request reaches the new session. |
| A stalled writer does not delay the deadline (§4.1) | Yes. With the connection loop blocked in `send`, the probe is dropped on time and a second connection can run the same probe. Unblocking delivers the waiting `probe_timeout`. |
| `probe.cancel` (§4.1) | Yes. The probe future is dropped, the permits are free, and the connection can run a probe at once. |
| Dropping the client call cancels the probe (§4.1) | Yes, over UDS in real time. A probe with a 120 s limit is gone within 200 ms of the dropped call. |
| A 4.0 client reads a 4.1 error frame as `Internal` (§4.1) | Yes. Checked against the unmodified 4.0 decoder with a `msg` that contains escaped quotes, a line break and a `\u` escape. |
| Twelve worst-case `probe.list` entries fit a serial frame (§4.1) | Yes: a page of twelve is 8 159 bytes, which is exactly the largest payload the serial accumulator always delivers (8 159, measured byte by byte). There is no spare byte. |
| An error reply is at most 604 bytes (§4.1) | Yes: 578 bytes with a message of quotes cut at 256 bytes. |
| Registration limits and sealing (§3) | Yes: name, description length, control character, invalid schema JSON, describe reply over 6 KB and a duplicate name each fail `build()`; registration after `build()` is refused. |
| `max_frame` in `hello` lowers the response budget (§5.3) | Yes. |
| knx-pico exposes the telegram's source and APCI (§6.3) | Yes: `LDataFrame` has public `source` and `apci` fields. |

### 11.3 Where the design had to change

The sections named below have been updated to match.

1. **Streaming serialization on `std` needs `serde_json`'s `std` feature
   (§5.3).** The workspace builds `serde_json` with `alloc` only, on every
   target, so `to_writer` and `Serializer` are missing on `std` as well.
   Core's `std` feature enables `serde_json/std`. The spike did this
   and the `no_std` check still passes.
2. **`ProbeCtx::db()` is not read-only (§2, §7).** The only handle core has
   is `AimDb`, which can `produce` and `set_record_from_json`. The
   statement "a probe has no record writers" needs a new read-only view
   type. §2 now defines `ProbeDb`.
3. **No path exists from a transport to the session for the reply limit
   (§5.3).** `Framer` has no size accessor and `AimxDispatch` ignores the
   peer. The spike added a field to `PeerInfo` that reaches the session
   through `SessionCtx`. Each transport must fill it; §5.3 now names this
   path.
4. **State both limits in decoded payload bytes (§5.3).** The text said
   `hello.max_frame` is counted "the way that transport counts them". Then
   the server needs every transport's overhead function to compare it with
   its own limit. Now the client states the largest payload it accepts,
   the transport states the same, and the server takes the smaller one.
5. **`call_deferred` should borrow the params (§4.1).** With
   `params: Payload` by value, a session that returns `None` has consumed
   them and the loop cannot fall back to `call`. The spike used `&Payload`.
6. **A misspelled parameter is not reported (§4.2, §5.1).** serde ignores
   unknown fields. A misspelled optional field is silently replaced by its
   default; a misspelled required field is reported as the *missing* one.
   "An error names the field" holds only for a wrong type. Params types
   should use `deny_unknown_fields`, and the ready-made probes must
   (§5.1).
7. **The existing test runtimes cannot test deadlines (§8).** Core's
   `NoopRuntimeOps` and `ManualClockRuntimeOps` complete every sleep at
   once, so "a manually advanced clock" does not exist yet. The spike used
   Tokio's paused clock behind a small `RuntimeOps` (this needs Tokio's
   `test-util` feature as a dev-dependency). The first release adds such a
   test runtime (§8).

### 11.4 Details the spike settled

Most of these are now stated in the sections they belong to.

- **Reply order after a cancel.** The reply to `probe.cancel` arrives
  first, then `probe_cancelled` for the run.
- **When the connection's slot is free.** As soon as the reply future
  resolves, before the reply is written. A stalled writer does not keep the
  connection `probe_busy`.
- **The deadline timer.** `RuntimeOps` offers only a relative sleep. The
  driver starts it when it first polls the invocation, while
  `ctx.deadline()` is computed when the call is accepted. The two differ by
  the time until the driver is polled.
- **The reply envelope** around a body is 45 bytes with the longest request
  id. A `probe.list` entry can gain no field without the serial page
  dropping to eleven entries.
- **Where the registry lives.** It fits in the instance's existing
  `Extensions`, with a `spin::Mutex` inside, so `AimDbInner` does not
  change.
- **Instance settings.** `max_concurrent_probes` and `max_response_bytes`
  had no builder method in this design; §5.3 now names them.
- **Build errors.** `DbError::InvalidConfiguration` carries `ConfigError`,
  which is keyed by record. Probe errors use an empty record key.
- **Cancel on drop needs a task.** `ClientHandle::call` is async, so the
  spike's drop guard spawned a task to send `probe.cancel`. §4.1 now
  gives `ClientHandle` a fire-and-forget request instead.
- **Version check.** The client has only a major-version comparison; the
  probe check needs the minor as well. Raising `PROTOCOL_VERSION` to `4.1`
  breaks one unit test that asserts the string.

### 11.5 Not covered

The first three items were covered by a second spike; see §11.6.

- `MqttDiscover` and `KnxDiscover`, including the Embassy buffers and the
  tunnel-engine observer. For KNX only the knx-pico fields were checked.
- Running on Embassy. Core was compiled for a `no_std` target, not run.
- Registration from a real connector's `build()`. The registry accepts
  specs until it is sealed, but no connector did so in the spike.
- The CLI and MCP commands, WebSocket, and a real serial link.
- `schemars` schemas (§10.1).

### 11.6 Second spike: the discovery probes, on a board

A second prototype added `mqtt.discover` and `knx.discover` and ran them on
a NUCLEO-H563ZI (Embassy), built from the two Embassy connector demos. The
host talked to the board over the ST-LINK serial port with `aimdb-client`.
The broker was the bench Mosquitto on the LAN; the KNX gateway was a real
KNX/IP interface on a live installation.

**Confirmed**

| Claim | Result |
|---|---|
| A probe runs on Embassy (§2, §5.2) | Yes. `probe_fn` and both discovery probes run on the board. Their futures are `Send` through the adapter's existing transport wrappers; the probes add no `unsafe`. |
| Liveness, busy, deadline and cancel on the board (§4.1, §5.2) | Yes, over serial. `record.list` is answered in 275 ms during a 4 s probe; a second `probe.run` gets `probe_busy`; a 1 500 ms deadline drops the probe future 1.50 s after it started, by the board's own log; dropping the client call frees a 60 s probe at once. |
| `MqttDiscover::dedicated` on Tokio and Embassy (§6.2) | Yes, with one implementation over core's `StreamDialer`: `TokioNet::tcp()` on the host, `EmbassyNet::tcp(..)` on the board. On the board it runs next to the connector's own session, which keeps publishing and receiving. |
| Dropping the probe releases the connection (§5.2) | Yes. The Embassy dialer already returns its socket to a slot when the stream is dropped. After a probe cancelled mid-window, the next run connects again. |
| Registration from a connector's `build()` (§3) | Yes. The KNX connector registers `knx.discover` through `db.register_probe` while it builds. |
| The KNX observer sits before the write-only filter (§6.3) | Yes. Reads, responses, writes, the source address and a data length of `0` for 6-bit values are reported; writes still reach their records; the probe sends nothing beyond the engine's ACKs. On the real bus a 90 s window listed 20 group addresses, two of them marked `linked`. |
| Trimming to the serial budget (§5.3) | Yes, over the real serial link: 37 of 124 topics in a 7 458-byte reply, and 38 of 200 group addresses with four sources each on the host. The estimate of 30–50 entries holds. |
| A window longer than the timeout is shortened (§5.2) | Yes; the reply arrives before the deadline and `coverage` says so. |
| Error kinds (§2) | A wrong password gives `Refused`, an unreachable broker `Unavailable`, a KNX tunnel that is not connected `Unavailable`, and a misspelled parameter `InvalidParams` naming the field. |

**Where the design had to change**

The sections named below have been updated to match.

1. **`linked` cannot be computed from the instance (§2, §6.2).** Nothing
   lists the configured inbound links: `RecordMetadata` carries no link
   topics. The KNX probe took them from its connector's own
   `InboundDispatch`. The standalone `MqttDiscover::dedicated` had to be
   given the filters by hand at registration. After review, core gains
   `ProbeDb::inbound_links` instead (see the end of this section).
2. **`KnxConnector::discover_probe()` cannot exist as written (§6.3).** The
   connector is moved into the builder before the tunnel exists. The spike
   used an opt-in on the connector, `KnxConnector::with_discover_probe()`,
   which registers the probe from `build()`. After review, the connector
   hands out a tap instead (see the end of this section).
3. **The observer needs the tunnel's state (§6.3).** With only a telegram
   callback, an empty result from a disconnected tunnel looks like a quiet
   bus. The spike's slot also carries a `connected` flag that the
   connection task keeps current; the probe refuses with `Unavailable`
   when it is not set and notes a tunnel lost during the window.
4. **The Embassy MQTT probe needs less memory than §6.2 says.** It reads
   the stream byte-wise and discards payloads, so it needs no 4 KB packet
   buffer. On the board it used a 1 024-byte receive and a 512-byte send
   socket buffer, one more `StackResources` slot, and its topic table.
5. **The probe has its own MQTT client (§6.2).** The spike
   wrote a minimal MQTT 3.1.1 client (CONNECT, SUBSCRIBE, DISCONNECT, about
   120 lines) instead of reusing the session's `mountain-mqtt` v5 code,
   which is built around the session's buffers. It sends no keep-alive
   pings; it asks for a keep-alive longer than the window. Retained
   messages can arrive before the SUBACK, so the window loop must accept
   both.
6. **A shared trimming helper is already justified (§5.3, §10.4).** Both
   probes ended up with the same loop: sort, add entries while the running
   size fits, report what was left out. Both connector crates also need
   `serde_json` to size an entry. Core now provides the helper.

**Open after this spike**

- **Duplicate KNX telegrams.** Many group addresses on the real bus showed
  exactly two writes in the window. The observer counts every tunnelling
  request, and the engine does not filter repeated sequence numbers. It was
  not determined whether the bus carries these telegrams twice or the
  gateway retransmits them (§10).
- **Ordering.** The spike sorted group addresses as strings (`1/0/12`
  before `1/0/6`); §6.3 now asks for address order.
- **Caps at registration** for `max_topics` and `max_addresses`, and the
  `allowed_filter`, were not built.

**Changed after review, not prototyped.** The spike registered
`knx.discover` from the connector's `build()` and fed a callback inside
the tunnel engine; it gave the MQTT probe its link topics by hand. A
review after the spike changed three things, and the sections above
describe the result:

- Probes are registered on the builder only. The KNX connector hands out
  a tap before `build()`; `db.register_probe` and the sealed registry are
  gone (§3). The spike's slot was already a handle that does not depend on
  the tunnel, so this is expected to work, but it was not run.
- The tap is a bounded queue per subscription with a drop count, not a
  callback, so user code never runs inside the tunnel engine (§6.3).
- `ProbeDb::inbound_links(scheme)` is new in core and replaces the link
  topics handed over by a connector (§2).

**Still not covered:** `mqtts` for the probe on Embassy (its own TLS
buffers and RNG), observing the running MQTT session, the CLI and MCP
commands, WebSocket, and `schemars` schemas.

