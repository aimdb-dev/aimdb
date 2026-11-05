# 059 — Probe: ask a running instance what its connectors and inputs see

**Status:** 📝 Proposed

**Scope:** The feature request "Probe: ask a running instance what its
connectors and inputs see, as readable lines". A read-only, time-bounded
`probe` that every connector implements and inputs may implement, invoked on
a running instance over AimX. It covers the `Probe` trait and line model in
`aimdb-core`, how a connector registers a probe from `build()` and how the
probe reaches the live session loop, the per-connector ring of recent typed
events (building on #365), discovery under an allow-list, AimX 4.1 (additive),
`aimdb probe` in the CLI, a read-only `probe` MCP tool, and the first
implementations (MQTT on Tokio and Embassy, KNX, the AimX servers, and the
demo inputs).

Supersedes the drafts for passive MQTT topic discovery, its MCP/CLI command,
and a separate `Discover` trait.

---

## 1. The rule

> A probe reports what a running connector or input sees. It may ask the
> live session a question that changes nothing. It never measures, never
> writes, never reconnects, and it always returns by its deadline.

> A probe line is `{ verdict, subject, text, facts }`. The verdict is what a
> tool acts on; the text is for a reader; the facts are for a check.

What a probe says:

- **Now:** the result of an active check against the live session (an MQTT
  `PINGREQ`, a KNX `CONNECTIONSTATE_REQUEST`) and the session's state and
  counters.
- **Recently:** the connector's last few operational events, with their age,
  in the same words as the log line for the same event.
- **Nearby, if allowed:** what exists under prefixes the application owner
  opened for discovery (MQTT topics, KNX group addresses), as shapes and
  counts, never values.

What a probe does **not** say:

- Payload values. Not in text, not in facts.
- What a pin, sensor or bus reads *now*. An input reports what its source
  task already observed.
- Anything about a `.source()` closure. Closures have no probe; an `Input`
  does.

## 2. Requirements

| # | Requirement |
|---|---|
| R1 | Every connector in this repository registers a probe. A connector that does not is reported by core as `info  <scheme>: no probe`. |
| R2 | A probe is read-only: no publish, no bus write, no output switched, no reconnect. The only session change allowed is a discovery subscription (R7). |
| R3 | One deadline per probe run (default 10 s, max 60 s). Core enforces it and reports every handler still running as `fail`. |
| R4 | Lines stream to the caller as they are produced. Memory per run is bounded on the device. |
| R5 | Lines never carry payload values. |
| R6 | Recent events come from the existing typed events (#365). The probe line and the log line for one event have the same wording, whether or not logging is enabled and whatever the log backend (`tracing`, `log`, `defmt`, none). |
| R7 | Discovery runs only under prefixes the application allowed at build. A request may narrow them, never widen them. Discovery never replaces or overlaps an application subscription. |
| R8 | Works over TCP, UDS and serial; additive to AimX (minor version). |
| R9 | Runs under AimX's `ReadOnly` policy; the MCP tool needs no `--allow-writes`. |
| R10 | Fits small MCUs: the event ring's size is a constant that can be set to 0; the active checks remain. |
| R11 | No process-wide state. Two connectors of one kind (`mqtt` and `mqtt-cloud`) have separate rings and probes (`tools/check-no-globals`). |

## 3. The line model

### 3.1 One line

```rust
pub enum Verdict { Ok, Info, Warn, Fail, Event }

pub struct ProbeLine {
    pub verdict: Verdict,
    /// The connector's scheme (`mqtt`, `mqtt-cloud`, `knx`), the record key
    /// of an input (`b1`), or `probe` for core's summary.
    pub subject: String,
    /// One line, no newlines, at most `LINE_TEXT_MAX` (160) bytes.
    pub text: String,
    /// At most `LINE_FACTS_MAX` (8) `key=value` pairs.
    pub facts: Vec<(&'static str, FactValue)>,
    /// Only for `Verdict::Event`: the level the event was reported at, and
    /// its age when the probe ran.
    pub event: Option<EventMeta>,
}

pub enum FactValue { U64(u64), I64(i64), Bool(bool), Str(String) }
pub struct EventMeta { pub level: Level, pub age_ms: u64 }
```

- `Event` is the `·` lines: they never count towards the summary.
- Fact keys are `&'static str`, so they are chosen in code and never come
  from a remote peer. Unit suffixes are a convention (`_ms`, `_s`,
  `_bytes`); there is no schema per connector.
- Text that is too long is cut at a UTF-8 boundary and ends with `…`. A
  newline in text is replaced by a space. Core does this, not each probe.

### 3.2 The sink

```rust
pub struct ProbeOut<'a> { /* sink, deadline, budget, subject */ }

impl ProbeOut<'_> {
    pub fn ok(&mut self, text: impl Display) -> LineBuilder<'_>;
    pub fn info(&mut self, text: impl Display) -> LineBuilder<'_>;
    pub fn warn(&mut self, text: impl Display) -> LineBuilder<'_>;
    pub fn fail(&mut self, text: impl Display) -> LineBuilder<'_>;
    /// Emit the connector's recent events (§5) as `Event` lines.
    pub fn recent<E: RecentEvent>(&mut self, ring: &EventRing<E>);
    /// One line per outbound route, from `RouteStats` (§4.4).
    pub fn routes(&mut self, routes: &RouteStatsView);
    pub fn deadline(&self) -> Instant;   // instance clock
    pub fn remaining(&self) -> Duration;
    pub fn request(&self) -> &ProbeRequest;
}

// LineBuilder::fact(key, value) -> LineBuilder; the line is emitted on drop.
out.warn(format_args!("{}: 1 send failed (buffer full)", topic))
    .fact("sent", 1230)
    .fact("failed", 1)
    .fact("last_ms", 2000);
```

The subject is fixed per handler (the scheme or record key it registered
under), so a handler cannot write lines in another connector's name.
`ProbeOut::subject_suffix("b1")` exists only for an input, whose subject is
its record key.

Taking `impl Display` lets a probe write `format_args!` without allocating
in its own code; core renders into the bounded `String` once.

### 3.3 The trait and the request

```rust
pub trait Probe: Send + Sync {
    /// Reads only, and returns by `out.deadline()`.
    fn probe<'a>(&'a self, out: &'a mut ProbeOut<'_>) -> BoxFuture<'a, ()>;
}

pub struct ProbeRequest {
    pub connector: Option<String>,   // scheme filter
    pub record: Option<String>,      // input filter (record key)
    pub discover: Vec<String>,       // narrowing only (§7.3)
    pub timeout: Duration,           // already clamped by core
}
```

The issue sketched `probe(&self, req, out)`. The request is reachable as
`out.request()` instead, so the deadline, the request and the sink travel as
one value and a handler cannot hold the request past the sink.

## 4. Core: registration and running

### 4.1 The registry

A connector has no object after `build()`: `ConnectorBuilder::build` returns
futures and is dropped by `AimDbBuilder::build()` (`builder.rs`, the loop
that collects connector futures). The database, however, exists before any
connector builds (`build(&self, db: &AimDb)`), so the registry lives there:

```rust
// aimdb-core/src/probe/mod.rs
pub(crate) struct ProbeRegistry {
    entries: Mutex<Vec<ProbeEntry>>,   // std::sync / spin, as inbound_key.rs
}

struct ProbeEntry {
    kind: ProbeKind,                   // Connector(scheme) | Input(record key)
    probe: Arc<dyn Probe>,
}

impl AimDb {
    /// Register the probe for a connector. Called from `ConnectorBuilder::build`.
    pub fn register_probe(&self, scheme: &str, probe: Arc<dyn Probe>);
}
```

`ProbeRegistry` sits in `AimDbInner`. Registration happens only while
`build()` runs; after that the vector is read under the lock and cloned
(`Arc`s) at the start of each probe run, so a run never holds the lock
across an `.await`.

`AimDbBuilder::build()` knows every connector's scheme. After the connector
loop it records which schemes registered no probe; a run reports each as
`info  <scheme>: no probe`. This is the migration path for out-of-tree
connectors (R1): nothing breaks, the gap is visible.

`ConnectorBuilder` itself does not change. A trait method (`fn probe(&self)
-> Option<Arc<dyn Probe>>`) was considered and rejected: the probe needs the
handles that `build()` creates (the session's mailbox, its ring, its route
stats), so it can only exist once `build()` has run.

### 4.2 Inputs

```rust
impl<T> RecordRegistrar<'_, T> {
    /// Register a probe for this record's input. Reported under the record key.
    pub fn probe(&mut self, probe: impl Probe + 'static) -> &mut Self;
}
```

The registrar stores the probe on the record's build-time state;
`build()` moves it into the registry alongside the connectors. `Input::wire`
is unchanged; its default is to call nothing. A record may have one input
probe; a second call replaces the first and is a `debug_assert`.

### 4.3 Running a probe

`aimdb_core::probe::run(db, request, sink)`:

1. Clamp `timeout` to `[1 s, 60 s]`; compute the deadline on the instance
   clock.
2. Select handlers by `connector` / `record`. An unknown scheme or key is
   one `fail` line (`no connector with scheme "mqt"`), not an RPC error, so
   a script sees one result shape.
3. Start every selected handler **concurrently** and race each against the
   deadline.
4. Emit lines **ordered by registration**: the first handler's lines stream
   through at once; later handlers' lines buffer until every earlier
   handler has finished. Each handler has a line budget
   (`PROBE_LINES_PER_SUBJECT`, default 24); lines past it are counted and
   reported once as `info  N more lines not shown`. With at most a few
   connectors, the worst-case buffer is a few KB, and it is reached only
   when an early handler is slow.
5. At the deadline, every handler still running is dropped (its future is
   cancelled, which must be safe: §4.5) and gets
   `fail  no answer within 10 s`.
6. Emit the summary line: subject `probe`, verdict = worst of all non-event
   verdicts (`fail` > `warn` > `ok`; `info` alone is `ok`), text
   `1 fail, 1 warn, 4 ok, 1 info`, facts `fail warn ok info` and
   `elapsed_ms`. The summary is always the last line and carries
   `end = true` on the wire (§8).

Concurrency matters: a KNX gateway that never answers must not eat the MQTT
probe's time. Ordering matters for a person reading the text output.

### 4.4 Outbound route counters are core's

`OutboundRoutes` already counts per route (`RouteStats`: `sent`,
`rejected`, `lagged`, `topic_overflow`, `serialize_failed`) but keeps the
counts in `&mut self`, owned by the session loop. Every connector would
otherwise ask its loop for a copy.

Change: `OutboundRoutes::stats_view() -> RouteStatsView`, a cheap `Arc`
handle over per-route counters held in `portable_atomic::AtomicU64` (already
a core dependency, with the critical-section fallback for 32-bit MCUs), plus
`last_sent: Stamp`. The loop keeps counting exactly where it does today;
`stats()` keeps its signature and reads the same atomics. Any probe can then
print route lines without touching the loop:

```
mqtt   ok    sensors/temp/outdoor: sent=1240 rejected=0 lagged=0  last_ms=1980
mqtt   warn  sensors/temp/indoor: 1 send rejected  sent=1230 rejected=1 last_ms=2000
```

`ProbeOut::routes()` renders these, with `warn` when any loss counter is
non-zero. A route's topic is the route's configured topic (or its pattern
for dynamic topics), never a value-derived topic.

The same is done for `InboundDispatch`: a per-route `delivered` count and
`last_delivered` stamp, so a probe can say which subscription has never
received anything.

### 4.5 Reaching the session loop: the probe port

Some answers must come from inside the session loop: the MQTT ping, the KNX
connection-state request, a discovery window. The loop is a single future
(`client_loop` in `aimdb-mqtt-connector/src/embedded/session_loop.rs`,
`drive_connection` in `aimdb-knx-connector/src/client.rs`) parked in a
`select3`. Core provides one small, runtime-neutral primitive both use:

```rust
// aimdb-core/src/probe/port.rs
pub fn probe_port<Q, A>() -> (ProbeAsk<Q, A>, ProbeServe<Q, A>);

impl<Q, A> ProbeAsk<Q, A> {           // held by the Probe handler
    pub async fn ask(&self, q: Q) -> Option<A>;   // None: loop not running / gone
}
impl<Q, A> ProbeServe<Q, A> {         // held by the session loop
    pub fn poll_request(&mut self, cx: &mut Context<'_>) -> Poll<Ticket<Q, A>>;
}
impl<Q, A> Ticket<Q, A> {
    pub fn query(&self) -> &Q;
    pub fn answer(self, a: A);         // may be kept and answered later
}
```

- One slot, behind a `Mutex<Option<…>>` and an `AtomicWaker`. A second
  probe while one is in flight waits for the slot; probes are rare.
- `ask` is cancel-safe: dropping it (deadline) marks the slot abandoned; a
  later `answer` is discarded. The loop never blocks on a probe.
- `poll_request` is a fourth `select` arm in the loop. It costs one poll per
  wake and no allocation; the loops move from `select3` to `select4`
  (`embassy_futures::select::select4`). The size test
  `the_session_future_has_not_outgrown_the_loop_it_replaced` gets the new
  budget written down.
- A `Ticket` that needs a later answer (a ping in flight) is kept in the
  loop's state and answered when the response arrives or the loop resets.
  A session that ends drops the ticket; `ask` then returns `None` and the
  probe says `fail  session restarted during the probe`.

The answer `A` is a plain snapshot struct (state, counters, a round-trip
time). The loop copies it and moves on; **all text is rendered in the probe
handler**, on the AimX server's task, not in the session loop.

## 5. Recent events

### 5.1 The ring

```rust
// aimdb-core/src/probe/ring.rs
pub struct EventRing<E> { /* Mutex<Deque<(Stamp, Level, E), N>> */ }

impl<E> EventRing<E> {
    pub fn new(clock: Arc<dyn InstanceClock>) -> Arc<Self>;
    pub fn record(&self, level: Level, event: E);   // drops the oldest
    pub fn snapshot(&self) -> Vec<(Stamp, Level, E)> where E: Clone;
}
```

- `N` is `aimdb_core::probe::EVENT_RING_LEN`, default 8, set by the
  `probe-ring-4` / `probe-ring-16` / `probe-ring-off` features (cargo
  features are additive; the smallest selected wins, `off` wins over all).
  With `off`, `record` is a no-op, `snapshot` is empty, and the probe prints
  `info  recent events: not kept on this build`.
- The ring stores **owned** events with bounded strings (§5.3) and renders
  text only when probed (R6, and no formatting cost on the hot path).
- One ring per connector instance, created in `build()` from `db`'s clock
  and passed to the session task: no globals (R11). Two MQTT connectors
  keep two rings.
- `Stamp` is the instance clock (design 058). Age is `now - stamp`; the
  probe prints it as `38 min ago` in text and `age_ms` in facts.
- The ring also counts how many events it dropped since start
  (`recent events: 8 shown, 31 older not kept`).

### 5.2 `report()` records

Each crate's `report()` today is a free function that logs (`events.rs`).
It gains the ring:

```rust
pub(crate) fn report(ring: &Recent, event: MqttEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    let level = event.level();
    ring.record(level, event.to_owned_bounded());
    event.log();      // the existing log_* call, unchanged in wording
}
```

`Recent` is the crate's `EventRing<MqttEvent<'static>>` type alias. Call
sites already sit in code that has the session's settings; they pass the
ring along with them. Events raised before a ring exists (a connector's
`BuildFailed`) keep calling a ring-less `report_unrecorded(event)`.

Core's own `report()`s (router, sessions, outbound) do not get rings in this
design (§13, open question 2).

### 5.3 Owned events

Events borrow (`topic: &'a str`, `error: &'a rumqttc::ConnectionError`) so
reporting allocates nothing. The ring needs `'static` copies:

- `&'a str` fields become `Text<'a>`: `Borrowed(&'a str)` or
  `Owned(heapless::String<TEXT_MAX>)` (default 48 bytes, cut with `…`).
  `Text<'a>` derefs to `&str` and implements `defmt::Format`, so call sites
  and log calls do not change.
- Foreign error fields (`&rumqttc::ConnectionError`, `embedded_tls::TlsError`)
  are converted on copy into `Text::Owned` of their `Display` (std) or their
  variant name (no_std), since the ring cannot hold the reference.
- Each crate implements `fn to_owned_bounded(&self) -> Event<'static>` by
  hand; a unit test per crate round-trips every variant.

### 5.4 One wording for log and probe

Today each arm of `report()` is a `log_*!` call with a literal format string.
`defmt` interns that literal and cannot take a runtime `Display`, so the
wording cannot simply move into `impl Display` and be logged from there
without losing defmt's cheap, interned frames.

Each crate's events are therefore declared once in a table that core's
`aimdb_core::event_wording!` macro expands twice, from the same literal:

```rust
aimdb_core::event_wording! {
    impl MqttEvent<'_> {
        BrokerUnresponsive
            => warn(target::SESSION_LOOP, "MQTT: broker unresponsive"),
        ConnectFailed
            => warn(target::SESSION, "MQTT: connect failed, will retry"),
        PublishTooLarge { topic, bytes, broker_limit }
            => warn(target::SESSION_LOOP,
                    "MQTT: skipping publish to '{}': {} bytes exceed the {} limit",
                    topic, bytes, limit_name(*broker_limit)),
    }
}
```

It generates:

- `fn level(&self) -> Level`,
- `fn log(&self)` — the `log_warn!(target: …, "<literal>", args…)` the crate
  has today, so `tracing`, `log` and `defmt` output is byte-for-byte what it
  is now, and `tests/log_targets.rs` keeps working,
- `impl Display` — `write!(f, "<literal>", args…)` from the same literal.

The literal must use only placeholders both `core::fmt` and `defmt` accept
(`{}` and `{:?}`); that is already the rule in `aimdb-core/src/log.rs`. The
macro rejects anything else at compile time. Arguments must be `Display`
(for the probe) and `defmt::Format` (for defmt); `LogDisplay` / `LogDebug`
cover foreign types, as today.

Of the issue's wording, "each event type gets a `Display` impl next to its
`defmt::Format`": the `Display` impl is generated, and `defmt::Format` on
the event *enum* is not needed (KNX's derive stays, MQTT never had one).

## 6. Inputs

An input's channel moved into its source task, so the input keeps counters
beside it and its probe reads them (R2, "never measures"):

```rust
// examples/embassy-mqtt-connector-demo/src/board.rs
#[derive(Default)]
struct Seen { edges: AtomicU32, last: AtomicU64 /* Stamp nanos */ }

impl<T> Input<T> for PushButton where /* … */ {
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) {
        let seen = Arc::new(Seen::default());
        let probe_seen = seen.clone();
        reg.source(move |ctx, producer| async move {
            // … on each debounced rising edge:
            seen.edges.fetch_add(1, Relaxed);
            seen.last.store(ctx.clock().now().as_nanos(), Relaxed);
        });
        reg.probe(probe_fn(move |out| {
            let edges = probe_seen.edges.load(Relaxed);
            match edges {
                0 => out.warn("PushButton: no rising edge since start").fact("edges", 0),
                n => out.ok(format_args!("PushButton: {n} rising edges since start, last {} ago",
                                         Age(out.age(probe_seen.last.load(Relaxed)))))
                        .fact("edges", n).fact("last_ms", out.age_ms(..)),
            };
        }));
    }
}
```

`probe_fn` is a core helper turning a synchronous closure over `ProbeOut`
into a `Probe`; most input probes are synchronous reads.

"No edge since start" is a `warn`, not a `fail`: a button nobody pressed is
not broken. A `Thermometer` that has produced nothing for three periods is a
`warn` with `last_ms`; one whose source returned (the sim's
`readings: Some(n)`) is `info  finished after 5 readings`.

## 7. Safety, enforced by core

### 7.1 Read-only

- `probe` is accepted under `SecurityPolicy::ReadOnly`; it is listed with
  `read` in the permission check, not with `write`.
- A probe has no handle that writes: `ProbeOut` exposes no record writer,
  `Probe` gets no `AimDb`. What a handler captured at `build()` is up to
  its author; the review checklist for connectors (design 012) gains "the
  probe's captured handles are read handles, and its port queries change no
  session state except discovery".
- Active checks allowed by protocol:
  - MQTT: `PINGREQ`. Stateless by definition.
  - KNX: `CONNECTIONSTATE_REQUEST`, which the engine already sends every
    55 s; the probe only moves the next one forward.

  An active check can make a dead link visible *sooner* (no `PINGRESP`, no
  `CONNECTIONSTATE_RESPONSE`), and the loop's existing liveness rules may
  then reset the session. That is the loop reacting to the link, not the
  probe acting, and it is the answer the person asked for. The probe never
  calls into reconnect logic itself.

### 7.2 Bounded

Deadline and line budgets as in §4.3. Additionally:

- a run holds one AimX subscription slot (`max_subs_per_connection`);
- one run per connection at a time (`TOO_MANY_SUBSCRIPTIONS`-style
  `PROBE_BUSY` error otherwise), and one in flight per connector (the port's
  single slot);
- a discovery window is at most the remaining deadline minus 1 s, so the
  unsubscribe and the summary still fit.

### 7.3 Discovery under an allow-list

```rust
MqttConnector::new(broker_url)
    .with_client_id("hub")
    .with_probe_discovery(["sensors/", "plant/boiler/"]);

KnxConnector::new(gateway)
    .with_probe_discovery(["1/2/", "3/"]);   // group-address prefixes
```

- The allow-list is fixed at build. `ProbeRequest::discover` is intersected
  with it: a requested prefix is used only if it starts with an allowed one
  (`sensors/temp/` under `sensors/`). Anything else is one
  `warn  discovery under "plant/" not allowed` line. No `discover` in the
  request means no discovery (discovery is opt-in per call as well).
- Without `with_probe_discovery`, the probe prints
  `info  discovery: off (not enabled for this connector)`.
- Prefixes may not contain wildcards (`+`, `#`); core's `topic_pattern`
  validation rejects them at build.

**MQTT discovery** subscribes `<prefix>#` at QoS 0 on the live session for
the window, then unsubscribes. Two extra rules keep it from affecting the
application:

1. **No overlap.** If the discovery filter overlaps any inbound route
   filter of the application (`sensors/#` and `sensors/temp/indoor` do),
   discovery under that prefix is skipped with
   `info  discovery under sensors/ skipped: overlaps the subscription sensors/temp/indoor`.
   Overlapping subscriptions can make a broker deliver a message twice, and
   the second copy would reach the application's record. Brokers differ
   here (MQTT 3.1.1 allows either; v5 subscription identifiers would let us
   tell the copies apart), so the rule is the conservative one. A later
   version can relax it on the v5 backend using subscription identifiers.
2. **Not delivered.** Publishes arriving on the discovery subscription are
   matched against it *before* `InboundDispatch` and never reach a record.
   The loop folds each into a bounded table: topic (cut to `TEXT_MAX`),
   count, retained flag, last-seen, and payload **shape** (empty, `N bytes
   binary`, `JSON object {a,b,c}` with at most 4 key names, `JSON number`,
   `JSON string`, `text N bytes`). The payload itself is discarded after
   shaping. At most `DISCOVERY_TOPICS_MAX` (default 32) topics; beyond that
   they are counted, not listed.

On the embedded backend `ClientStateNoQueue` tracks one outstanding request,
so the discovery `SUBSCRIBE` / `UNSUBSCRIBE` wait their turn like the
application's subscriptions; a probe never jumps ahead of a QoS 1 publish.
If the session resets during the window, nothing is re-subscribed: the
discovery subscription is not in `subscribe_topics` and dies with the
session; the probe says so.

**KNX discovery** is passive: the tunnel already receives every group
telegram the gateway forwards. During the window, telegrams to group
addresses under the allowed prefixes that match no inbound route are
counted per address (count, last seen, APCI kind: write/response/read, data
length). No request goes on the bus.

### 7.4 No values

Enforced in three places: `ProbeOut` has no API taking a payload; route and
dispatch counters are counts and stamps; discovery shapes the payload inside
the loop and keeps only the shape. A test feeds known secret payloads
through every first implementation and asserts no probe line contains them.

## 8. AimX 4.1

The probe streams, so it rides the existing subscription machinery instead
of a new frame type.

**Request.** A `sub` frame on the reserved topic `$probe`, with the
existing `params` field (already in the codec's `Frame`, unused on `sub`):

```json
{"t":"sub","id":7,"topic":"$probe","params":{"connector":"mqtt","discover":["sensors/"],"timeout_ms":10000}}
```

- `Inbound::Subscribe` gains `params: Option<Payload>`; `Session::subscribe`
  gains a defaulted `subscribe_with(topic, params)` that forwards to
  `subscribe` when `params` is `None`. Existing dispatches are unaffected.
- `AimxSession::subscribe` routes `$probe` to `probe::run` before record
  lookup. Record keys are not validated today, so a record could be named
  `$probe`; `AimDbBuilder::build()` rejects record keys starting with `$`
  (reserved for AimX system topics, following MQTT's `$SYS` convention).
  That is a build-time error for such a key, the one breaking edge of this
  design, and expected to hit no one.

**Lines.** Each line is an `event` frame on that subscription; `ts` is the
stamp when the line was emitted; `data` is:

```json
{"verdict":"warn","subject":"mqtt","text":"sensors/temp/indoor: 1 send rejected","facts":{"sent":1230,"rejected":1,"last_ms":2000}}
{"verdict":"event","subject":"knx","text":"KNX outbound: no ACK for sequence 7","level":"warn","age_ms":121000}
{"verdict":"fail","subject":"probe","text":"1 fail, 1 warn, 4 ok, 1 info","facts":{"fail":1,"warn":1,"ok":4,"info":1,"elapsed_ms":5012},"end":true}
```

The stream ends after the `end` line; the client unsubscribes on it. Line
loss is visible through the existing `seq` gap accounting
(`SubUpdate::skipped`); the client reports a gap as
`warn  probe: N lines lost in transit` and counts it as `warn`.

**Version.** `PROTOCOL_VERSION` becomes `"4.1"`. `version_compatible`
matches on the major, so 4.0 clients keep working. A 4.1 client talking to
a 4.0 server sees `NOT_FOUND` for `$probe` and reports
`this instance does not support probe (AimX 4.0)`.

**Policy.** Allowed under `ReadOnly`. `welcome.permissions` is unchanged;
the version tells a client the method exists.

**Serial.** One AimX connection carries the probe next to other tools, as
`aimdb watch` does. Lines are small (≤ ~400 bytes framed) and the per-run
line budget bounds the burst.

## 9. Client, CLI and MCP

### 9.1 `aimdb-client`

```rust
pub async fn probe(conn: &AimxConnection, req: ProbeRequest)
    -> Result<impl Stream<Item = Result<ProbeLine, ClientError>>, ClientError>;
```

Same `ProbeLine` type as core (re-exported; it is `serde` and
`no_std + alloc`). The client applies its own deadline of
`timeout + 5 s` on top, so a dead link ends the stream with
`ClientError::Timeout` rather than hanging.

### 9.2 `aimdb probe`

```
aimdb probe [--connector <scheme>] [--record <key>] [--discover <prefix>]...
            [--timeout <secs>] [--format text|json] [--fail-on warn]
```

- **Text**: columns `subject  verdict  text  facts`, subject and verdict
  padded to the widest seen so far; event lines show `·`, the age and the
  level, as in the example in the issue. Colour only on a TTY.
- **JSON**: one object per line, exactly the `data` above (NDJSON).
- **Exit codes**: `0` no `fail`; `5` (`EXIT_PROBE_FAILED`, new in
  `tools/aimdb-cli/src/error.rs`) at least one `fail`, or at least one
  `warn` with `--fail-on warn`; existing codes 1–4 keep their meaning
  (connection failed is still `3`, client-side timeout `4`).

### 9.3 MCP

A `probe` tool in `tools/aimdb-mcp/src/tools/probe.rs` with the same
parameters, registered whether or not `--allow-writes` is set, returning
the text form as one text block. Its description states that the probe is
read-only and lists the verdicts, so an agent knows to look at `fail` lines
first.

## 10. First implementations

### 10.1 MQTT, embedded (Embassy; mountain-mqtt, MQTT v5)

Port query: `enum MqttProbeQ { Status, Ping, Discover(Vec<String>, Duration) }`.

| Part | Source |
|---|---|
| Active: `keep-alive ping answered  rtt_ms=14` | Loop sends `PINGREQ` now (the same `state.send_ping()` it uses on schedule, via `ring.try_put`), keeps the ticket, answers on `PINGRESP` with the round trip. Ping dropped because the write ring is full → `warn  ping not sent: write ring full`. No answer before the deadline → `fail`. |
| State: `connected to <host:port> as client "<id>" (TLS on/off)  up_s=…` | Loop's state: `connected`, connect stamp, `broker_max`. Host and client ID come from build-time settings; credentials are never printed. |
| State: one line per subscription, granted QoS or refusal | The SUBACK reason codes, which `drain_packets` sees and today discards; the loop keeps one `u8` per entry of `subscribe_topics`. |
| State: per outbound route | `RouteStatsView` (§4.4), no loop involvement. |
| Discovery | §7.3. |
| Recent events | Ring passed to `run_session` with the other settings. |

### 10.2 MQTT, native (Tokio; rumqttc)

rumqttc sends keep-alive pings itself and offers no call to send one. The
probe therefore reports the last observed round trip instead of a fresh
one: the event loop task notes `Outgoing::PingReq` and the following
`Incoming::PingResp` with stamps.

- `ok  keep-alive answered 12 s ago  rtt_ms=9` when the last answer is
  younger than 1.5 × keep-alive.
- `warn  no keep-alive answer for 140 s (keep-alive 60 s)` otherwise.
- The line says "observed", so a reader knows it was not a fresh request.

Subscriptions: `Incoming::SubAck` return codes, matched to topics by the
order subscriptions were issued at build. Discovery uses
`AsyncClient::subscribe` / `unsubscribe` with the same overlap rule and the
same pre-dispatch filter in `run_event_loop`. State lives in an
`Arc<Mutex<NativeState>>` shared by the event loop task and the probe; no
port is needed on this backend except for discovery.

### 10.3 KNX

Port query: `enum KnxProbeQ { Status, ConnectionState, Discover(Vec<GaPrefix>, Duration) }`.

- **Active**: `TunnelEngine::request_heartbeat(now)` sets `next_heartbeat`
  to `now` unless a `CONNECTIONSTATE_REQUEST` is already pending; the loop
  answers the ticket when `heartbeat_pending_since` clears (with the round
  trip) or when the engine drops the connection. The engine is sans-io, so
  this is a unit test in `tunnel.rs` with no socket.
- **State**: gateway address, phase (`connected` with channel id, `backoff`
  with time to retry, `connecting`), pending ACKs, per outbound route
  (`RouteStatsView`), per inbound group address (dispatch counters).
- **Discovery**: passive (§7.3).
- **Recent events**: the five `KnxEvent`s, with the ring.

### 10.4 AimX servers (serial, UDS, TCP)

No port: the server already knows its listener and its open sessions.
`ok  listening on /run/aimdb.sock, 2 of 8 connections in use`, and for
serial `ok  serving /dev/ttyACM0 at 115200 baud, client connected 4 min ago`.
These live in core's `session::server` so all three connectors get them by
calling one helper from `build()`.

### 10.5 Demo inputs

`PushButton` (Embassy MQTT and KNX demos) and `Thermometer`
(`examples/mqtt-connector-demo-common/src/sim.rs`), as in §6.

### 10.6 Other connectors

WebSocket and Zenoh get a minimal probe in the same release (state, routes,
recent events; no active check where the library offers none) so R1 holds
for the repository. The two `ws` connectors share the server helper of
§10.4.

## 11. Footprint

| Item | Embedded (thumbv7em, default) |
|---|---|
| Event ring, 8 × (stamp 8 + level 1 + event with ≤ 2 × 48-byte text) | ≈ 1 KB per connector; `probe-ring-4` ≈ 0.5 KB; `probe-ring-off` 0 |
| Probe port | one slot + waker, < 64 bytes per connector |
| Route counters | 6 × 8 bytes per route (were already 5 × 8, now atomic) |
| Discovery table | allocated only during a window: ≤ 32 × ~80 bytes |
| Line buffering during a run | ≤ 24 lines × ~200 bytes per later subject, freed at the end |
| Code | `core::fmt` is already linked by AimX JSON on these targets; the wording macro adds one `Display` per event enum |

The session future grows by the port arm and the kept ticket; the size test
in `session_loop.rs` is updated with the measured figure.

## 12. Testing

- **Core**: `probe::run` with fake handlers: ordering by registration under
  concurrent completion, deadline cancellation and its `fail` lines, line
  budget, summary arithmetic, `no probe` lines, narrowing of `discover`.
- **Port**: cancel-safety (drop `ask` mid-flight, then answer), session reset
  drops the ticket, a second `ask` waits.
- **Wording**: per crate, every event variant's `Display` equals the text
  the `log` backend writes for it (captured through the existing
  `capture` module plus a test logger).
- **Ring**: drop-oldest, dropped count, `off` feature.
- **MQTT embedded**: existing in-memory transport tests extended — ping
  answered, ping with no answer, SUBACK refusal shown, discovery that does
  not reach a record, overlap rule.
- **KNX**: `TunnelEngine` heartbeat-forward unit test; discovery counts
  unmatched telegrams; nothing is sent during discovery.
- **No values** (§7.4) across all first implementations.
- **End to end**: `aimdb probe` against the Tokio MQTT demo with a local
  broker in CI, checking exit codes 0 and 5 and the JSON shape.
- **Wire**: codec round-trip of `sub` with `params`; a 4.0 server answering
  `$probe` with `NOT_FOUND`.

## 13. Rollout

1. Core: line model, `ProbeOut`, registry, `run`, port, ring,
   `event_wording!`, atomic `RouteStatsView`, dispatch counters, AimX 4.1
   `$probe`. Connectors without probes show `no probe`.
2. Wording migration: every crate's `events.rs` moves to `event_wording!`
   (no change to log output; checked by the wording tests).
3. MQTT (both backends) and KNX probes, including discovery; AimX server
   probes.
4. Client, CLI, MCP.
5. WebSocket, Zenoh, demo inputs; docs: connector development guide
   (design 012) gains "Implementing a probe", and the usage guide gains
   `aimdb probe`.

Each step is releasable; step 1 alone already gives `aimdb probe` a useful
answer for routes.

## 14. Alternatives

The issue lists the product-level alternatives (structured results per
connector, log capture, active-only, free-form text, a `Discover` trait,
pre-flight probes, acting probes). Design-level ones considered here:

- **A probe method on `ConnectorBuilder`.** The builder is gone after
  `build()`, and the probe needs handles `build()` creates (§4.1).
- **Shared state only, no port.** Simpler loops, but no active check: a
  stale "last ping" is not "does it work now". Kept for the native MQTT
  backend, where the library leaves no choice (§10.2).
- **A per-crate global ring.** Breaks with two connectors of one kind and
  is rejected by `check-no-globals`.
- **Rendering events to text when recorded.** Simpler (no owned copies), but
  formats on every warning even when nobody probes, including bursts like
  `NativePublishFailed`. Owned copies are cheap and rare to render.
- **A new `probe` request/stream frame type in AimX.** Needs a stream-end
  frame and new codec paths; `sub` with `params` reuses flow control, gap
  accounting and subscription caps as they are.
- **A one-shot `call` returning all lines.** Simplest wire, but no progress
  on a 10 s probe and unbounded reply size on serial.

## 15. Open questions

1. **Ring size and text bound on small MCUs.** Proposed: 8 entries × 48-byte
   text by default (≈ 1 KB), features to go to 4 or off. Measure on the
   STM32H5 demo before fixing the default.
2. **Core's own events and instance health.** Router, session and outbound
   events could get a ring, and a `core` probe could report buffer overruns
   per record and task restarts. Proposed: out of scope here; the registry
   and line model allow a `core` subject later without protocol change.
3. **Relaxing the discovery overlap rule on MQTT v5** with subscription
   identifiers, so discovery can run next to wildcard application
   subscriptions. Proposed: follow-up, once the embedded backend can set
   and read subscription identifiers.
4. **Time format in text.** `38 min ago` is relative; facts carry `age_ms`.
   Wall-clock times would need the instance's wall clock, which only TLS
   builds have (SNTP). Proposed: relative only.
5. **Probe rate limits.** One run per connection and one in flight per
   connector bound the load. A per-instance minimum interval between active
   checks (e.g. 1 s) may be worth adding if agents probe in a loop.
