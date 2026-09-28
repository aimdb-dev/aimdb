# 055 — Wildcard inbound links

**Status:** ✅ Implemented — validated by a spike (§3), 2026-09-27; matching
moved into connectors (§3.3), 2026-09-28

**Scope:** inbound links whose topic is a pattern: matching in
`aimdb-core`'s router, the matched topic and captures passed to a
match-aware deserializer, optional per-record key interning surfaced in
record metadata, a grammar trait through which each connector matches its
own topics, and the MQTT grammar in `aimdb-mqtt-connector`. The connector
interface changes (§5.8); the user-facing link API does not.

**Independent of** [054](./054-zero-alloc-connector-boundary.md). §7
describes what changes when 054 lands.

---

## 1. Problem

An inbound link maps one exact topic to one record:

```rust
reg.link_from("mqtt://sensors/kitchen/temp").with_deserializer(parse).finish();
```

`Router::route` compares an incoming topic with each route's `resource_id` by
string equality. Records and their routes are fixed at `build()`, so a
publisher that was not known at startup has no route and its messages are
dropped. Applications with a changing set of devices must pre-declare a pool
of records and assign pool slots out of band.

Two facts make a small change sufficient:

- Both MQTT backends subscribe to `Router::resource_ids()` verbatim, so a
  filter such as `sensors/+/temp` already subscribes correctly at the broker.
  Only the router drops the messages.
- The deserializer receives `(ctx, bytes)` but not the topic, so it cannot
  tell which publisher sent a matched message.

## 2. Goals and non-goals

**Goals**

- G1. One inbound link with a topic pattern feeds many topics into one
  record.
- G2. Named captures (`{device}`) are available to the deserializer as
  `&str`, together with the full topic.
- G3. Optionally, a capture becomes a **key**: a small integer assigned the
  first time a value is seen, from a bounded table per record.
- G4. No behaviour change for existing links that do not use patterns.
- G5. No per-message allocation on pattern routes, and none for a known key.
- G6. Protocols with different wildcard rules (MQTT, Zenoh) plug in without
  changing core.

**Non-goals**

- Creating records at runtime. One record per pattern is the unit.
- Outbound topic templates (`link_to("mqtt://out/{device}")`). Possible
  follow-up on 054's `TopicWriter`.
- The AimX/WebSocket wildcard grammar over record keys
  (`session/topic_match.rs`). It matches dot-separated record keys, not broker
  topics.
- Making the match visible to plain `with_deserializer` closures (§5.5).
- Releasing or reusing keys. A key lives as long as the process.
- An index (trie) over pattern routes. Routes are scanned linearly (§5.4).
- Inbound subscribe QoS. Both MQTT backends subscribe at QoS 1 and ignore
  `with_qos` on inbound links, today and after this design (§5.7).

## 3. Evaluation

Before this revision, the design was implemented as a spike on the
in-tree code: core (`topic_pattern.rs`, router, builder, link builder,
metadata), both MQTT backends, the `b0_alloc_connector` bench rows and
end-to-end tests. About 775 changed lines plus 1,250 new ones, most of them
tests. Everything below was measured on that spike, not estimated.

### 3.1 Results

| Question | Result |
|---|---|
| Allocations per message: exact / pattern / known key / new key | **0 / 0 / 0 / 1** (the key name, 24 bytes) |
| Routing time, 64 other routes, release build | exact 76–83 ns, pattern with key 132–143 ns, no match 41–47 ns |
| Grammar as a trait (`&'static dyn`) instead of a data struct | same allocations; latency within run-to-run noise |
| Zenoh-style `a/**/b` (multi-level wildcard mid-pattern) | works through the trait, with a backtracking matcher; not expressible with the data struct |
| Both MQTT backends against one broker, pattern next to a covered exact link | each subscribes only `parity/+/in`; each record gets the message once; capture and key reach the deserializer |
| `mountain-mqtt` with `+` in SUBSCRIBE | accepted unchanged |
| Mosquitto 2.0.18, overlapping subscriptions, one publish | **MQTT 3.1.1 client: 1 copy. MQTT 5 client: 2 copies.** |
| Key table memory | about 66 bytes per key plus the name; 1,024 keys ≈ 67 KB, 65,535 keys ≈ 4.3 MB |
| Key table growing lazily instead of reserving capacity | still 1 allocation per new key on average (log₂ n extra in total) |
| thumbv7em-none-eabihf: core (`alloc`, `connector-session`, `remote`) and the embedded MQTT backend | builds |

