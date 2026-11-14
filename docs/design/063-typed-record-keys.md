# 063 — Typed record keys

**Status:** 📝 Proposed

**Scope:** A record is declared once, as a key bound to its data contract
(`Key<T>`, or a `RecordKey` derive with `contract = T`). That declaration is
what every reference to the record uses: `configure`, `producer`,
`consumer`, transforms, joins, links, `aimdb-sync`, and the
graph/CLI/MCP. The payload type is inferred from the key, so a key used with
the wrong type does not compile. Links declare which contract versions they
accept from or send to peers, and that declaration is checked against the
contract's migration chain at compile time. `aimdb.lock` adds the key →
contract binding to the shape it already locks. Breaking for the builder's
string-keyed entry points, which are renamed `*_dyn` and kept for tools.

**Related:** 015 (record ID architecture), 016 (`RecordKey` trait), 020 and
027 (transforms), 039 (`no_std` migrations), 041 (data contracts), 055
(wildcard inbound links), `aimdb.lock`
(`aimdb-data-contracts/src/lock.rs`). **Wire half:**
[062](062-value-envelope.md). This design says what a record *accepts*; 062
says what a value *is*. The acceptance test (§9) needs both.

---

## 1. Problem

A record's key and its payload type are supplied separately at every place
the record is used, and only meet at run time:

```rust
// aimdb-core/src/builder.rs:540
pub fn configure<T>(&mut self, key: impl RecordKey, f: impl FnOnce(&mut RecordRegistrar<'_, T>))
// aimdb-core/src/builder.rs:1202
pub fn producer<T>(&self, key: impl Into<String>) -> DbResult<Producer<T>>
// aimdb-sync/src/handle.rs:357
pub fn producer<T>(&self, key: impl AsRef<str>) -> SyncResult<SyncProducer<T>>
```

- **Wrong types compile.** `db.producer::<Humidity>("temp.indoor")`
  compiles and fails at run time with `DbError::TypeMismatch`, through the
  downcast in `builder.rs:177`. `configure` catches a key registered with
  two types only when `build()` runs (`builder.rs:573`).
- **A key does not know its contract.** `RecordKey` (`record_id.rs:179`)
  carries the key string and a link address. The derive
  (`#[derive(RecordKey)]`, `key_prefix`, `link_address`) has nowhere to say
  "these keys carry temperature".
- **The schema is optional and set as a side effect.** A record reports its
  contract to the graph and `record.list` only if something calls
  `RecordRegistrar::schema()`: in practice a `Linkable` link
  (`linkable.rs:22`, `with_schema(T::NAME, T::VERSION)`). A record with no
  link shows no contract in the CLI or MCP, even when `T: SchemaType`.
- **Transform inputs are strings.** `transform::<I, _>(input_key, …)` and
  `JoinBuilder::input::<I>(key)` repeat the input type next to the key; the
  pair is checked at `build()` (`transform_input_types`).
- **The lock checks shapes, not bindings.** `aimdb.lock` guarantees that
  `temperature@2` keeps its wire shape. Nothing guarantees that
  `temp.indoor` keeps carrying `temperature`, or that its version only
  advances along the migration chain.
- **Version compatibility is implicit.** Which old firmware a hub can still
  read follows from a `migration_chain!` somewhere and a JSON field probe
  at run time (062 §1). No link says "I accept v1 and v2", so nothing can
  check that statement or report against it.

So the promise "producers and consumers can't disagree about the shape of
the data" holds at `build()` or at run time, and only for the parts that
are checked at all. It should hold where the code is written.

## 2. The rule

> A record is declared once: a key name bound to the contract type this
> build speaks. Every reference to the record uses that declaration, and
> the payload type is inferred from it.

> Record identity is the key name. The contract version is a property of a
> build, not of the record: `temp.indoor` stays `temp.indoor` when its
> firmware moves from `temperature@1` to `temperature@2`.

> Older versions are accepted only where a link declares them, and the
> declaration compiles only if the migration chain covers it. At ingress,
> anything else is rejected with the key, the link, the received version
> and the accepted range.

