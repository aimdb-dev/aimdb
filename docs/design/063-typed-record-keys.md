# 063 — Typed record keys

**Status:** 📝 Proposed

**Goal:** One contracts crate declares each record once, and that
declaration compiles for MCU, edge and cloud. A hub states which older
contract versions it still reads, and the compiler checks that statement.
The design is done when the rolling-upgrade test in §7 passes.

**Scope:** A record is declared as a key bound to its data contract
(`Key<T>`, or a `RecordKey` derive with `contract = T`). `configure`,
`producer`, `consumer` and links take that declaration and infer the
payload type from it, so a key used with the wrong type does not compile.
Links declare which contract versions they accept from or send to peers,
and that declaration is checked against the contract's migration chain at
compile time. Breaking for the builder's string-keyed `configure`,
`producer` and `consumer`, which are renamed `*_dyn` and kept for tools.

Typed transforms, joins and `aimdb-sync`, key bindings in `aimdb.lock`,
and tool views are deferred (§8).

**Related:** 015 (record ID architecture), 016 (`RecordKey` trait), 039
(`no_std` migrations), 041 (data contracts), 055 (wildcard inbound
links). **Wire half:** [062](062-value-envelope.md). This design says
what a record *accepts*; 062 says what a value *is*. The acceptance test
(§7) needs both.

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
| R1 | Using a key with the wrong payload type is a compile error in `configure`, `producer`, `consumer` and links. |
| R2 | Declarations are `const`, `no_std` and allocation-free, so the same contracts crate serves MCU, edge and cloud. |
| R3 | Every record declared with a typed key reports its contract name and version to the graph and `record.list`, link or no link. |
| R4 | Several keys may share one contract (a second sensor), each its own record. |
| R5 | A link that accepts or sends an older version declares it; the declaration fails to compile if the chain does not reach it. |
| R6 | Rejections at ingress name key, link, received contract and version, and accepted range, and are counted per route (062 §7). |
| R7 | Dynamic access (CLI, MCP, `aimdb-client`, wildcard links) keeps working through an explicit, separately named string path. |

**Non-goals:**

- Typing remote access. `aimdb-client` stays dynamic (JSON).
- Version negotiation between peers. Links state what they accept and send;
  nothing is negotiated.
- Per-variant contracts in one key enum (§10.6).
- Everything in §8.

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
`RecordKey` and works with the `_dyn` path (§5.2).

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
- The same signatures compile on Tokio, Embassy and WASM: nothing here
  depends on the runtime.

### 5.2 The dynamic path

```rust
builder.configure_dyn::<Temperature>("temp.lab", |r| …);
db.producer_dyn::<Temperature>("temp.lab")?;
```

These are today's functions, renamed. They exist for code that learns keys
at run time: tools, wildcard inbound links (055), generated configuration.
They keep today's checks: a type conflict fails `build()` and a mismatch
returns `TypeMismatch`.

The rename is a breaking change with no alias: the typed functions take
the plain names, so the old signatures cannot stay under them. The fix at
a call site is mechanical: append `_dyn`, or switch to a typed key.

### 5.3 Two declarations of one name

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
| V2 | Hub chain without the v1→v2 step, with `accepts_from::<1>()` | compile error (trybuild): "migration chain does not reach this version" |
| V3 | A peer sends `temperature@3` | rejected: `received temperature@3, accepts temperature@1..=2` |
| V4 | A peer built from a crate where `TemperatureV2` gained a field without a bump | rejected: `shape_mismatch` with both hashes |
| V5 | A peer publishes `humidity@1` on the indoor topic | rejected: `contract_mismatch` |
| V6 | `db.producer::<Humidity>(INDOOR_TEMP)` | compile error |
| V7 | Hub `sends_as::<1>()` to a v1 consumer | the v1 consumer decodes the downgraded value |
| V8 | Same as the main case with `Cdr` | upgraded |
| V9 | MCU sends bare Postcard, no header; hub link `with_contract::<TemperatureV1>()` | upgraded: a deployed binary device is read without new firmware |
| V10 | MCU sends bare JSON with `schema_version: 1`, no header, no binding | upgraded through the legacy probe; `legacy_probe` = 1 |

## 8. Deferred

None of these is needed for §7. Each is additive on top of `TypedKey`.

| Item | Note |
|---|---|
| Typed `transform` and `JoinBuilder::input` | They keep the explicit input type and string key, checked at `build()` as today. |
| Typed `aimdb-sync` `producer`/`consumer` | Keep `impl AsRef<str>` and the run-time check. |
| Key bindings in `aimdb.lock` (`catalog! { keys: … }`) | The lock keeps checking shapes. A key that changes contract is caught by review, and by `contract_mismatch` at ingress. |
| Tool views: accepted range, received versions and last rejection in `record.list`, CLI and MCP | The counters exist (062 §7); the views come later. |
| Typed `aimdb-client` (`client.get(INDOOR_TEMP)`) | |
| Converting all examples and the README to typed keys | Only the examples the acceptance test touches are converted. The rest get the mechanical `_dyn` rename. |

## 9. What breaks

| Item | Change |
|---|---|
| `AimDbBuilder::configure`, `AimDb::producer`/`consumer` | take `K: TypedKey`; string forms renamed `configure_dyn`, `producer_dyn`, `consumer_dyn` |
| `#[derive(RecordKey)]` | new `#[record_key(contract = …, prefix = …)]` (additive) |
| Inbound/outbound link builders | `accepts_from`, `sends_as` (additive) |
| Graph, `record.list` | schema always set for typed records (additive) |

### 9.1 Phases

1. `TypedKey`, `Key<T>`, the derive attribute; typed `configure`,
   `producer`, `consumer`; string forms renamed to `_dyn`; schema set from
   the key.
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

## 11. Open questions

1. `accepts_from::<V>` uses a const generic, which reads well but cannot
   come from configuration. Is a runtime `accepts_from(v)` with a
   build-time check also needed (for generated configs)?
2. Should the derive's `link_address` and `Key::with_link` converge on one
   attribute that also carries the link's codec and carrier, making the
   contracts crate the single place a fleet's links are declared?
