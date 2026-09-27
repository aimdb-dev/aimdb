# 055 — Wildcard inbound links

**Status:** 📝 Proposed

**Scope:** inbound links whose topic is a pattern: matching in
`aimdb-core`'s router, the matched topic and captures available to the
deserializer, optional per-link key interning, and the MQTT grammar in
`aimdb-mqtt-connector`. **Additive:** no existing public signature changes.

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

**Non-goals**

- Creating records at runtime. One record per pattern is the unit.
- Outbound topic templates (`link_to("mqtt://out/{device}")`). Possible
  follow-up on 054's `TopicWriter`.
- The AimX/WebSocket wildcard grammar over record keys
  (`session/topic_match.rs`). It matches dot-separated record keys, not broker
  topics.

## 3. User API

```rust
builder.configure::<Reading>("sensors.readings", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 256 })
       .link_from("mqtt://sensors/{device}/temp")
       .key("device", 1024)                       // optional (§4.4)
       .with_match_deserializer(|ctx, m, bytes| {
           let device = m.get("device").unwrap();  // borrowed &str
           let key = m.key();                      // KeyId, when .key(..) is set
           Reading::decode(key, bytes)
       })
       .finish();
});
```

- `{name}` matches exactly one level. `{name..}` matches the remaining levels
  (zero or more) and must be last.
- The grammar's own wildcards (`+`, `#` for MQTT) are also accepted as
  unnamed levels, for users who write filters by hand.
- `with_deserializer(|ctx, bytes|)` keeps working on pattern links; the
  match is available through `ctx.inbound_match()` (§4.3).

## 4. Design

### 4.1 Grammar is chosen by the connector

Wildcard syntax is protocol-specific (MQTT `/` `+` `#`; Zenoh `*` `**`; KNX
none), and the router is protocol-agnostic. Core defines the grammar
interface; connectors supply an implementation:

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
    pub const EXACT: Self = /* … */;
}

impl RouterBuilder {
    pub fn with_grammar(self, grammar: TopicGrammar) -> Self;
}

pub fn pump_source_with(db: &AimDb, scheme: &str, src: impl Source + 'static,
                        grammar: TopicGrammar) -> Vec<BoxFuture>;
// pump_source(..) == pump_source_with(.., TopicGrammar::EXACT)

// aimdb-mqtt-connector
pub const MQTT_GRAMMAR: TopicGrammar = /* '/', "+", "#", leading '$' hidden */;
```

A plain data struct keeps `Router` non-generic, which is what makes this
additive. The pattern walk itself is generic code in core; the struct only
supplies tokens.

### 4.2 Compilation and matching

At `build()` (route collection), each pattern route is parsed once into
levels: `Literal(Arc<str>)`, `Capture { name, multi }` or `Wildcard { multi }`.
Invalid patterns fail `build()` with the record key and URL, like other link
configuration errors:

- a capture sharing a level with text (`sensors/dev-{id}`);
- a multi-level capture or wildcard that is not last;
- two captures with the same name;
- more than 8 captures.

`Router::route` checks exact routes as today, then pattern routes by walking
`topic.split(separator)` against the compiled levels. Capture positions are
recorded as byte ranges into the topic in a fixed array; no allocation for
matching. A topic matching several routes (exact or pattern) is delivered to
each, as today.

`Router::resource_ids()` returns each pattern rendered in the grammar
(`sensors/{device}/temp` → `sensors/+/temp`), so connectors subscribe
unchanged.

### 4.3 The match reaches the deserializer through `RuntimeContext`

`IngestFn` is `Fn(&RuntimeContext, &[u8])`; changing it would break a public
type. Instead, `RuntimeContext` carries an optional match, set by the router
for the duration of one ingest call:

```rust
impl RuntimeContext {
    /// The inbound match being ingested, if any.
    pub fn inbound_match(&self) -> Option<&TopicMatch>;
}

pub struct TopicMatch {
    topic: Arc<str>,
    spans: [(u16, u16); 8],
    names: Arc<[Arc<str>]>,   // from the compiled route
    key: Option<KeyId>,
}