### 3.2 What the evaluation changed

1. **The covering set is required, not an optimisation.** The native backend
   speaks MQTT 3.1.1 and the embedded one MQTT 5, and Mosquitto sends
   overlapping subscriptions one copy and two copies respectively. Without
   the covering set (§5.7) the backends would disagree.
2. **The grammar is a trait.** Zenoh allows `**` anywhere and hides
   verbatim `@…` chunks from wildcards at any level. A data struct with
   tokens and a "must be last" rule cannot say that (§5.1). §3.3 moved
   the matcher itself behind the trait.
3. **Fewer checks run at `build()`.** "Capture shares a level with text"
   needs the separator, and "multi-level capture must be last" depends on
   the grammar. Both run when the connector builds (§5.3).
4. **One inbound path.** Whether `+` is a wildcard depends on the grammar,
   so a route list built without one cannot tell. Every connector builds
   its router with `inbound_router`, and the grammar-less path is removed
   (§5.2).
5. **Keys are per record.** With per-link tables, two keyed links on one
   record handed out overlapping `KeyId`s. One table per record fixes that
   and matches the "array indexed by `KeyId`" guidance (§5.6).
6. **Key tables grow lazily.** Reserving full capacity cost 67 KB for 1,024
   keys before any device appeared; lazy growth costs nothing measurable on
   the per-message path (§5.6).
7. **Not additive** (§5.8): the connector interface changes.
8. **Rules the first draft left implicit:** setting both deserializers is an
   error; a literal topic with a match-aware deserializer takes the pattern
   path; the dropped counter is `AtomicU32` (thumbv7em has no 64-bit
   atomics).

### 3.3 After the spike: connectors own matching

The spike kept one matcher in core and asked the grammar about single
levels. That put every protocol's rules in core: backtracking over `**`
and hidden `@…` chunks for Zenoh, which has no connector yet, next to the
far simpler MQTT rules. Matching now belongs to the connector (§5.1): core
parses the `{…}` syntax, numbers the captures and routes; the grammar
compiles each pattern into a matcher and decides covering.

The trait moves from one call per wildcard level to one call per pattern
route. The routing times in §3.1 were measured with core's matcher;
criterion 6 measures the allocations again on the new shape.

## 4. User API

```rust
builder.configure::<Reading>("sensors.readings", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 256 })
       .link_from("mqtt://sensors/{device}/temp")
       .key("device", 1024)                        // optional (§5.6)
       .with_match_deserializer(|ctx, m, bytes| {
           let device = m.get("device").unwrap();   // &str borrowed from the topic
           let key = m.key();                       // Option<KeyId>; Some when .key(..) is set
           Reading::decode(key, bytes)
       })
       .finish();
});
```

- `{name}` matches exactly one level. `{name..}` matches zero or more
  levels; where it may stand is the grammar's rule (MQTT: last only).
- The grammar's own wildcards (`+`, `#` for MQTT) are also accepted as
  unnamed levels, for users who write filters by hand.
- `with_match_deserializer` is the only way to see the match. A pattern link
  with a plain `with_deserializer(|ctx, bytes|)` still works (every matching
  message is ingested) but cannot tell publishers apart. Setting both on one
  link is a configuration error.

## 5. Design

### 5.1 The grammar is a connector-supplied trait

Wildcard syntax is protocol-specific and the router is protocol-agnostic.
Core owns the `{…}` syntax and the capture numbering; the connector owns
levels, wildcards and matching:

```rust
// aimdb-core
pub const MAX_CAPTURES: usize = 8;
/// Byte range of each capture in the topic, indexed by capture number.
pub type Spans = [(u16, u16); MAX_CAPTURES];

/// A topic with its `{…}` syntax checked.
pub struct TopicPattern<'a> { /* the topic, split into parts */ }
pub enum PatternPart<'a> { Text(&'a str), Capture { name: &'a str, multi: bool } }

impl<'a> TopicPattern<'a> {
    pub fn parse(topic: &'a str) -> Result<Self, PatternError>;
    pub fn as_str(&self) -> &'a str;
    pub fn parts(&self) -> &[PatternPart<'a>];
    pub fn has_captures(&self) -> bool;
}

pub trait TopicGrammar: Send + Sync {
    /// Compile one pattern. Captures are numbered in `parts()` order.
    fn compile(&self, pattern: &TopicPattern<'_>)
        -> Result<Box<dyn TopicFilter>, String>;
    /// Whether filter `a` matches every topic filter `b` matches (§5.7).
    fn covers(&self, a: &str, b: &str) -> bool { a == b }
}

pub trait TopicFilter: Send + Sync {
    /// What the connector subscribes (`sensors/+/temp`).
    fn filter(&self) -> &str;
    /// Matches only `filter()` itself; the router compares strings.
    fn is_literal(&self) -> bool;
    /// Match `topic`, writing capture `i`'s byte range to `spans[i]`.
    fn matches(&self, topic: &str, spans: &mut Spans) -> bool;
}

/// KNX, WebSocket, AimX session connectors: no wildcards.
pub struct ExactGrammar;

// aimdb-mqtt-connector
pub struct MqttGrammar;   // '/', "+", "#" last only, `$` hidden at level 0
```

- Grammars are passed as `&'static dyn TopicGrammar` (unit structs:
  `&MqttGrammar`). `Router` stays non-generic; each pattern route costs one
  dynamic `matches()` call per message.
- The router never passes a topic longer than `u16::MAX` bytes, so spans
  fit.
- `ExactGrammar` rejects every pattern with captures and compiles the rest
  to string equality.
- A second grammar needs no change to core. Zenoh (053) can build on the
  `zenoh` crate's own key-expression inclusion for `covers`.

### 5.2 One inbound router per connector

```rust
impl AimDb {
    /// Every link on `scheme`, compiled against `grammar`, with key tables
    /// attached.
    pub fn inbound_router(&self, scheme: &str, grammar: &'static dyn TopicGrammar)
        -> DbResult<Router>;
}

impl Router {
    /// Filters to subscribe, with any filter covered by another removed.
    pub fn subscriptions(&self) -> Vec<Arc<str>>;
}

/// Routes `src` with the router the connector subscribed from.
pub fn pump_source(db: &AimDb, router: Router, src: impl Source + 'static)
    -> Vec<BoxFuture>;
```

- A connector subscribes and routes with the **same** router, so the two
  cannot disagree.
- The router keeps its grammar, which covering needs (§5.7).
- **Every in-tree connector moves to `inbound_router`** in the same change:
  MQTT with `&MqttGrammar`; KNX, the WebSocket server and client, and
  core's AimX session client (TCP, UDS, serial) with `&ExactGrammar`. Only then is a `{…}` link on those
  connectors an error instead of a warning, and a hand-written `+` on MQTT
  starts working.
- The grammar-less path is removed: `collect_inbound_routes`,
  `RouterBuilder`, `Route` and `Router::new`. A `Router` comes only from
  `inbound_router`, and `pump_source` and `pump_client` take it.

### 5.3 Validation

**At `AimDbBuilder::build()`**: the grammar-independent `{…}` syntax.
Errors name the record key and URL:

- unbalanced braces, empty names, names outside `[A-Za-z0-9_]`;
- two captures with the same name;
- more than 8 captures;
- `.key(name, ..)` naming a capture that the pattern does not have, a
  capacity of 0, or a capacity that differs from another keyed link on the
  same record (§5.6).

**At connector build** (`inbound_router`, returning `DbResult`):
everything that needs the grammar:

- whatever `TopicGrammar::compile` rejects. For MQTT: a capture sharing a
  level with text (`sensors/dev-{id}`), a multi-level capture or `#` that
  is not last, a wildcard that is not a whole level (`a+`);
