# 063 — Typed record keys

**Status:** 📝 Proposed

**Goal:** One contracts crate declares each record once, and that
declaration compiles for MCU, edge and cloud. A hub states which older
contract versions it still reads, and the compiler checks that statement.
The design is done when the rolling-upgrade test in §7 passes.

**Scope:** A record is declared as a key bound to its payload type
(`Key<T>`, or a `RecordKey` derive with `contract = T`). `configure`,
`producer`, `consumer`, `transform`, join inputs, links and `aimdb-sync`
take that declaration and infer the payload type from it, so a key used
with the wrong type does not compile. Links declare which contract
versions they accept from or send to peers, and that declaration is
checked against the contract's migration chain at compile time.

**The typed key is the only way to name a record.** The string-keyed
functions are removed, not renamed: there is no `configure::<T>("name")`
and no `_dyn` variant. There are two key types: `Key<T>` for a record with
a data contract, and `Name<T>` for everything else, which is a record
without a contract or a name learned at run time (§5.2). Breaking for every call site
that names a record; §5.4 measures the conversion.

Key bindings in `aimdb.lock`, tool views and a typed `aimdb-client` are
deferred (§8).

**Related:** 015 (record ID architecture), 016 (`RecordKey` trait), 039
(`no_std` migrations), 041 (data contracts), 055 (wildcard inbound
links). **Wire half:** [062](062-value-envelope.md). This design says
what a record *accepts*; 062 says what a value *is*. The acceptance test
(§7) needs both.

A throwaway spike tested this design together with 062, including the
rolling upgrade of §7 over both MQTT backends. §12 lists what it showed;
the corrections are already in the text above it.

---

## 1. Problem

A record's key and its payload type are supplied separately at every place
the record is used, and only meet at run time:

```rust
// aimdb-core/src/builder.rs:540
pub fn configure<T>(&mut self, key: impl RecordKey, f: impl FnOnce(&mut RecordRegistrar<'_, T>))
// aimdb-core/src/builder.rs:1202
pub fn producer<T>(&self, key: impl Into<String>) -> DbResult<Producer<T>>
```

- **Wrong types compile.** `db.producer::<Humidity>("temp.indoor")`
  compiles and fails at run time with `DbError::TypeMismatch`, through the
  downcast in `builder.rs:177`. `configure` catches a key registered with
  two types only when `build()` runs (`builder.rs:573`).
- **A key does not know its contract.** `RecordKey` (`record_id.rs:179`)
  carries the key string and a link address. The derive
  (`#[derive(RecordKey)]`, `key_prefix`, `link_address`) has nowhere to say
  "these keys carry temperature". So a contracts crate cannot be the one
  place that says what a fleet's records are.
- **The schema is optional and set as a side effect.** A record reports its
  contract to the graph and `record.list` only if something calls
  `RecordRegistrar::schema()`: in practice a `Linkable` link
  (`linkable.rs:22`, `with_schema(T::NAME, T::VERSION)`). A record with no
  link shows no contract, even when `T: SchemaType`.
- **Version compatibility is implicit.** Which old firmware a hub can still
  read follows from a `migration_chain!` somewhere and a JSON field probe
  at run time (062 §1). No link says "I accept v1 and v2", so nothing can
  check that statement or report against it.

So the promise "producers and consumers can't disagree about the shape of
the data" holds at `build()` or at run time, and only for the parts that
are checked at all. It should hold where the code is written, on every
target.

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
| R1 | Using a declared key with the wrong payload type is a compile error in `configure`, `producer`, `consumer`, `transform`, join inputs, links and `aimdb-sync`. |
| R2 | Declarations are `const`, `no_std` and allocation-free, so the same contracts crate serves MCU, edge and cloud. |
| R3 | Every record declared with a typed key reports its contract name and version to the graph and `record.list`, link or no link. |
| R4 | Several keys may share one contract (a second sensor), each its own record. |
| R5 | A link that accepts or sends an older version declares it; the declaration fails to compile if the chain does not reach it. |
| R6 | Rejections at ingress name key, link, received contract and version, and accepted range, and are counted per route (062 §7). |
| R7 | One path: no public function takes a record name without its payload type. Names learned at run time (the WASM bindings, generated configuration) and records without a contract use the same functions. |

