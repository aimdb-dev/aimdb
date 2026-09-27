# 055 — Wildcard inbound links

**Status:** 📝 Proposed (revised after review, 2026-09-27)

**Scope:** inbound links whose topic is a pattern: matching in
`aimdb-core`'s router, the matched topic and captures passed to a
match-aware deserializer, optional per-link key interning, and the MQTT
grammar in `aimdb-mqtt-connector`. **Additive:** no existing public signature
changes; `RuntimeContext` and `IngestFn` are untouched.

**Independent of** [054](./054-zero-alloc-connector-boundary.md). This
design ships on today's interfaces; §6 describes what changes when 054 lands.

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
  first time a value is seen, with a bounded table per link.
- G4. No change to existing links, routers or connectors that do not use
  patterns.
- G5. No per-message allocation on pattern routes, and none for a known key.

**Non-goals**

- Creating records at runtime. One record per pattern is the unit.
- Outbound topic templates (`link_to("mqtt://out/{device}")`). Possible
  follow-up on 054's `TopicWriter`.
- The AimX/WebSocket wildcard grammar over record keys
  (`session/topic_match.rs`). It matches dot-separated record keys, not broker
  topics.
- Making the match visible to plain `with_deserializer` closures (§4.3).

## 3. User API

```rust
builder.configure::<Reading>("sensors.readings", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 256 })
       .link_from("mqtt://sensors/{device}/temp")
       .key("device", 1024)                        // optional (§4.4)
       .with_match_deserializer(|ctx, m, bytes| {
           let device = m.get("device").unwrap();   // &str borrowed from the topic
           let key = m.key();                       // Option<KeyId>; Some when .key(..) is set
           Reading::decode(key, bytes)
       })
       .finish();
});
```

- `{name}` matches exactly one level. `{name..}` matches the remaining levels
  (zero or more) and must be last.
- The grammar's own wildcards (`+`, `#` for MQTT) are also accepted as
  unnamed levels, for users who write filters by hand.
- `with_match_deserializer` is the only way to see the match. A pattern link
  with a plain `with_deserializer(|ctx, bytes|)` still works (every matching
  message is ingested) but cannot tell publishers apart.

## 4. Design

### 4.1 Grammar is chosen by the connector

Wildcard syntax is protocol-specific (MQTT `/` `+` `#`; Zenoh `*` `**`; KNX
none), and the router is protocol-agnostic. Core defines the grammar as a
plain data struct; connectors supply a value:

```rust
// aimdb-core
#[derive(Clone, Copy)]
pub struct TopicGrammar {
    pub separator: char,
    /// The grammar's single-level and multi-level wildcard tokens.
    pub single: &'static str,
    pub multi: &'static str,
    /// Topics that wildcards must not match (MQTT: leading `$`).
    pub hidden: fn(&str) -> bool,
}

impl TopicGrammar {
    /// No wildcards: every resource id is literal (today's behaviour).
    /// A pattern link on an EXACT connector is a configuration error.
    pub const EXACT: Self = /* … */;
}

impl AimDb {
    /// The one inbound router for `scheme`: pattern links compiled against
    /// `grammar`, key tables attached (§4.4). Both subscription and routing
    /// use this router, so they cannot disagree.
    pub fn inbound_router(&self, scheme: &str, grammar: TopicGrammar)
        -> DbResult<Router>;
}

pub fn pump_source_with(db: &AimDb, scheme: &str, src: impl Source + 'static,
                        grammar: TopicGrammar) -> DbResult<Vec<BoxFuture>>;
// pump_source(..) keeps its signature and uses TopicGrammar::EXACT.

// aimdb-mqtt-connector
pub const MQTT_GRAMMAR: TopicGrammar = /* '/', "+", "#", leading '$' hidden */;
```

The struct keeps `Router` non-generic, which is what makes this additive.
The pattern walk is one function in core, parameterised at runtime by the
struct's tokens.

`collect_inbound_routes` keeps its signature and returns exact links only;
it logs a warning for each pattern link it skips. Out-of-tree connectors that
use it keep working for exact links, and in-tree connectors move to
`inbound_router`.

### 4.2 Compilation, validation and matching

Validation happens in two places, because the grammar is only known when the
connector builds.

**At `AimDbBuilder::build()`** — the `{…}` syntax, which is the same for
every grammar. Errors name the record key and URL, like other link
configuration errors:

- a capture sharing a level with text (`sensors/dev-{id}`);
- a multi-level capture that is not last;
- two captures with the same name;
- more than 8 captures;
- `.key(name, ..)` naming a capture that the pattern does not have, or a
  capacity outside `1..=65535`.

**At connector build** (`inbound_router` / `pump_source_with`, which return
`DbResult`) — everything that needs the grammar or the connector:

- a hand-written multi-level wildcard (`#`) that is not last;
- any pattern (captures or grammar wildcards) on a connector using
  `TopicGrammar::EXACT`, e.g. KNX or the WebSocket server;
