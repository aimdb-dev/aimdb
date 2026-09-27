# 055 — Wildcard inbound links

**Status:** 📝 Proposed — validated by a spike (§3), 2026-09-27

**Scope:** inbound links whose topic is a pattern: matching in
`aimdb-core`'s router, the matched topic and captures passed to a
match-aware deserializer, optional per-record key interning surfaced in
record metadata, a grammar trait implemented by connectors, and the MQTT
grammar in `aimdb-mqtt-connector`. `RuntimeContext` and `IngestFn` are
untouched; §5.8 lists the two public structs that gain fields.

**Independent of** [054](./054-zero-alloc-connector-boundary.md). This
design ships on today's interfaces; §7 describes what changes when 054 lands.

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
- G4. No change to existing links, routers or connectors that do not use
  patterns.
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
   tokens and a "must be last" rule cannot say that (§5.1).
3. **Fewer checks run at `build()`.** "Capture shares a level with text"
   needs the separator, and "multi-level capture must be last" depends on
   the grammar. Both run when the connector builds (§5.3).
4. **Hand-written `+`/`#` stay broken on the old path.** Whether `+` is a
   wildcard depends on the grammar, so `collect_inbound_routes` cannot spot
   such links. Every in-tree connector moves to `inbound_router` (§5.2).
5. **Keys are per record.** With per-link tables, two keyed links on one
   record handed out overlapping `KeyId`s. One table per record fixes that
   and matches the "array indexed by `KeyId`" guidance (§5.6).
6. **Key tables grow lazily.** Reserving full capacity cost 67 KB for 1,024
   keys before any device appeared; lazy growth costs nothing measurable on
   the per-message path (§5.6).
7. **Two public structs gain fields** (§5.8), so "additive" needs a caveat.
8. **Rules the first draft left implicit:** setting both deserializers is an
   error; a literal topic with a match-aware deserializer takes the pattern
   path; the dropped counter is `AtomicU32` (thumbv7em has no 64-bit
   atomics).

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
Core owns one matcher; a grammar only answers questions about single levels:

```rust
// aimdb-core
pub enum LevelKind { Literal, Single, Multi, Invalid(&'static str) }

pub trait TopicGrammar: Send + Sync {
    /// `false`: every `{…}` link on this connector is an error.
    fn supports_patterns(&self) -> bool { true }
    fn separator(&self) -> char;
    /// What a hand-written level is (`+` → Single, `a+` → Invalid, …).
    fn classify(&self, level: &str) -> LevelKind;
    /// Tokens used to render `{name}` / `{name..}` for the subscription.
    fn single_token(&self) -> &str;
    fn multi_token(&self) -> &str;
    /// Zenoh `a/**/b`: true. MQTT `#`: false (last only).
    fn multi_anywhere(&self) -> bool { false }
    /// Whether a wildcard at level `index` may match `level`.
    /// MQTT: not a leading `$…`. Zenoh: not a verbatim `@…` chunk.
    fn wildcard_matches(&self, index: usize, level: &str) -> bool { true }
}

/// KNX, WebSocket, AimX session connectors: no wildcards.
pub struct ExactGrammar;

// aimdb-mqtt-connector
pub struct MqttGrammar;   // '/', "+", "#", `$` hidden at level 0
```

- Grammars are passed as `&'static dyn TopicGrammar` (unit structs:
  `&MqttGrammar`). `Router` stays non-generic and the per-match dynamic
  calls are one `separator()` at compile time and one
  `wildcard_matches()` per wildcard level; §3.1 shows no measurable cost.
- Features outside the trait are rejected by `classify` with a reason, e.g.
  Zenoh's `$*` sub-chunk wildcards. Supporting them later is a trait
  addition, not a change to the router.

### 5.2 One inbound router per connector