- any `{…}` on an `ExactGrammar` connector;
- patterns returned by a `TopicResolverFn` (018). They go through every
  check in this section, including the `.key(..)` capture check: the router
  looks up the key's capture slot in the resolved pattern, and a missing
  capture is an error rather than a keyed link without keys (§5.6). Both
  paths share one parser for the `{…}` syntax.

### 5.4 Matching

Each pattern route is compiled once into a `TopicFilter`. A filter whose
`is_literal()` is true becomes an exact route.

`Router::route` checks exact routes as today, then pattern routes in
registration order, calling `matches()` with a `Spans` on its stack. A topic
matching several routes is delivered to each, as today.

Routes are scanned linearly. At 64 routes a pattern route costs about 60 ns
over an exact one (§3.1). An index can come later if a benchmark asks for it.

### 5.5 The match reaches the deserializer as a borrow

Every route's ingest receives the match:

```rust
pub type IngestFn =
    Arc<dyn Fn(&RuntimeContext, &TopicMatch<'_>, &[u8]) -> Result<(), String> + Send + Sync>;

pub struct TopicMatch<'a> { /* topic: &'a str, spans, names, key */ }

impl<'a> TopicMatch<'a> {
    pub fn topic(&self) -> &'a str;
    pub fn get(&self, name: &str) -> Option<&'a str>;
    pub fn key(&self) -> Option<KeyId>;   // Some iff the link has .key(..)
}
```

- `Router::route(&self, topic: &str, ..)` already borrows the topic for the
  whole call, so the router builds `TopicMatch` on its stack. No allocation,
  no change to `RuntimeContext`, and the match cannot outlive its message.
- `with_match_deserializer` passes the match to the closure; a plain
  `with_deserializer` ignores it. An exact topic is a route whose filter
  matches only itself, and the router compares it as a string.
- The closure borrows the context (`&RuntimeContext`), so no reference
  count changes per message. `with_deserializer` takes it by value and
  clones an `Arc` per message; moving it to a borrow is a breaking change
  for a later release.

### 5.6 Keys

```rust
pub struct KeyId(NonZeroU16);   // Option<KeyId> is 2 bytes; .index() is 0-based

.key("device", 1024)            // capture name, capacity 1..=65535
```

- **One table per record.** All keyed links of a record share it, so a
  `KeyId` names one value per record, and the same device seen through
  `temp/{dev}` and `hum/{id}` gets the same key. Keyed links on one record
  must give the same capacity.
- The table is `HashMap<Arc<str>, KeyId>` (hashbrown) plus
  `Vec<Arc<str>>` for the reverse direction, under one mutex (`std` or
  `spin`, as elsewhere in `aimdb-core`).
- **It grows as keys arrive**; capacity is a limit, not a reservation.
  Memory is about 66 bytes per key plus the name (§3.1).
- A new value costs one allocation (its name); a known value costs none.
- A `{name..}` capture can be a key; its value is the whole remainder.
- **When the table is full**, the message is dropped and the table's
  `dropped` counter (`AtomicU32`) increases. A message that reaches the
  deserializer of a keyed link always has `m.key() == Some(_)`.
- Keys are never reused while the process runs.
- `db.inbound_key_name("sensors.readings", key) -> Option<Arc<str>>`
  resolves a key for display, AimX and logs.

**Record metadata.** `RecordMetadata` gains

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub inbound_keys: Option<InboundKeysInfo>,

#[non_exhaustive]
pub struct InboundKeysInfo {
    pub captures: Vec<String>,   // one per keyed link
    pub capacity: u16,
    pub assigned: usize,
    pub dropped: u32,
}
```

It is filled on every build, not only with `observability`: a full table
silently turns away new publishers, so the numbers matter everywhere. The
field is optional in serde, so older AimX clients keep working. Key names
are not listed (there can be 65,535); `inbound_key_name` resolves one.

The lock is taken once per keyed message by the single task that routes for
the connector, so it is uncontended.

### 5.7 MQTT specifics

- `MqttGrammar` follows MQTT 3.1.1 §4.7: `+` one level, `#` the rest
  including the parent level (`a/#` matches `a`) and only last, a leading
  wildcard does not match a `$…` topic, and a wildcard must be a whole
  level.