## 3. Requirements

| # | Requirement |
|---|---|
| R1 | Using a key with the wrong payload type is a compile error in `configure`, `producer`, `consumer`, `transform`, `join`, links and `aimdb-sync`. |
| R2 | Declarations are `const`, `no_std` and allocation-free, so the same contracts crate serves MCU, edge and cloud. |
| R3 | Every record declared with a typed key reports its contract name and version to the graph, `record.list`, CLI and MCP, link or no link. |
| R4 | Several keys may share one contract (a second sensor), each its own record. |
| R5 | A link that accepts or sends an older version declares it; the declaration fails to compile if the chain does not reach it. |
| R6 | Rejections at ingress name key, link, received `NAME@VERSION` and accepted range, and are counted per route (062 §7). |
| R7 | `aimdb.lock` fails when a key changes contract, or moves to a version its chain does not connect to the locked one. |
| R8 | Dynamic access (CLI, MCP, `aimdb-client`, wildcard links) keeps working through an explicit, separately named string path. |

**Non-goals:**

- Typing remote access. `aimdb-client` stays dynamic (JSON); a typed
  client over `Key<T>` is a natural follow-up (§12).
- Version negotiation between peers. Links state what they accept and send;
  nothing is negotiated.
- Per-variant contracts in one key enum (§11.6).

## 4. Declaring a record

### 4.1 Types

```rust
// aimdb-core::record_id

/// A record key bound to the contract type `T` this build speaks.
pub trait TypedKey: RecordKey + Copy {
    type Contract: SchemaType + Send + Sync + Clone + Debug + 'static;
}

/// The single-record form: a const.
pub struct Key<T> {
    name: &'static str,
    link: Option<&'static str>,
    _contract: PhantomData<fn() -> T>,   // Copy/Send/Sync for any T
}

impl<T: SchemaType> Key<T> {
    pub const fn new(name: &'static str) -> Self;
    pub const fn with_link(self, address: &'static str) -> Self;
}

impl<T: SchemaType + …> TypedKey for Key<T> { type Contract = T; }
impl<T> RecordKey for Key<T> { /* as_str = name, link_address = link */ }
```

`Key<T>` is `Copy`, `const`-constructible and holds no allocation (R2). It
implements `RecordKey`, so everything that takes an untyped key today
accepts it.

### 4.2 In a contracts crate

```rust
// One alias names the version this revision of the crate speaks.
pub type Temperature = TemperatureV2;

pub const INDOOR_TEMP: Key<Temperature> =
    Key::new("temp.indoor").with_link("mqtt://sensors/indoor/temperature");
pub const OUTDOOR_TEMP: Key<Temperature> =
    Key::new("temp.outdoor").with_link("mqtt://sensors/outdoor/temperature");

// The grouped form: one enum, one contract, many records.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Debug)]
#[record_key(contract = Temperature, prefix = "station.")]
pub enum StationTemp {
    #[key = "alpha"] #[link_address = "mqtt://stations/alpha/temperature"] Alpha,
    #[key = "beta"]  #[link_address = "mqtt://stations/beta/temperature"]  Beta,
}
```

`#[record_key(contract = …)]` makes the derive implement `TypedKey` as
well as `RecordKey`. The existing `#[key_prefix]` attribute stays as an
alias for `prefix`. A derive without `contract` keeps implementing only
`RecordKey` and works with the `_dyn` path (§5.3), with a deprecation
warning.

The type parameter is the *current* version for this build. When the crate
moves to v3, `Temperature` points at `TemperatureV3`. Every call site that
reads a changed field then fails to compile on its own. The key name does
not change, so the record's identity does not change.

## 5. Using a record

### 5.1 Builder and handles

```rust
builder.configure(INDOOR_TEMP, |r| {             // r: RecordRegistrar<'_, Temperature>
    r.buffer(BufferCfg::SingleLatest)
     .link_from_key()                            // address from the key
     .finish();
});

let tx = db.producer(INDOOR_TEMP)?;              // Producer<Temperature>
let rx = db.consumer(StationTemp::Alpha)?;       // Consumer<Temperature>
```