**Non-goals:**

- Typing remote access. `aimdb-client`, the CLI and MCP address records
  by name over AimX and exchange JSON; that path is type-erased inside
  the instance and does not use the functions of this design.
- Version negotiation between peers. Links state what they accept and send;
  nothing is negotiated.
- Per-variant contracts in one key enum (§10.6).
- Everything in §8.

## 4. Declaring a record

### 4.1 Types

`SchemaType` lives in `aimdb-data-contracts`, which depends on
`aimdb-core`, not the other way round. So core's trait cannot name
`SchemaType`; it carries the contract's identity as a const, and `Key<T>`
lives in `aimdb-data-contracts` (behind its `aimdb-core` dependency), where
both are known:

```rust
// aimdb-core::record_id

/// Names a record and its payload type: what `producer` and `consumer`
/// take.
pub trait TypedName {
    type Contract: Send + Sync + Clone + Debug + 'static;
    fn name(&self) -> &str;
}

/// A typed name that can be stored: what `configure`, `transform` and
/// join inputs take.
pub trait TypedKey: TypedName + RecordKey + Copy {
    /// Contract name and version, reported as the record's schema.
    /// `None` for a record without a data contract.
    const SCHEMA: Option<(&'static str, u32)>;
}

/// A record name with its payload type and no data contract (§5.2).
/// `Name<'static, T>` is a `TypedKey`; any `Name<'a, T>` is a `TypedName`.
pub struct Name<'a, T> { /* name: &'a str, link, PhantomData<fn() -> T> */ }

impl<'a, T> Name<'a, T> {
    pub const fn new(name: &'a str) -> Self;
    pub const fn with_link(self, address: &'a str) -> Self;
}
impl<T> Name<'static, T> {
    /// For a name learned at run time; interns it.
    pub fn interned(name: &str) -> Self;
}

// aimdb-data-contracts

/// A key for a record with a data contract.
pub struct Key<T> {
    name: &'static str,
    link: Option<&'static str>,
    _contract: PhantomData<fn() -> T>,   // Copy/Send/Sync for any T
}

impl<T: SchemaType> Key<T> {
    pub const fn new(name: &'static str) -> Self;
    pub const fn with_link(self, address: &'static str) -> Self;
    /// For a name learned at run time; interns it.
    pub fn interned(name: &str) -> Self;
}

impl<T: SchemaType + …> TypedName for Key<T> { type Contract = T; /* … */ }
impl<T: SchemaType + …> TypedKey for Key<T> {
    const SCHEMA: Option<(&'static str, u32)> = Some((T::NAME, T::VERSION));
}
impl<T> RecordKey for Key<T> { /* as_str = name, link_address = link */ }
```

`TypedKey` has `TypedName` as a supertrait, not a blanket impl over it,
so that `Name` can be both: a `'static` one is a key, a shorter borrow is
only a name.

`Key<T>` and `Name<T>` are `Copy`, `const`-constructible and hold no
allocation (R2); their `Clone`, `Eq`, `Hash` and `Borrow<str>` are written
by hand so they do not require them of `T`. A contracts crate that
declares keys depends on `aimdb-core`, as one that derives `RecordKey`
already does.

`RecordKey` stays as the untyped supertrait that core stores and hashes.
It is no longer accepted by any public function on its own, and the
blanket `impl RecordKey for &'static str` is removed with the string
path.

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

The derive implements `TypedKey` as well as `RecordKey`, and it requires
the payload type: `#[record_key(contract = T)]` for a contract (the
generated `SCHEMA` names `::aimdb_data_contracts::SchemaType`, so the
crate depends on it under that name), or `#[record_key(payload = T)]` for
a type without one (§5.2). A derive with neither is an error, because an
enum that names no type cannot be used anywhere. `prefix` moves into the
same attribute and the separate `#[key_prefix]` attribute is removed.

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

The type parameter of the typed functions is the key, not the payload. So
`db.producer::<Temperature>(INDOOR_TEMP)`, a natural thing to write, does
not compile even when the type is right: the error says `Temperature` does
not implement `TypedKey`. Call sites leave the turbofish out.