```rust
impl AimDb {
    /// Exact links as `collect_inbound_routes`, plus pattern links compiled
    /// against `grammar`, with key tables attached.
    pub fn inbound_router(&self, scheme: &str, grammar: &'static dyn TopicGrammar)
        -> DbResult<Router>;
}

#[non_exhaustive]
pub struct Subscription {
    pub filter: Arc<str>,
    /// Config of every link this filter stands for: itself, identical
    /// topics and the filters it covers (§5.7).
    pub links: Vec<Arc<[(String, String)]>>,
}

impl Router {
    /// Subscription filters with any filter covered by another removed.
    pub fn subscriptions(&self) -> Vec<Subscription>;
}

pub fn pump_source_with(db: &AimDb, scheme: &str, src: impl Source + 'static,
                        grammar: &'static dyn TopicGrammar)
    -> DbResult<Vec<BoxFuture>>;
// pump_source(..) keeps its signature and behaviour.
```

- A connector subscribes and routes with the **same** router, so the two
  cannot disagree. The spike replaced the separate
  `RouterBuilder::from_routes(..)` calls in `native.rs` and
  `embedded/mod.rs::inbound_topics`.
- The router keeps its grammar (covering needs it, §5.7) and each link's
  config, so a connector can read per-link subscribe options. Core does not
  interpret the config. A `Router` built with `Router::new` reports
  `links: []`; `Route` and `collect_inbound_routes` are unchanged.
- **Every in-tree connector moves to `inbound_router`** in the same change:
  MQTT with `&MqttGrammar`; KNX, the WebSocket server and client, and
  core's AimX session client (TCP, UDS, serial) with `&ExactGrammar`. Only then is a `{…}` link on those
  connectors an error instead of a warning, and a hand-written `+` on MQTT
  starts working.
- `collect_inbound_routes` keeps its signature for out-of-tree connectors.
  It returns exact links only and logs a warning for each `{…}` link it
  skips. A hand-written `+`/`#` link without braces still goes through it
  as an exact route: it subscribes and never matches, which is today's
  behaviour.

### 5.3 Validation

**At `AimDbBuilder::build()`**: the grammar-independent `{…}` syntax.
Errors name the record key and URL:

- unbalanced braces, empty names, names outside `[A-Za-z0-9_]`;
- two captures with the same name;
- more than 8 captures;
- `.key(name, ..)` naming a capture that the pattern does not have, a
  capacity of 0, or a capacity that differs from another keyed link on the
  same record (§5.6).

**At connector build** (`inbound_router` / `pump_source_with`, returning
`DbResult`): everything that needs the grammar:

- a capture sharing a level with text (`sensors/dev-{id}`);
- a multi-level capture or wildcard where the grammar forbids it;
- a level `classify` rejects (`a+` in MQTT, `$*` in Zenoh);
- any `{…}` on a connector whose grammar has `supports_patterns() == false`;
- patterns returned by a `TopicResolverFn` (018). They go through every
  check in this section, including the `.key(..)` capture check: the router
  looks up the key's capture slot in the resolved pattern, and a missing
  capture is an error rather than a keyed link without keys (§5.6). Both
  paths share one parser for the `{…}` syntax.

### 5.4 Matching

Each pattern route is compiled once into levels: `Literal`, `Single` or
`Multi`, each wildcard optionally carrying a capture slot.

`Router::route` checks exact routes as today, then pattern routes in
registration order. The matcher walks the topic in place, recording capture
positions as byte ranges in a fixed `[(u16, u16); 8]`. A `Multi` level that
is last takes the rest in one step; one followed by more levels (Zenoh)
backtracks over split points. A topic matching several routes is delivered
to each, as today.

Routes are scanned linearly. At 64 routes a pattern route costs about 60 ns
over an exact one (§3.1). An index can come later if a benchmark asks for it.

### 5.5 The match reaches the deserializer as a borrow

Exact routes keep `IngestFn`. Pattern routes use a second ingest type:

```rust
pub type MatchIngestFn =
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
- `with_match_deserializer` builds a `MatchIngestFn`. A `{…}` link with a
  plain `with_deserializer` gets one that ignores the match. A **literal**
  topic with `with_match_deserializer` becomes an all-literal pattern route
  so the closure still receives the topic.
- The closure takes `RuntimeContext` by value, like `with_deserializer`.
  That clones an `Arc` per message (an atomic increment, no allocation).
  Passing `&RuntimeContext` would be cheaper but is a change for both
  builders; it belongs in 054's breaking window.

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
  `Vec<Arc<str>>` for the reverse direction, under one `spin::Mutex`. Both
  dependencies are already in `aimdb-core`.
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
  `db.inbound_router("mqtt", &MqttGrammar)` and route with
  `pump_source_with(.., &MqttGrammar)`.
- **Covering set.** A filter is left out when another matches every topic
  it matches (`sensors/kitchen/temp` under `sensors/+/temp`; `a/+/b` under
  `a/#`). Required for backend parity (§3.1). The router still fans each
  message out to every route.
- A wildcard covers a literal level only where `wildcard_matches` allows
  it. `#` and `+/x` do not cover `$SYS/x`: the broker never delivers `$…`
  topics to a leading wildcard, so dropping `$SYS/x` would silence that
  link. `sensors/+` does cover `sensors/$x` (the rule is level 0 only). A
  wildcard in the covered filter is covered by one at the same level,
  because `wildcard_matches` does not depend on the wildcard kind.
- **Subscribe QoS.** Each filter is subscribed at the highest `qos` among
  its `links` (set by `with_qos`), default 1. A subscriber receives
  `min(publish, subscribe)` QoS, so every covered link gets at least what
  it asked for. The embedded backend caps at 1 (mountain-mqtt rejects QoS 2
  subscriptions) and `warn_unsupported_qos` names each inbound route that
  asks for 2, once at build.
- Outbound links reject patterns at `build()`: you cannot publish to a
  filter.

### 5.8 Compatibility

No existing function or type signature changes. Two public structs with
public fields gain fields, which breaks code that builds them as struct
literals:

- `InboundConnectorLink` gains `match_ingest_factory` and `key`. It has
  `InboundConnectorLink::new`, and no in-tree code uses a literal.
- `RecordMetadata` gains `inbound_keys`. It has `RecordMetadata::new`; the
  serde form is backward compatible.

Mark both `#[non_exhaustive]` in the same change, so later fields are not
breaking. The attribute itself also breaks struct literals and exhaustive
destructuring outside the crate, so it ships in the same breaking change as
the fields. The new `InboundKeysInfo` is `#[non_exhaustive]` from the start.

One behaviour change: inbound `with_qos` takes effect. Both backends ignored
it and subscribed at QoS 1.

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
  `inbound_router` + `pump_source_with` for migrated connectors.
- 054's breaking window is where `&RuntimeContext` can replace the by-value
  context in both deserializer builders (§5.5).

## 8. Alternatives considered

1. **Reuse `topic_matches` from `session/topic_match.rs`.** Wrong grammar:
   dot-separated with `*` for one level. Translating MQTT filters breaks on
   topics that contain dots.
2. **MQTT grammar hard-coded in core.** Makes the router protocol-aware; Zenoh
   (053) would add a second hard-coded grammar.
3. **Grammar as a data struct** (separator, tokens, a `hidden` function).
   Fits MQTT but cannot express Zenoh's mid-pattern `**` or per-level hidden
   chunks (§3.2). The trait costs nothing measurable.
4. **Change `IngestFn` to take the topic.** Clean, but breaking; 054 is the
   breaking window that removes the need.
5. **Match carried in `RuntimeContext` (`ctx.inbound_match()`).** Reaches
   plain `with_deserializer` users, but costs one allocation per message,
   lets the match outlive the message through a cloned context, and breaks
   when 054 turns the topic into a borrow.