```rust
pub fn configure<K: TypedKey>(&mut self, key: K,
    f: impl FnOnce(&mut RecordRegistrar<'_, K::Contract>)) -> &mut Self;
pub fn producer<K: TypedKey>(&self, key: K) -> DbResult<Producer<K::Contract>>;
pub fn consumer<K: TypedKey>(&self, key: K) -> DbResult<Consumer<K::Contract>>;
```

- `configure` sets the record's schema from `K::Contract` unconditionally
  (R3). `RecordRegistrar::schema()` remains for `_dyn` records only.
- `producer`/`consumer` can still fail, with `MissingConfiguration` (the
  key was never configured), but never with `TypeMismatch`.
- `aimdb-sync`'s `producer`/`consumer` take `K: TypedKey` the same way.

### 5.2 Transforms and joins

```rust
builder.configure(COMFORT, |r| {
    r.transform(INDOOR_TEMP, |b| b.map(|t| Comfort::from(t)));   // I inferred
});
builder.configure(DEW_POINT, |r| {
    r.transform_join(|j| j.input(INDOOR_TEMP).input(INDOOR_HUMIDITY).on(…));
});
```

The input type comes from the input key; `transform_input_types` and its
build-time check stay as the backstop for the `_dyn` path only.

### 5.3 The dynamic path

```rust
builder.configure_dyn::<Temperature>("temp.lab", |r| …);
db.producer_dyn::<Temperature>("temp.lab")?;
```

These are today's functions, renamed. They exist for code that learns keys
at run time: tools, wildcard inbound links (055), generated configuration.
They keep today's checks: a type conflict fails `build()` and a mismatch
returns `TypeMismatch`. Examples and docs never use them for declared
records.

### 5.4 Two declarations of one name

Two consts can declare `"temp.indoor"` with different contracts. That
cannot be a compile error across crates. It stays a `build()` error
(`key already registered with a different type`), and becomes a CI error
through the lock (§7), which sees every key in the catalog.

## 6. Links and versions

A link states which versions it exchanges with its peer. The default is
the current version only.

```rust
builder.configure(INDOOR_TEMP, |r| {
    r.link_from_key().accepts_from::<1>().finish();      // v1 and v2 firmware
    r.link_to("mqtt://legacy-dash/indoor").sends_as::<1>().finish();
});
```

```rust
impl<T: MigrationChain> InboundLinkBuilder<'_, T> {
    pub fn accepts_from<const V: u32>(self) -> Self {
        const { assert!(V >= T::MIN_VERSION && V <= T::VERSION,
                        "migration chain does not reach this version") };
        /* record accepted range V..=T::VERSION on the link */
    }
}
impl<T: MigrationChain> OutboundLinkBuilder<'_, T> {
    pub fn sends_as<const V: u32>(self) -> Self { /* same assert; downgrade on send */ }
}
```

Both are only available when `T: MigrationChain`, and the inline `const`
assertion (stable since 1.79; this workspace pins 1.98) turns an uncovered
version into a compile error (R5). Removing a step from the chain breaks
every link that relied on it, at compile time, in the hub that relied on
it.

**Default "current only" is deliberate.** Accepting every version in the
chain silently would hide old firmware that should have been upgraded. A
hub that is mid-rollout says so on the link, and the 062 counters
(`upgraded{from}`) show when the last old sender is gone.

**At ingress** the link builds the admission input from its declaration:
the record's contract (`K::Contract::NAME`, current version, locked shape)
and the accepted range. 062 §7 supplies the received `NAME@VERSION`, from
the envelope or from the link's static binding. A rejection reads:

```text
temp.indoor via mqtt://sensors/indoor/temperature:
  received temperature@3, accepts temperature@1..=2
temp.indoor via mqtt://sensors/indoor/temperature:
  received humidity@1, record carries temperature
temp.indoor via serial://mcu0:
  received temperature@2 with shape 3be0…, locked shape 9c41…
```

The same text is in the route's last-rejection slot, readable from the CLI,
MCP and the probe (059), next to the per-reason counters (R6).