```rust
// AimDbBuilder, AimDb
pub fn configure<K: TypedKey>(&mut self, key: K,
    f: impl FnOnce(&mut RecordRegistrar<'_, K::Contract>)) -> &mut Self;
pub fn producer<N: TypedName>(&self, key: N) -> DbResult<Producer<N::Contract>>;
pub fn consumer<N: TypedName>(&self, key: N) -> DbResult<Consumer<N::Contract>>;

// RecordRegistrar<T>, JoinBuilder<T>
pub fn transform<K: TypedKey, F>(&mut self, input: K, build: F) -> &mut Self
    where F: FnOnce(TransformBuilder<K::Contract, T>) -> TransformPipeline<K::Contract, T>;
pub fn input<K: TypedKey>(self, key: K) -> Self;

// aimdb-sync: AimDbHandle
pub fn producer<N: TypedName>(&self, key: N) -> SyncResult<SyncProducer<N::Contract>>;
pub fn consumer<N: TypedName>(&self, key: N) -> SyncResult<SyncConsumer<N::Contract>>;
```

```rust
// The input type comes from the key: `t` is &Temperature.
r.transform(INDOOR_TEMP, |b| b.map(|t| Some(t.celsius * 1.8 + 32.0)));
r.transform_join(|j| j.input(INDOOR_TEMP).input(OUTDOOR_TEMP).on_triggers(…));
```

- `configure` sets the record's schema from `K::SCHEMA` (R3).
  `RecordRegistrar::schema()` and the link builders' `with_schema` are
  removed from the public API: nothing else may set a schema.
- `producer`/`consumer` on a declared key can still fail, with
  `MissingConfiguration` (the key was never configured), but never with
  `TypeMismatch`.
- A bare string is not a key. `configure("temp.indoor", …)` fails with
  "`&str` is not a typed record key" and a note that points at `Key<T>`
  and `Name<T>` (`#[diagnostic::on_unimplemented]` on both traits).
- The same signatures compile on Tokio, Embassy and WASM: nothing here
  depends on the runtime.

### 5.2 `Name<T>`: everything that is not a contract key

Two kinds of record have no `Key<T>` in a contracts crate, and one type
covers both.

**A record without a contract.** A counter, a derived mean or an internal
state record has a payload type and no `SchemaType`:

```rust
pub const MEAN: Name<f32> = Name::new("temp.mean");

#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Debug)]
#[record_key(payload = u64, prefix = "count.")]
pub enum Counter { #[key = "in"] In, #[key = "out"] Out }

builder.configure(MEAN, |r| …);
let rx = db.consumer(MEAN)?;                     // Consumer<f32>
```

Its record reports no schema, and the version declarations of §6 are not
available on its links, because they require `T: MigrationChain`.

**A name learned at run time.** The WASM bindings get record names from
JavaScript, and generated configuration reads them from a file. That code
still knows the payload type, so it states both:

```rust
// Registration, once at startup. The name is interned (leaked), as
// `configure` already does today for every key.
builder.configure(Key::<Temperature>::interned(&name), |r| …);   // with a contract
builder.configure(Name::<u64>::interned(&name), |r| …);          // without

// Lookup, possibly per call. Borrowed; nothing is interned.
let tx = db.producer(Name::<Temperature>::new(&name))?;
```

Registration and lookup differ because their costs differ. A key that is
stored must be `'static`, so a run-time name is interned; that is right
once per record and wrong once per call, and the WASM bindings look a
record up on every `set` from JavaScript. The lifetime enforces it:
`configure(Name::new(&name), …)` with a borrowed `name` does not compile
("borrowed data escapes … must outlive `'static`").

A lookup by `Name` works for any record, with or without a contract,
because a lookup needs only the name and the type. It can claim the wrong
type, since nothing ties a run-time string to a declaration. That stays a
run-time `TypeMismatch`, and it is the only place one remains. For the
same reason `Name::<Humidity>::new("temp.indoor")` written out at a call
site compiles; the protection of R1 is for keys that are declared once
and used by name.