- Both backends subscribe `subscriptions()` of
  `db.inbound_router("mqtt", &MqttGrammar)` and pass that router to
  `pump_source`.
- **Covering set.** A filter is left out when another matches every topic
  it matches (`sensors/kitchen/temp` under `sensors/+/temp`; `a/+/b` under
  `a/#`). Required for backend parity (§3.1). The router still fans each
  message out to every route.
- `MqttGrammar::covers` compares level by level. A wildcard covers a
  literal level only where it may match it: `#` and `+/x` do not cover
  `$SYS/x`, because the broker never delivers `$…` topics to a leading
  wildcard, so dropping `$SYS/x` would silence that link. `sensors/+`
  does cover `sensors/$x` (the rule is level 0 only). `+` covers `+`, and
  `#` covers `+` and `#`.
- **Subscribe QoS** stays 1 for every filter, as today. Covering drops a
  filter only where another delivers the same messages at the same QoS, so
  nothing is lost. Inbound `with_qos` stays unapplied; its doc says so.
- Outbound links reject patterns at `build()`: you cannot publish to a
  filter.

### 5.8 Compatibility

A breaking change to the connector interface. Every connector lives in this
repository and moves with it; the user-facing link API (`link_from`,
`with_deserializer`) does not change.

- Removed: `AimDb::collect_inbound_routes`, `RouterBuilder`, `Route`,
  `Router::new`. Connectors call `inbound_router`.
- `IngestFn` takes the `TopicMatch`. `pump_source` and `pump_client` take
  the `Router`.
- `InboundConnectorLink` gains `key`, `RecordMetadata` gains
  `inbound_keys` (the serde form stays backward compatible). Both become
  `#[non_exhaustive]`; the new `InboundKeysInfo` is from the start.

## 6. Guidance for pattern records

A pattern record is one interleaved stream: its latest value is whichever
publisher sent last. Applications that need per-publisher state keep it
downstream, for example a transform holding an array indexed by
`KeyId::index()`. Use `SpmcRing`; `SingleLatest` would drop other
publishers' values between reads.

When the broker binds topics to credentials (e.g. an ACL allowing a client
to publish only under `sensors/<its-name>/…`), the topic is the
authenticated part of a message and the payload is the publisher's claim.
Keep them separate in the record: identity from `m.get(..)`/`m.key()`,
everything else from the payload.

Watch `inbound_keys.dropped` in record metadata: a non-zero value means
publishers are being turned away because the key table is full.

## 7. Relation to 054

When 054 lands, `InboundDispatch::dispatch(topic, payload)` borrows the topic
for the whole ingest call, exactly as `Router::route` does today. Because
`TopicMatch<'a>` already borrows the topic:

- `with_match_deserializer`'s closure signature and `TopicMatch<'a>` are
  unchanged.
- `InboundDispatch::new` takes a `&'static dyn TopicGrammar` and replaces
  `inbound_router` + `pump_source` for migrated connectors.
- 054 does not change `with_deserializer`; its by-value context stays until
  a later breaking release (§5.5).

## 8. Alternatives considered

1. **Reuse `topic_matches` from `session/topic_match.rs`.** Wrong grammar:
   dot-separated with `*` for one level. Translating MQTT filters breaks on
   topics that contain dots.
2. **MQTT grammar hard-coded in core.** Makes the router protocol-aware; Zenoh
   (053) would add a second hard-coded grammar.
3. **Grammar as a data struct** (separator, tokens, a `hidden` function).
   Fits MQTT but cannot express Zenoh's mid-pattern `**` or per-level hidden
   chunks (§3.2). The trait costs nothing measurable.
4. **One matcher in core, a per-level grammar trait** (the spike's shape,
   §3.3). Core would carry every protocol's matching rules, Zenoh's
   backtracking included, and a Zenoh connector could not reuse the
   `zenoh` crate's own key-expression logic.