## 7. The lock binds keys

`catalog!` lists keys next to contracts:

```rust
aimdb_data_contracts::catalog! {
    contracts: [TemperatureV1, TemperatureV2, Humidity],
    keys: [INDOOR_TEMP, OUTDOOR_TEMP, StationTemp],   // enums expand to every variant
}
```

`aimdb.lock` gains one line per key:

```text
key temp.indoor     temperature 2
key temp.outdoor    temperature 2
key station.alpha   temperature 2
```

| Change | Result |
|---|---|
| New key | fails until accepted with `AIMDB_UPDATE_LOCK=1` |
| Key removed | fails until accepted |
| Key moves to another contract name | always fails |
| Key moves from version `a` to `b`, chain of `b` reaches `a` | fails until accepted (an upgrade) |
| Key moves to a version whose chain does not reach the locked one | always fails |
| Two keys with one name | always fails |

"Always fails" means the change must be a new key: a record whose meaning
changed is a new record, so a process engineer's trend line does not
silently switch signals. Moving `temp.indoor` from v2 to v3 with a chain is
an ordinary, accepted update.

## 8. What tools see

Because every typed record carries its contract from `configure` (R3):

- `graph.nodes` and `record.list` report `schema_type` and
  `schema_version` for every typed record. `RecordMetadata::schema_version`
  is always set for typed records.
- Each inbound link reports its accepted range and, from 062, the versions
  actually received and the reason counts.
- `aimdb record show temp.indoor` and the MCP record tools show one record
  with its contract, its links, what each link accepts and receives, and the
  last rejection, if any. "Where did compatibility break" is answered by one
  record view, not a log search.

## 9. Acceptance test: a rolling firmware upgrade

The design is done when this test passes, in CI, as one workspace test
plus compile-fail cases.

**Setup.** A fixture contracts crate at two revisions, `contracts_n` and
`contracts_n1` (two crates under `tools/rolling-upgrade-fixture/`, the
second a copy of the first plus v2):

| | `contracts_n` (MCU firmware) | `contracts_n1` (hub) |
|---|---|---|
| `Temperature` | `TemperatureV1` | `TemperatureV2`, chain from v1 |
| `INDOOR_TEMP` | `Key<Temperature>("temp.indoor")` | same name |
| `OUTDOOR_TEMP` | — | `Key<Temperature>("temp.outdoor")` |

The MCU side runs on Tokio in the test and is built for
`thumbv7em-none-eabihf` in the embedded check lane, so the same
declaration provably compiles there. Transport: the in-process MQTT test
broker the connector tests already use, with the 062 carrier for MQTT 5.

**Main case.**

1. MCU: `producer(INDOOR_TEMP)` writes `TemperatureV1 { temp: 21.5 }`.
2. Hub: `link_from_key().accepts_from::<1>()` on `INDOOR_TEMP`.
3. Assert: the hub's `consumer(INDOOR_TEMP)` yields
   `TemperatureV2 { celsius: 21.5, .. }`, stamped by the hub (058).
4. Assert: `upgraded{from: 1}` = 1 on that route; no rejections.
5. Assert: `record.list` on the hub has exactly one `temp.indoor`, with
   `temperature` v2, link accepts `1..=2`, received `{1}`.
6. A second sensor publishes `TemperatureV2` on `OUTDOOR_TEMP`: a separate
   record with the same contract, unaffected by the first.

**Variants.**

| # | Change | Expected |
|---|---|---|
| V1 | Hub link without `accepts_from::<1>()` | value rejected: `received temperature@1, accepts temperature@2..=2` |
| V2 | Hub chain without the v1→v2 step, with `accepts_from::<1>()` | compile error (trybuild): "migration chain does not reach this version" |
| V3 | A peer sends `temperature@3` | rejected: `received temperature@3, accepts temperature@1..=2` |
| V4 | A peer built from a crate where `TemperatureV2` gained a field without a bump | rejected: `shape_mismatch` with both hashes |
| V5 | A peer publishes `humidity@1` on the indoor topic | rejected: `received humidity@1, record carries temperature` |
| V6 | `db.producer::<Humidity>(INDOOR_TEMP)` | compile error |
| V7 | `contracts_n1` rebinds `temp.indoor` to `Humidity` | lock test fails: key moved to another contract |
| V8 | Hub `sends_as::<1>()` to a v1 dashboard | the dashboard's v1 consumer decodes the downgraded value |