### 5.3 What a contracts crate exports

So a user learns two types and one rule:

| The record | Its key |
|---|---|
| has a data contract | `Key<T>` const, or a derive with `contract = T` |
| has none | `Name<T>` const, or a derive with `payload = T` |
| is named at run time | `Key::interned` / `Name::interned` to register, `Name::new(&name)` to look up |

### 5.4 Converting existing code

Every call site that names a record changes. The change is mechanical: a
string and a type at the call site become a key that carries both.

| Today | With this design |
|---|---|
| `b.configure::<T>("k", f)` | `b.configure(K, f)` |
| `db.producer::<T>("k")` | `db.producer(K)` |
| `r.transform::<I, _>("k", f)` | `r.transform(K, f)` |
| `j.input::<I>("k")` | `j.input(K)` |
| `handle.producer::<T>("k")` (`aimdb-sync`) | `handle.producer(K)` |

In the spike a script rewrote 463 call sites in 118 Rust files to this
form, building the key in place (`Name::<T>::new("k")`, or a spike-only
helper that wraps an existing enum key). Every workspace
crate then compiled, including the WASM adapter for
`wasm32-unknown-unknown` and the Embassy demos for
`thumbv8m.main-none-eabihf`, and the workspace's test suites passed
(§12). Three sites needed a hand edit: benches that passed an owned
`format!(…)` string to a lookup, and now borrow it. So nothing in the workspace needs a string path. The real
conversion declares each key once as a `const` and uses it at every site;
the script's in-place form is the fallback for tests that use a name
once. The existing key enums already hold one payload type each, so each
gains one attribute. Left to convert by hand: 111 sites in 26 Markdown
files outside `docs/design`.

### 5.5 Two declarations of one name

Two consts can declare `"temp.indoor"` with different contracts. That
cannot be a compile error across crates. It stays a `build()` error
(`key already registered with a different type`).

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
// aimdb-data-contracts: generated by migration_chain!, one per version
// the chain reaches, including the current one.
impl Reaches<1> for TemperatureV2 {}
impl Reaches<2> for TemperatureV2 {}

#[diagnostic::on_unimplemented(
    message = "migration chain of `{Self}` does not reach version {V}")]
pub trait Reaches<const V: u32> {}

// Extension traits: the builders are core types, so this crate cannot add
// inherent methods to them.
pub trait VersionedInboundExt<T: MigrationChain> {
    fn accepts_from<const V: u32>(self) -> Self where T: Reaches<V>;
}
pub trait VersionedOutboundExt<T: MigrationChain> {
    fn sends_as<const V: u32>(self) -> Self where T: Reaches<V>;   // downgrade on send
}
```

Both are only available when `T: MigrationChain`, and an uncovered
version fails to type-check (R5):

```text
error[E0277]: migration chain of `TemperatureV1` does not reach version 0
   |         .accepts_from::<0>()
   |                         ^ no migration step reaches this version
help: the trait `Reaches<0>` is not implemented for `TemperatureV1`
      but trait `Reaches<1>` is implemented for it
```

**Not an inline `const` assertion.** `const { assert!(V >= T::MIN_VERSION
…) }` in the method body is evaluated after monomorphization. In the spike
it passed `cargo check` and `cargo clippy`, so rust-analyzer and trybuild
(which checks) never see it; it failed `cargo build` only when the wiring was
reachable from `main`. A trait bound fails in `cargo check`, reachable or
not. The same holds for 062's `with_contract::<U>()`: the chain generates
`impl VersionOf<TemperatureV2> for TemperatureV1 {}` per step type, and the
binding requires `U: VersionOf<T>` (062 §6.1). A bound link takes its
accepted version from the binding, so `accepts_from` applies to inline
links only. Removing a step from the chain breaks
every link that relied on it, at compile time, in the hub that relied on
it.

**Default "current only" is deliberate.** Accepting every version in the
chain silently would hide old firmware that should have been upgraded. A
hub that is mid-rollout says so on the link, and the 062 counters
(`upgraded{from}`) show when the last old sender is gone.

**At ingress** the link builds the admission input from its declaration:
the record's contract (`K::Contract::NAME`, current version, locked shape)
and the accepted range. 062 §7 supplies the received contract and version,
from the inline header or from the link's static binding. A rejection
reads:

```text
temp.indoor via mqtt://sensors/indoor/temperature:
  received temperature@3, accepts temperature@1..=2