6. **Records created per new topic at runtime.** Per-publisher buffers and
   AimX addresses, but it is the post-`run()` registration problem, far
   larger than this.
7. **Positional captures only (`+` → index 0).** Indices shift when a pattern
   changes; names cost nothing at runtime.
8. **All checks at `build()`** by declaring grammars on the builder. Adds a
   registration step for every connector; connector-build errors are early
   enough.
9. **Subscribing every filter as written.** Duplicates on MQTT 5 but not
   3.1.1 (§3.1), so the backends would disagree. **Rejecting overlaps**
   instead rules out a legitimate layout.
10. **Delivering unkeyed messages when the key table is full.** Every
    consumer of a keyed link would have to handle `key() == None`.
11. **One key table per link.** Overlapping `KeyId`s on a record with two
    keyed links (§3.2).
12. **Reserving the key table's full capacity.** 67 KB for 1,024 keys up
    front; lazy growth measured the same per message.
13. **An index over pattern routes.** Not needed at the measured cost;
    revisit with a benchmark.

## 9. Open questions

None.

## 10. Acceptance criteria

1. Matcher unit tests: the MQTT §4.7 cases; captures at first, middle and
   last level; `{name..}` matching zero levels; a Zenoh-style test grammar
   with `a/**/b`, a mid-pattern `{path..}` and verbatim `@` chunks;
   `ExactGrammar` routers unchanged.
2. `build()` rejects each §5.3 build-time error with the record key;
   `inbound_router` rejects each connector-build error, including a pattern
   on an `ExactGrammar` connector, an invalid resolver-returned pattern, and
   a resolver-returned pattern without the keyed capture (the error names
   the record and the resolved topic).
3. Covering-set unit tests: `sensors/+/temp` covers `sensors/kitchen/temp`;
   `a/#` covers `a` and `a/+/b`; `a/**/b` covers `a/*/b`; unrelated filters
   are all kept. Hidden levels: `#` and `+/x` do not cover `$SYS/x`;
   `$SYS/#` covers `$SYS/x`; `#` covers `+/x`; `sensors/+` covers
   `sensors/$x`; with the Zenoh-style grammar, `a/*` does not cover `a/@x`
   and `a/**` does not cover `a/@x/y`. `subscriptions()` groups each
   filter's link config.
4. Parity test, both backends against one broker: a pattern link beside a
   covered exact link subscribes only the covering filter; each record
   receives the message once; capture and key reach the deserializer. With
   `with_qos(2)` on the covered link, the native backend subscribes the
   covering filter at QoS 2 (granted QoS in the `SubAck`) and the embedded
   backend at QoS 1. Unit test: the highest `qos` wins, the default is 1.
5. End-to-end test: two devices on one pattern record get distinct
   `KeyId`s; `inbound_key_name` resolves them; a table of capacity 2 drops
   the third device and counts it; two keyed links on one record share keys;
   `RecordMetadata::inbound_keys` reports captures, capacity, assigned and
   dropped.
6. `b0_alloc_connector` gains `inbound_route_pattern` (0 allocs/msg),
   `inbound_route_keyed_known` (0) and `inbound_route_keyed_new` (1).
   Existing rows unchanged.
7. `weather-station-gamma` and the embedded MQTT demo build for
   `thumbv7em-none-eabihf` with no behaviour change.

## 11. References

- [018 — Dynamic MQTT topics](./018-M7-dynamic-mqtt-topics.md)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, shared by both MQTT backends)
- [053 — Zenoh connector](./053-zenoh-connector.md) (a second grammar)
- [054 — Zero-allocation connector boundary](./054-zero-alloc-connector-boundary.md)
- [MQTT 3.1.1 §4.7 — Topic names and topic filters](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc398718106)
- [Zenoh key expressions](https://github.com/eclipse-zenoh/roadmap/blob/main/rfcs/ALL/Key%20Expressions.md)