## 10. What breaks

| Item | Change |
|---|---|
| `AimDbBuilder::configure`, `AimDb::producer`/`consumer` | take `K: TypedKey`; string forms renamed `configure_dyn`, `producer_dyn`, `consumer_dyn` |
| `transform`, `JoinBuilder::input` | take `K: TypedKey`; `transform_dyn`/`input_dyn` keep the explicit type |
| `aimdb-sync` `producer`/`consumer` | as above |
| `#[derive(RecordKey)]` | new `#[record_key(contract = …, prefix = …)]`; without `contract`, deprecation warning |
| `catalog!` | new optional `keys:` list; lock format gains `key` lines (lock `FORMAT_VERSION` bump) |
| Inbound/outbound link builders | `accepts_from`, `sends_as` (additive) |
| Graph, `record.list` | schema always set for typed records (additive) |

For one minor release the old names remain as deprecated aliases of the
`_dyn` functions, so downstream code keeps compiling with warnings.

### 10.1 Phases

1. `TypedKey`, `Key<T>`, the derive attribute; typed `configure`,
   `producer`, `consumer` next to the existing functions; schema set from
   the key.
2. Typed `transform`/`join`/`aimdb-sync`; rename the string forms to
   `_dyn` with deprecated aliases; convert examples and the README.
3. `accepts_from`/`sends_as` on the link builders. Needs the
   `MigrationChain` of 062 §7.2 and its admission procedure.
4. `catalog! { keys: … }` and the lock rules.
5. Tools: accepted range, received versions and last rejection in
   `record.list`, CLI and MCP.
6. The rolling-upgrade fixture (§9), gated in CI from phase 3 onward,
   with variants added as each phase lands.

## 11. Alternatives considered

1. **Keep types at call sites and add a lint.** A lint cannot see that
   `"temp.indoor"` was configured as `Temperature` in another crate.
   Rejected.
2. **Make the type the identity (`TypeId` → record).** Breaks R4: two
   sensors with one contract would be one record. This is why keys exist
   (015). Rejected.
3. **Put the version in the key name** (`temp.indoor.v2`). Every firmware
   upgrade becomes a new record, so history, dashboards and transforms
   lose the signal. Rejected: identity is the name, version is per build.
4. **Key typed by contract family** (`Key<TemperatureFamily>` with an
   associated current type). Adds a type per contract to express what the
   `Temperature` alias already expresses. Rejected for now; revisit if
   aliases prove error-prone.
5. **Accept every chained version by default.** Silent acceptance hides
   stragglers in a fleet. Rejected: opt-in per link, counted (§6).
6. **Per-variant contracts in one key enum.** The derive would have to
   generate a type per variant, and `TypedKey::Contract` could no longer
   be a single associated type. Rejected: one enum per contract, or
   separate consts.
7. **A `record!` declaration DSL** that generates key, link and contract
   in one block. Possible sugar on top of `Key<T>`; not needed for the
   guarantees here.

## 12. Open questions

1. Should a typed `aimdb-client` (`client.get(INDOOR_TEMP) ->
   Temperature`) be part of this design or a follow-up? It would extend R1
   across the network for Rust clients.
2. `accepts_from::<V>` uses a const generic, which reads well but cannot
   come from configuration. Is a runtime `accepts_from(v)` with a
   build-time check also needed (for generated configs)?
3. Should `catalog!` require every `Key` in the crate to be listed, through
   a test that walks the crate's public consts, so a forgotten key cannot
   escape the lock?
4. Should the derive's `link_address` and `Key::with_link` converge on one
   attribute that also carries the link's codec, making the contracts crate
   the single place a fleet's links are declared (README "centrally
   managed")?