temp.indoor via mqtt://sensors/indoor/temperature:
  received contract 0x5b1c22e0@1, record carries temperature
temp.indoor via mqtt://sensors/indoor/temperature:
  received temperature@2 with shape 3be0…, locked shape 9c41…
```

A foreign contract is shown by its 32-bit ID, because the binary header
does not carry the name (062 §4). The text goes to the log, next to the
per-reason counters (R6).

## 7. Acceptance test: a rolling firmware upgrade

The design is done when this test passes, in CI, as one workspace test
plus compile-fail cases. It runs the path that does not work today: a
binary codec, two versions, two platforms' MQTT stacks.

**Setup.** A fixture contracts crate at two revisions, `contracts_n` and
`contracts_n1` (two crates under `tools/rolling-upgrade-fixture/`, the
second a copy of the first plus v2):

| | `contracts_n` (MCU firmware) | `contracts_n1` (hub) |
|---|---|---|
| `Temperature` | `TemperatureV1` | `TemperatureV2`, chain from v1 |
| `INDOOR_TEMP` | `Key<Temperature>("temp.indoor")` | same name |
| `OUTDOOR_TEMP` | — | `Key<Temperature>("temp.outdoor")` |

| | MCU side | Hub side |
|---|---|---|
| Contracts | `contracts_n` | `contracts_n1` |
| MQTT backend | `mountain-mqtt` (`Embedded`, the backend Embassy firmware uses) | `rumqttc` (`Native`, MQTT 3.1.1) |
| Codec | `Postcard` | `Postcard` |
| Carrier | inline header (062 §6) | inline header |

Both run in one process against the test broker, as
`aimdb-mqtt-connector/tests/backend_parity.rs` does today. The MCU side's
declarations and link setup are also built for `thumbv7em-none-eabihf` in
the embedded check lane, so the same code provably compiles for the
target. Both fixture crates run their lock test (062 §7.1).

**Main case.**

1. MCU: `producer(INDOOR_TEMP)` writes `TemperatureV1 { temp: 21.5 }`.
2. Hub: `link_from_key().accepts_from::<1>()` on `INDOOR_TEMP`.
3. Assert: the hub's `consumer(INDOOR_TEMP)` yields
   `TemperatureV2 { celsius: 21.5, .. }`, stamped by the hub (058).
4. Assert: `upgraded{from: 1}` = 1 on that route; no rejections.
5. Assert: `record.list` on the hub has exactly one `temp.indoor`, with
   `temperature` v2.
6. A second sensor publishes `TemperatureV2` on `OUTDOOR_TEMP`: a separate
   record with the same contract, unaffected by the first.

**Variants.**

| # | Change | Expected |
|---|---|---|
| V1 | Hub link without `accepts_from::<1>()` | value rejected: `received temperature@1, accepts temperature@2..=2` |
| V2 | Hub chain without the v1→v2 step, with `accepts_from::<1>()` | compile error (trybuild): "migration chain of `TemperatureV2` does not reach version 1" |
| V3 | A peer sends `temperature@3` | rejected: `received temperature@3, accepts temperature@1..=2` |
| V4 | A peer built from a crate where `TemperatureV2` gained a field without a bump | rejected: `shape_mismatch` with both hashes |
| V5 | A peer publishes `humidity@1` on the indoor topic | rejected: `contract_mismatch` |
| V6 | `db.producer::<Humidity>(INDOOR_TEMP)` | compile error |
| V7 | Hub `sends_as::<1>()` to a v1 consumer | the v1 consumer decodes the downgraded value |
| V8 | Same as the main case with `Cdr` | upgraded |
| V9 | MCU sends bare Postcard, no header; hub link `with_contract::<TemperatureV1>()` | upgraded: a deployed binary device is read without new firmware |
| V11 | MCU sends the inline header; hub link is bound, Postcard | rejected: `decode_error` (062 §6, whole-payload rule) |
| V10 | MCU sends bare JSON whose v1 struct has a `schema_version` field; hub link `with_contract::<TemperatureV1>()` | upgraded: the field is ordinary data of v1, and nothing probes it (062 §9) |

## 8. Deferred

None of these is needed for §7. Each is additive on top of `TypedKey`.

| Item | Note |
|---|---|
| Key bindings in `aimdb.lock` (`catalog! { keys: … }`) | The lock keeps checking shapes. A key that changes contract is caught by review, and by `contract_mismatch` at ingress. |
| Tool views: accepted range, received versions and last rejection in `record.list`, CLI and MCP | The counters exist (062 §7); the views come later. |
| Typed `aimdb-client` (`client.get(INDOOR_TEMP)`) | |

## 9. What breaks

Nothing here has a deprecation period or an alias. The old forms are
removed in the release that adds the new ones.

| Item | Change |
|---|---|
| `AimDbBuilder::configure`, `AimDb::producer`/`consumer` | take a typed key; the string forms are removed |
| `RecordRegistrar::transform`, `JoinBuilder::input` | take a typed key; the input type parameter is removed |
| `aimdb-sync` `producer`/`consumer` | take a typed key |
| `#[derive(RecordKey)]` | requires `#[record_key(contract = T)]` or `(payload = T)`; `#[key_prefix]` removed in favour of `prefix` |
| `impl RecordKey for &'static str` | removed |
| `RecordRegistrar::schema()`, link builders' `with_schema` | removed from the public API; the key sets the schema |
| Inbound/outbound link builders | `accepts_from`, `sends_as` (new) |
| Graph, `record.list` | schema always set for records with a contract |