- patterns returned by a `TopicResolverFn` (018): the resolved string is
  compiled and checked exactly like a URL pattern.

Each pattern route is parsed once into levels: `Literal(Arc<str>)`,
`Capture { name, multi }` or `Wildcard { multi }`.

`Router::route` checks exact routes as today, then pattern routes by walking
`topic.split(separator)` against the compiled levels. Capture positions are
recorded as byte ranges into the topic in a fixed array; no allocation for
matching. A topic matching several routes (exact or pattern) is delivered to
each, as today.

`Router::resource_ids()` returns each pattern rendered in the grammar
(`sensors/{device}/temp` → `sensors/+/temp`).

### 4.3 The match reaches the deserializer as a borrow

`IngestFn` is `Fn(&RuntimeContext, &[u8])` and stays unchanged; exact routes
keep using it. Pattern routes use a second, internal ingest type, and
`Route` holds one or the other:

```rust
// aimdb-core (internal)
type MatchIngestFn =
    Arc<dyn Fn(&RuntimeContext, &TopicMatch<'_>, &[u8]) -> Result<(), String> + Send + Sync>;

// public
pub struct TopicMatch<'a> {
    topic: &'a str,
    spans: [(u16, u16); 8],
    names: &'a [Arc<str>],    // from the compiled route
    key: Option<KeyId>,
}

impl<'a> TopicMatch<'a> {
    pub fn topic(&self) -> &'a str;
    pub fn get(&self, name: &str) -> Option<&'a str>;
    pub fn key(&self) -> Option<KeyId>;   // Some iff the link has .key(..)
}
```

- `Router::route(&self, topic: &str, ..)` already borrows the topic for the
  whole call, so the router builds `TopicMatch` on its stack and passes a
  reference. **No per-message allocation**, and no change to
  `RuntimeContext`.
- The match cannot outlive its message: `TopicMatch` borrows the topic, so
  a closure cannot keep it.
- `with_match_deserializer(|ctx, m, bytes|)` builds a `MatchIngestFn`;
  `with_deserializer` on a pattern link builds one that ignores `m`.

### 4.4 Keys

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct KeyId(NonZeroU16);   // Option<KeyId> is 2 bytes

.key("device", 1024)            // capture name, capacity 1..=65535 (required)
```

- **Ownership.** The table belongs to the link and is created at `build()`
  as an `Arc<KeyTable>`. Every router built from the link holds a clone, so
  `KeyId`s are the same everywhere, and `db.inbound_key_name` looks there.
- The table is `HashMap<Box<str>, KeyId>` (hashbrown, created with full
  capacity) plus `Vec<Box<str>>` for the reverse direction, under one
  `spin::Mutex`. Both dependencies are already in `aimdb-core`.
- A value seen for the first time gets the next `KeyId`; storing its name is
  one allocation, once. Later messages look it up by `&str` without
  allocating.
- A `{name..}` capture can be a key; its value is the whole remainder
  (`site/{loc..}` on `site/a/b/c` → key for `a/b/c`).
- **When the table is full**, the message is dropped and a per-link counter
  increases (logged; visible in record metadata with `observability`).
  Capacity is therefore also an admission limit. A message that reaches the
  deserializer of a keyed link always has `m.key() == Some(_)`.
- Keys are never reused while the process runs.
- `db.inbound_key_name("sensors.readings", key) -> Option<Arc<str>>` resolves
  a key for display, AimX and logs.

The lock is taken once per message by the single task that runs routing for
the connector, so it is uncontended. 054's `InboundDispatch` does not remove
it by itself; moving the table to `&mut` ownership is a follow-up if
measurements show the lock.

### 4.5 MQTT specifics

- `MQTT_GRAMMAR` follows MQTT 3.1.1 §4.7: `+` one level, `#` the rest
  including the parent level (`a/#` matches `a`), topics starting with `$`
  are not matched by a leading wildcard.
- Both backends build their subscription list from
  `db.inbound_router("mqtt", MQTT_GRAMMAR)`, replacing the separate
  `RouterBuilder::from_routes(..)` calls in `native.rs` and
  `embedded/mod.rs::inbound_topics`, and switch from `pump_source` to
  `pump_source_with(.., MQTT_GRAMMAR)`.
- **Overlapping filters.** Some brokers deliver one copy per matching
  subscription, so `sensors/{d}/temp` next to `sensors/kitchen/temp` could
  ingest a message twice. The connector subscribes only the **covering set**:
  a filter is dropped from the subscription list when another filter matches
  every topic it matches. The router still fans out locally, so each record
  receives exactly one copy. Coverage is a level-by-level comparison over the
  compiled patterns. The subscription QoS of a covering filter is the highest
  QoS of the filters it covers (both backends subscribe at a fixed QoS 1
  today, so this only matters once per-link subscribe QoS is honoured).
- Outbound links reject patterns at `build()`: you cannot publish to a
  filter.