5. **Keeping the grammar-less route API** (`collect_inbound_routes`,
   `RouterBuilder`) beside `inbound_router`, for connectors outside this
   repository. There are none, and keeping it meant two ingest types and
   two paths through the link builder.
6. **Match carried in `RuntimeContext` (`ctx.inbound_match()`).** Reaches
   plain `with_deserializer` users, but costs one allocation per message,
   lets the match outlive the message through a cloned context, and breaks
   when 054 turns the topic into a borrow.
7. **Records created per new topic at runtime.** Per-publisher buffers and
   AimX addresses, but it is the post-`run()` registration problem, far
   larger than this.
8. **Positional captures only (`+` → index 0).** Indices shift when a pattern
   changes; names cost nothing at runtime.
9. **All checks at `build()`** by declaring grammars on the builder. Adds a
   registration step for every connector; connector-build errors are early
   enough.
10. **Subscribing every filter as written.** Duplicates on MQTT 5 but not
    3.1.1 (§3.1), so the backends would disagree. **Rejecting overlaps**
    instead rules out a legitimate layout.
11. **Delivering unkeyed messages when the key table is full.** Every
    consumer of a keyed link would have to handle `key() == None`.
12. **One key table per link.** Overlapping `KeyId`s on a record with two
    keyed links (§3.2).
13. **Reserving the key table's full capacity.** 67 KB for 1,024 keys up
    front; lazy growth measured the same per message.
14. **An index over pattern routes.** Not needed at the measured cost;
    revisit with a benchmark.

## 9. Open questions

None.

## 10. Acceptance criteria

1. Core: `TopicPattern::parse` accepts and rejects the §5.3 syntax;
   `ExactGrammar` routers unchanged. `MqttGrammar`: the §4.7 cases;
   captures at first, middle and last level; `{name..}` matching zero
   levels.
2. `build()` rejects each §5.3 build-time error with the record key;
   `inbound_router` rejects each connector-build error, including a pattern
   on an `ExactGrammar` connector, an invalid resolver-returned pattern, and
   a resolver-returned pattern without the keyed capture (the error names
   the record and the resolved topic).
3. `MqttGrammar::covers`: `sensors/+/temp` covers `sensors/kitchen/temp`;
   `a/#` covers `a` and `a/+/b`; unrelated filters do not cover. Hidden
   levels: `#` and `+/x` do not cover `$SYS/x`; `$SYS/#` covers `$SYS/x`;
   `#` covers `+/x`; `sensors/+` covers `sensors/$x`. Core, with a stub
   grammar: `subscriptions()` drops covered filters and keeps unrelated
   ones.
4. Parity test, both backends against one broker: a pattern link beside a
   covered exact link subscribes only the covering filter; each record
   receives the message once; capture and key reach the deserializer.
5. End-to-end test: two devices on one pattern record get distinct
   `KeyId`s; `inbound_key_name` resolves them; a table of capacity 2 drops
   the third device and counts it; two keyed links on one record share keys;
   `RecordMetadata::inbound_keys` reports captures, capacity, assigned and
   dropped.
6. `b0_alloc_connector` gains `inbound_route_pattern` (0 allocs/msg),
   `inbound_route_keyed_known` (0) and `inbound_route_keyed_new` (1).
   Existing rows unchanged.
7. `weather-station-gamma` and the embedded MQTT demo build for
   `thumbv8m.main-none-eabihf` with no behaviour change.

## 11. References

- [018 — Dynamic MQTT topics](./018-M7-dynamic-mqtt-topics.md)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, shared by both MQTT backends)
- [053 — Zenoh connector](./053-zenoh-connector.md) (a second grammar)
- [054 — Zero-allocation connector boundary](./054-zero-alloc-connector-boundary.md)
- [MQTT 3.1.1 §4.7 — Topic names and topic filters](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc398718106)
- [Zenoh key expressions](https://github.com/eclipse-zenoh/roadmap/blob/main/rfcs/ALL/Key%20Expressions.md)