### 9.1 Phases

1. `TypedKey`, `TypedName` and `Name` in core; `Key<T>` in
   data-contracts; the derive attributes; typed `configure`, `producer`,
   `consumer`, `transform`, join `input`, and `aimdb-sync`. The string
   forms are removed in the same change, and the workspace, examples and
   README are converted with it.
2. `accepts_from`/`sends_as` on the link builders. Needs the
   `MigrationChain` of 062 §7.2 and its admission procedure.
3. The rolling-upgrade fixture (§7), gated in CI: the main case first,
   then the variants.

## 10. Alternatives considered

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
7. **Typed entry points under new names** (`configure_key`), keeping the
   string forms where they are. Not breaking, but the checked path would
   be the longer name and the unchecked one the default. Rejected.
8. **Keep the string forms as `configure_dyn`, `producer_dyn`,
   `consumer_dyn`** for tools and run-time names. This was the first
   draft. The spike showed that nothing needs them: tools reach records
   over AimX, not through these functions, and the one real run-time
   caller (the WASM bindings) knows the payload type and can say so in a
   `Name`. Keeping them would leave a second, unchecked way to do
   everything, and tests and examples would stay on it. Rejected.
9. **One `Key<T>` in core for records with and without a contract.** One
   type to learn, but core cannot see `SchemaType`, so it could not set
   the schema from `T`, and a contract record declared through it would
   silently report none. Rejected: `Key<T>` requires a contract,
   `Name<T>` says there is none.
10. **Three types: `Key<T>`, `PlainKey<T>` for records without a
    contract, and a borrowed `Name<T>` for run-time lookups.** The spike
    built this first. It works, but a plain key and a run-time name carry
    the same information (a name and a type, no contract) and differ only
    in lifetime. Rejected: one `Name<'a, T>`, where `'static` makes it a
    key.

## 11. Open questions

1. `accepts_from::<V>` uses a const generic, which reads well but cannot
   come from configuration. Is a runtime `accepts_from(v)` with a
   build-time check also needed (for generated configs)?
2. Should the derive's `link_address` and `Key::with_link` converge on one
   attribute that also carries the link's codec and carrier, making the
   contracts crate the single place a fleet's links are declared?

## 12. Spike