## 5. Guidance for pattern records

A pattern record is one interleaved stream: its latest value is whichever
publisher sent last. Applications that need per-publisher state keep it
downstream, for example a transform holding an array indexed by `KeyId`.
Use `SpmcRing`; `SingleLatest` would drop other publishers' values between
reads.

When the broker binds topics to credentials (e.g. an ACL allowing a client
to publish only under `sensors/<its-name>/…`), the topic is the
authenticated part of a message and the payload is the publisher's claim.
Keep them separate in the record: identity from `m.get(..)`/`m.key()`,
everything else from the payload.

## 6. Relation to 054

When 054 lands, `InboundDispatch::dispatch(topic, payload)` borrows the topic
for the whole ingest call, exactly as `Router::route` does today. Because
`TopicMatch<'a>` already borrows the topic:

- `with_match_deserializer`'s closure signature and `TopicMatch<'a>` are
  unchanged.
- `InboundDispatch::new` gains a grammar argument (or a `with_grammar`
  variant) and replaces `inbound_router` + `pump_source_with` for migrated
  connectors.
- The `TopicGrammar` struct may become a trait for inlining if the
  054 bench shows the pattern walk.

## 7. Alternatives considered

1. **Reuse `topic_matches` from `session/topic_match.rs`.** Wrong grammar:
   dot-separated with `*` for one level. Translating MQTT filters breaks on
   topics that contain dots.
2. **MQTT grammar hard-coded in core.** Makes the router protocol-aware; Zenoh
   (053) would add a second hard-coded grammar.
3. **Change `IngestFn` to take the topic.** Clean, but breaking; 054 is the
   breaking window that removes the need.
4. **Records created per new topic at runtime.** Per-publisher buffers and
   AimX addresses, but it is the post-`run()` registration problem, far
   larger than this.
5. **Positional captures only (`+` → index 0).** Indices shift when a pattern
   changes; names cost nothing at runtime.
6. **Match carried in `RuntimeContext` (`ctx.inbound_match()`).** Reaches
   plain `with_deserializer` users, but the context cannot borrow the topic,
   so it costs one allocation per message, the match can outlive the message
   through a cloned context, and it breaks when 054 turns the topic into a
   borrow. Rejected in review.
7. **Declaring grammars on the builder** so every check runs at `build()`.
   Adds a registration step for every connector; connector-build errors are
   early enough.
8. **Allowing overlapping subscriptions** and documenting duplicates, or
   **rejecting overlaps** at build. The first gives records duplicate
   messages on some brokers; the second rules out a legitimate layout.
9. **Delivering unkeyed messages when the key table is full.** Keeps
   messages, but every consumer then has to handle `key() == None` on a
   keyed link. Capacity as an admission limit is the simpler contract.

## 8. Open questions

- **`mountain-mqtt` subscriptions.** Confirm `subscribe_packet` accepts `+`
  and `#` unchanged.
- **Broker duplicate behaviour.** The covering set makes it irrelevant for
  correctness, but record Mosquitto 2.x and EMQX behaviour in the
  integration test notes so the rationale is checked.

## 9. Acceptance criteria

1. Router unit tests: the MQTT §4.7 cases above, captures at first, middle
   and last level, `{name..}` matching zero levels, exact and pattern routes
   on one topic both delivering, `TopicGrammar::EXACT` routers unchanged.
2. `build()` rejects each `{…}` error in §4.2 with the record key;
   `inbound_router` rejects each connector-build error in §4.2, including a
   pattern on an `EXACT` connector and an invalid resolver-returned pattern.
3. Covering-set unit tests: `sensors/+/temp` covers `sensors/kitchen/temp`;
   `a/#` covers `a` and `a/+/b`; unrelated filters are all kept.
4. Tokio integration test against a local Mosquitto: two clients publish to
   `sensors/a/temp` and `sensors/b/temp`; one record receives both; the
   deserializer sees `device = a` and `b`; distinct `KeyId`s;
   `db.inbound_key_name` returns the names; a table of capacity 1 drops the
   second device and counts it. An exact link on `sensors/a/temp` next to the
   pattern link receives each message once, and the pattern record once.
5. `b0_alloc_connector` gains `inbound_route_pattern` (0 allocs/msg),
   `inbound_route_keyed_known` (0 allocs/msg) and `inbound_route_keyed_new`
   (1 alloc, the key name). Existing rows unchanged.
6. `weather-station-gamma` and the embedded MQTT demo build for
   `thumbv7em-none-eabihf` with no behaviour change.

## 10. References

- [018 — Dynamic MQTT topics](./018-M7-dynamic-mqtt-topics.md)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, shared by both MQTT backends)
- [053 — Zenoh connector](./053-zenoh-connector.md) (a second grammar)
- [054 — Zero-allocation connector boundary](./054-zero-alloc-connector-boundary.md)
- [MQTT 3.1.1 §4.7 — Topic names and topic filters](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc398718106)