impl TopicMatch {
    pub fn topic(&self) -> &str;
    pub fn get(&self, name: &str) -> Option<&str>;
    pub fn key(&self) -> KeyId;          // panics if the link has no key
}
```

- `RuntimeContext` gains one private field; `RuntimeContext::new` is
  unchanged.
- **Cost:** for pattern routes, the topic is copied into an `Arc<str>` once
  per message, because the context cannot borrow it. That is one allocation
  per message on pattern routes only (the `b0_alloc_connector` row added by
  this design records it). Exact routes are unaffected. 054 removes this
  cost (§6).
- `with_match_deserializer(|ctx, m, bytes|)` is convenience over
  `with_deserializer` that reads `ctx.inbound_match()`.

### 4.4 Keys

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct KeyId(NonZeroU16);   // Option<KeyId> is 2 bytes

.key("device", 1024)            // capture name, capacity (required)
```

- Each keyed link owns a table: `HashMap<Box<str>, KeyId>` (hashbrown,
  created with full capacity at `build()`) and `Vec<Box<str>>` for the
  reverse direction, under one `spin::Mutex`. Both dependencies are already
  in `aimdb-core`.
- A value seen for the first time gets the next `KeyId`; storing its name is
  one allocation, once. Later messages from the same value only look it up.
- When the table is full, the message is dropped and a per-link counter
  increases (logged; visible in record metadata with `observability`).
  Capacity is therefore also an admission limit.
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
- Both backends switch from `pump_source` to `pump_source_with(..,
  MQTT_GRAMMAR)`.
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
for the whole ingest call. The router then passes a borrowed match instead of
copying the topic into `RuntimeContext`:

- `with_match_deserializer`'s closure signature is unchanged; `m` becomes a
  borrow of the dispatcher's stack, and the per-message allocation on
  pattern routes disappears.
- `ctx.inbound_match()` stays available for `with_deserializer` users, set
  from the same borrow.
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

## 8. Open questions

- **Overlapping subscriptions.** With `sensors/{d}/temp` and
  `sensors/kitchen/temp` on different records, some brokers deliver one
  message per matching subscription, so the router would ingest it twice.
  Check Mosquitto 2.x and EMQX; if needed, the MQTT connector subscribes only
  the broadest filter of an overlapping set, since the router fans out
  locally.
- **`mountain-mqtt` subscriptions.** Confirm `subscribe_packet` accepts `+`
  and `#` unchanged.
- **`TopicResolverFn`.** A resolver (018) may return a pattern; it should be
  compiled the same way. Confirm no existing resolver returns strings with
  `{`.

## 9. Acceptance criteria

1. Router unit tests: the MQTT §4.7 cases above, captures at first, middle
   and last level, `{name..}` matching zero levels, exact and pattern routes
   on one topic both delivering, `TopicGrammar::EXACT` routers unchanged.
2. `build()` rejects each invalid pattern in §4.2 with the record key.
3. Tokio integration test against a local Mosquitto: two clients publish to
   `sensors/a/temp` and `sensors/b/temp`; one record receives both; the
   deserializer sees `device = a` and `b`; distinct `KeyId`s;
   `db.inbound_key_name` returns the names; a table of capacity 1 drops the
   second device and counts it.
4. `b0_alloc_connector` gains `inbound_route_pattern` (1 alloc/msg, the
   topic copy) and `inbound_route_keyed_known` (1 alloc/msg for a known
   key). Existing rows unchanged.
5. `weather-station-gamma` and the embedded MQTT demo build for
   `thumbv7em-none-eabihf` with no behaviour change.

## 10. References

- [018 — Dynamic MQTT topics](./018-M7-dynamic-mqtt-topics.md)
- [052 — Runtime-neutral connectors](./052-runtime-neutral-connectors.md)
  (`pump_source`, shared by both MQTT backends)
- [053 — Zenoh connector](./053-zenoh-connector.md) (a second grammar)
- [054 — Zero-allocation connector boundary](./054-zero-alloc-connector-boundary.md)
- [MQTT 3.1.1 §4.7 — Topic names and topic filters](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc398718106)