A throwaway branch implemented the typed keys (`TypedKey`, `TypedName`,
`Key<T>`, `Name<T>`), the derive attributes, the typed
`configure`, `producer`, `consumer`, `transform`, join `input` and
`aimdb-sync` handles, the link declarations as extension traits, and the
062 envelope and admission. The string-keyed functions were made private
to their crates and every call site in the workspace was converted
(§5.4). It ran the main case of §7 and its variants. Fixture contracts
were two modules of one crate, not two crates, and the chain was written
by hand as `migration_chain!` would generate it. Not done in the spike:
removing `impl RecordKey for &'static str`, `RecordRegistrar::schema()`
and `#[key_prefix]`, the derive error for a missing payload type, and the
Markdown.

| Claim | Result |
|---|---|
| `TypedKey` in core with `Contract: SchemaType` | does not compile: core does not depend on `aimdb-data-contracts`. `SCHEMA` const in core, `Key<T>` in data-contracts (§4.1) |
| `Key<T>` const, `Copy`, no allocation | holds; 32 bytes on a 64-bit host |
| `#[record_key(contract = T, prefix = …)]` and `(payload = T, …)` | hold; both generate `TypedKey` |
| `configure`/`producer`/`consumer` infer `T` from the key; wrong type is a compile error (V6) | holds |
| `transform` and join `input` infer the input type from the key | hold; a `map` closure needs no type annotation |
| A bare string is not a key | holds; compile error on `configure("…")` and `producer("…")` |
| No public string path is needed anywhere in the workspace | holds: 463 call sites in 118 files converted by a script; every crate compiles, including WASM and the Embassy demos; no test fails in the core, data-contracts, Tokio adapter, MQTT, `aimdb-sync`, WASM, persistence, WebSocket, KNX, Zenoh and UDS suites (§5.4) |
| One `Name<'a, T>` as both a key (`'static`) and a borrowed lookup | holds; needs `TypedName` as a supertrait of `TypedKey`, because a blanket impl would overlap. A first version with a separate `PlainKey<T>` also worked (§10.10) |
| A name learned at run time: `interned` to register, a borrowed `Name` to look up | holds; a `Name` with the wrong type is a run-time error (§5.2) |
| A borrowed `Name` cannot register a record | holds; borrow-check error, caught by `cargo check` and trybuild |
| A record without a contract reports no schema | holds |
| `producer` of an unconfigured key returns an error, not `TypeMismatch` | holds |
| Schema set with no link (R3), and with a link: one record, `temperature` v2 | holds |
| Two consts, one name, two contracts → `build()` error (§5.5) | holds |
| `accepts_from` as an inherent method on the link builders | not possible from data-contracts; extension traits (§6) |
| Inline `const` assertion makes an uncovered version a compile error (V2) | only in `cargo build`, only when reachable; `cargo check` and `cargo clippy` pass, and trybuild reports the case as compiling. Replaced by a `Reaches<V>` bound, which trybuild catches (§6) |
| Main case: v1 MCU (`mountain-mqtt`, Postcard, inline) read by v2 hub (`rumqttc`) | holds; `upgraded{from: 1}` counted. The fake broker does not route, so the MCU's frame was captured and replayed to the hub |
| V1 current-only link rejects v1 | holds, `too_old`; the text is `received temperature@1, accepts temperature@2..=2` |
| V3, V4, V5 | hold over MQTT into the hub; the frames were built by the test, not sent by a second database |
| V7 `sends_as::<1>()` | holds over MQTT |
| V8 CDR, V9 bare Postcard with a static binding | hold over MQTT into the hub |
| A static binding to another contract | compile error through `VersionOf` (trybuild) |
| V10 bare JSON with `schema_version`, bound to v1 | upgraded, in the admission function, not over MQTT |
| V11 inline frame on a bound Postcard link | admitted with a wrong value; the whole-payload rule that rejects it is not implemented (062 §6) |
| Same declarations on `thumbv7em-none-eabihf` | the fixture contracts, keys and admission build |

Findings that changed this document: where `TypedKey` and `Key<T>` live
(§4.1); extension traits and a trait bound instead of a `const` assertion
(§6); the turbofish (§5.1); one `Name<T>` for records without a contract
and for run-time names, borrowed for lookups because a stored key is
interned (§5.2); the conversion measured (§5.4).
