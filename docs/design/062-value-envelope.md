# 062 — Value envelope

**Status:** 📝 Proposed

**Goal:** A v1 device on a binary codec (Postcard, CDR) is read by a v2 hub
on another platform, with the version and shape checked on arrival. That
is impossible today, and it is the acceptance test of
[063 §7](063-typed-record-keys.md).

**Scope:** One logical envelope for every record value that crosses a
link. The envelope carries the value's contract identity (name, version,
shape) and an opaque payload in the link's codec. This design defines one
binary header for byte carriers, and a static binding for links that
cannot carry a header. It also removes the schema version from inside the
payload, so migration (`migration_chain!`) works with every codec and
needs no probe parse. Breaking for `migration_chain!`, `MigrationChain`,
contract structs that carry `schema_version`, and the connector SPI.

The header format reserves room for write metadata (058, 060), because a
wire format is permanent for a deployed fleet. Carrying that metadata, the
AimX, MQTT 5 and Zenoh header carriers, persistence and tool views are
deferred (§8).

**Related:** 039 (`no_std` migrations), 041 (data contracts), 045
(per-link codecs), 054 (zero-allocation connector boundary), `aimdb.lock`
(`aimdb-data-contracts/src/lock.rs`).
**Declaration half:** [063](063-typed-record-keys.md). 063 says what a
record accepts (typed keys, per-link accepted versions); this design says
what a value is, so the receiver can compare the two at ingress.

A throwaway spike tested this design together with 063, through the
rolling upgrade of 063 §7 over both MQTT backends. §13 lists what it
showed; the corrections are already in the text above it.

---

## 1. Problem

AimDB promises typed contracts that run on every device and platform, and
evolution without a flag day. Both depend on one question that a receiver
must answer for every value: **which contract, in which version and
shape, is this?** Today the answer is incomplete:

- **The version lives inside the user's struct.** Contracts carry
  `pub schema_version: u32` next to `SchemaType::VERSION`. The two can
  disagree; every `up`/`down` step must set the field by hand
  (`migratable.rs` example); a contract cannot evolve unless its author
  remembered to add the field to v1.
- **Migration only works for JSON.** `migration_chain!` takes
  `version_field = "schema_version"` and generates a `__VersionProbe`
  that `serde_json::from_slice`s the payload to read the version, then
  parses the same bytes a second time into the matching type
  (`aimdb-derive/src/migration_chain.rs:254-280`). A Postcard or CDR link
  (045) has no field names to probe, so a v1 MCU on a Postcard link
  cannot be upgraded on arrival. The codecs an MCU wants are the ones
  that cannot evolve.
- **The shape check never leaves the build.** `aimdb.lock` hashes each
  `NAME@VERSION`'s wire shape and fails CI on a silent change, but the hash
  is not available at run time and is not sent. A peer built from an older
  or modified contracts crate is not detected; a decode either fails with a
  serde error or, worse, succeeds on a compatible-looking layout.

The common root: there is no stable, codec-independent structure around a
value at the boundary.

Three related gaps have the same root and are not closed here (§8): write
metadata gets a new carrier per design (058, 060), persisted history does
not record its version, and tools cannot see which version a link
actually receives.

## 2. The rule

> Every record value that crosses a link travels as an **envelope**: a
> contract reference and an opaque payload. The envelope is the same
> logical object on every link; only its **carrier** differs.

> The contract version lives in the envelope and nowhere else. A payload is
> exactly the serialization of the contract struct for that version.

> A receiver can identify, version-check and reject a value without
> decoding its payload.

## 3. Requirements

| # | Requirement |
|---|---|
| R1 | Codec-independent: the header is the same for JSON, Postcard and CDR payloads, and is parsed without parsing the payload. |
| R2 | `no_std`, zero allocation: parse borrows from the input; encode writes into caller storage, so 054's scratch-buffer outbound path keeps its allocation targets. |
| R3 | Room to grow: the header format has optional fields and extensions that an old receiver skips, so later metadata needs no format change. |
| R4 | Compact: the minimal header fits in 8 bytes, so serial links and MCUs pay little. |
| R5 | Foreign consumers and deployed devices are not broken: a link carries a header only when it is configured to. Otherwise the payload stays bare and the envelope is implied by link configuration. |
| R6 | One decision procedure: a single inbound function decides accept, upgrade or reject for every carrier, and every rejection is counted per route. |
| R7 | Self-locating: a captured byte string can be recognised as an envelope. |

**Non-goals:**

- Transport framing. Lengths, COBS, MQTT packets and WebSocket frames stay
  where they are; the envelope begins where `LinkCodec::decode` begins
  today and ends at the end of the frame.
- Authentication or encryption of values.
- Negotiating versions between peers. The envelope makes the version
  visible; choosing what to send to an older peer stays link configuration
  (§7.3).
- Everything in §8.

## 4. The logical envelope

```rust
// aimdb-core::envelope (no_std, no alloc)

/// What a value claims to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContractRef {
    /// FNV-1a 32 of `SchemaType::NAME`.
    name_id: u32,
    version: u32,
    /// First 8 bytes of the `aimdb.lock` SHA-256 for `NAME@VERSION`,
    /// read as a big-endian `u64` (the first 16 hex digits in the lock).
    shape: Option<u64>,
}

/// The payload's codec, as already recorded per link (045 `WireFormat`).
#[non_exhaustive]
pub enum PayloadCodec { Unspecified, Json, Postcard, Cdr }

/// A borrowed, parsed envelope.
pub struct Envelope<'a> {
    contract: ContractRef,
    codec: PayloadCodec,
    payload: &'a [u8],
}

impl<'a> Envelope<'a> {
    /// Parses a binary header (§5). Skips fields this version does not read.
    pub fn parse(frame: &'a [u8]) -> Result<Self, EnvelopeError>;
    /// Builds the envelope of a bare payload from a link's binding (§6.1).
    pub fn bound(contract: ContractRef, codec: PayloadCodec, payload: &'a [u8]) -> Self;
    pub fn contract(&self) -> ContractRef;
    pub fn codec(&self) -> PayloadCodec;
    pub fn payload(&self) -> &'a [u8];
}

/// Writes a header into caller storage; the codec writes the payload after it.
pub struct HeaderWriter<'b> { /* ... */ }
impl HeaderWriter<'_> {
    pub fn new(out: &mut [u8]) -> HeaderWriter<'_>;
    pub fn contract(self, c: ContractRef) -> Self;
    pub fn codec(self, c: PayloadCodec) -> Self;
    /// Returns the header length; the payload starts at that offset.
    pub fn finish(self) -> Result<usize, EnvelopeError>;
}
```

Fields are private behind accessors for the same reason `WriteMetadata`'s
are (058): a later field (stamp, trace) adds an accessor and breaks no
caller.

`ContractRef` replaces the ad-hoc pairs in use today: `ContractId { name,
version }` in `catalog.rs` gains `name_id()` and `shape()` and converts into
it; `RecordMetadata::schema_version` is filled from it.

**`name_id` is 32 bits.** It is unique within one catalog, and `catalog!`'s
check fails on a collision. A fleet uses one contracts crate (README
step 1), so one catalog sees every name. The ID is not unique across
unrelated fleets and does not need to be. Collisions between ordinary
words exist (`costarring` and `liquid` share an ID), so the check is
needed; it is a `const` assertion over the catalog's names and fails
`cargo check`.

## 5. Binary header

Used where a carrier is a plain byte string.

```text
offset  size  field
0       1     magic+format   0xA1  (high nibble 0xA = AimDB, low nibble = header format 1)
1       1     flags          bit 0  shape present
                             bit 1  stamp present
                             bit 2  extensions present
                             bit 3  reserved, 0
                             bits 4-7  payload codec (0 unspecified, 1 JSON, 2 Postcard, 3 CDR)
2       4     name_id        u32 LE
6       1-5   version        LEB128 u32
..      8     shape          u64 LE           (if bit 0)
..      1-10  stamp          LEB128 u64 ns    (if bit 1)
..      1-2   ext_len        LEB128, ≤ 255    (if bit 2)
..      n     extensions     TLV*: tag LEB128 u16, len LEB128, bytes
..      rest  payload        to end of frame
```

| Header | Bytes |
|---|---|
| contract only (version < 128) | 7 |
| + shape | 15 |

The whole layout is fixed now. This design writes only the contract and
the shape: a sender never sets bit 1 or bit 2. A receiver parses and skips
a stamp and extensions if present, so a later sender that adds them is
still read by firmware built from this design.

**Rules:**

- `0xA1` cannot start a JSON document or a CDR encapsulation header
  (`0x00`/`0x01`), but it can start a Postcard payload (a varint first field
  ≥ 128). So a frame is never *guessed* to be an envelope: a link is either
  configured as enveloped or not (§6).
  The magic exists so captures can be recognised (R7) and so a
  misconfigured peer is rejected with `NotEnvelope` instead of a serde
  error.
- A different low nibble means a header format this receiver cannot read:
  `UnsupportedFormat`, counted, value dropped. Format 1 is expected to last;
  growth goes into extensions.
- Unknown flag bits or an unknown codec value → `UnsupportedFormat`.
  Unknown extension tags → skipped. That is the whole compatibility
  contract.
- A frame that ends inside the header, or a varint longer than its field
  allows, is malformed. A varint with redundant continuation bytes is
  accepted; senders write the shortest form.
- Extensions are bounded at 255 bytes so an outbound route can size its
  scratch buffer statically (054 §4.7).
- Extension tag 0 is reserved. Tag 1 is reserved for the trace context of
  060. Tags 2–63 are reserved for AimDB. No tag is defined by this design.

## 6. Carriers

The envelope is always present as a logical object at the connector
boundary. A link chooses one of two carriers when it is registered:

| Link setting | Carrier | Payload on the wire |
|---|---|---|
| default | static binding (§6.1) | bare, as today |
| `envelope = "inline"` | binary header (§5) at the start of the payload | header + payload |

Inline is available on every link whose payload is an opaque byte string
to the transport. The first one is MQTT, on both backends (`rumqttc`,
MQTT 3.1.1, and `mountain-mqtt`), because it needs nothing from the
protocol version. Both ends of a link must be configured the same way. What a mismatch
does depends on its direction:

| Sender | Receiver | Result |
|---|---|---|
| bare | inline | rejected, `malformed` (no magic) |
| inline | bound, CDR or JSON | rejected, `decode_error` (the header is not a CDR encapsulation or JSON) |
| inline | bound, Postcard | **decoded as garbage** unless the rule below applies |

Postcard has no framing of its own, and `postcard::from_bytes` ignores
bytes after the value. In the spike a bound Postcard link read the first
four header bytes of an inline frame as the `f32` of `TemperatureV1` and
admitted it. So a bound link requires the decoder to consume the whole
payload: `PayloadDecoder` reports the bytes it used, and `admit` rejects a
remainder as `decode_error`. This catches the mismatch whenever the
header and payload together are not a valid value of the bound type on
their own, which is every case but a contrived one. The built-in
`LinkCodec` for Postcard keeps ignoring trailing bytes outside `admit`.

Neither carrier needs anything from the connector. Inbound, the link's
deserializer receives the payload bytes it receives today, parses the
header or builds the bound envelope, and runs `admit`. Outbound, the
link's scratch serializer writes the header and then the payload. In the
spike, MQTT inline ran on both backends with no change to
`aimdb-mqtt-connector` (§13). The connector SPI changes only for carriers
that put the envelope in transport metadata (MQTT 5 properties, Zenoh
attachments), and those are deferred (§8).

Links read by software that is not AimDB (`ros2://`, KNX, MQTT
dashboards) stay on the default, and nothing they send or receive changes
(R5).

### 6.1 Static binding

When a link carries no header, the link configuration *is* the envelope:

```rust
registrar.linked_to_with("ros2://cell/reading", Cdr::<128>)
    .with_contract::<ReadingV2>();      // contract@version this link carries
```

Inbound, the connector builds the envelope from the binding (contract,
version, codec) with `Envelope::bound` and passes it through the same
decision procedure as any other. Without `with_contract`, the binding
defaults to the record's own `T::NAME@T::VERSION`, which is today's
implicit behaviour. A static binding names one version, so a link that
must read v1 and v2 senders at the same time needs the inline header.

The bound type must be a version in the record's chain. The chain
generates `impl VersionOf<ReadingV2> for ReadingV1 {}` for each of its
types, and `with_contract::<U>()` requires `U: VersionOf<T>`, so binding a
link to another contract fails to type-check (063 §6 explains why this is
a trait bound and not a `const` assertion). The binding is also the
link's accepted version: `with_contract::<ReadingV1>()` needs no
`accepts_from::<1>()`, and the two are not combined.

Static binding is also how a deployed v1 device on a binary codec becomes
readable without new firmware: the hub binds its link to
`with_contract::<ReadingV1>()` and upgrades on arrival (§7).

## 7. Receiving: one decision procedure

```rust
pub enum Admit<T> { Value(Stamped<T>), Reject(Rejection) }

pub fn admit<T: MigrationChain>(env: &Envelope, link: &LinkContract) -> Admit<T>;
```

`LinkContract` is the record's contract and the link's accepted version
range, `accepted_min..=T::VERSION` (063 §6). The default range is the
current version only.

| Condition | Result | Counter |
|---|---|---|
| header not an envelope, or cut short | reject | `malformed` |
| header format, flag or codec this build cannot read | reject | `unsupported_format` |
| `name_id` ≠ the record's contract | reject | `contract_mismatch` |
| `version > T::VERSION` | reject (no downgrade on ingest) | `too_new` |
| `version < accepted_min` | reject | `too_old` |
| shape present and ≠ the locked shape of *that version* | reject | `shape_mismatch` |
| codec in the header, and ≠ the link's codec | reject | `codec_mismatch` |
| `version == T::VERSION` | decode payload as `T` | |
| `accepted_min ≤ version < T::VERSION` | decode as that version's type, walk `up` | `upgraded{from}` |
| payload decode fails, or leaves bytes on a bound link (§6) | reject | `decode_error` |

The rows are checked in this order. Every counter is per route.
Rejections never panic and never stop the route. The ingested value is
stamped locally (058).

The shape is checked for every accepted version, not only the current
one: a v1 device built from a changed `TemperatureV1` is as wrong as a v2
one. The chain supplies the locked shape per version (§7.2). A header
that names a codec other than the link's is rejected before the decode,
because a decode error would hide the cause; a header with codec 0
(unspecified) is decoded with the link's codec. An unreadable header
format has its own counter, so "a newer peer" is not reported as
"garbage".

**Inbound routes have no counters today.** Outbound routes have
`RouteStats`; an inbound deserializer returns `Result<T, String>`, and the
router only logs the string (`RouterEvent::IngestFailed`). So phase 3
adds inbound route stats in core, and the deserializer's error type gains
a reason the router can count (`IngestError { reason: Rejection, .. }`
instead of `String`). The spike kept the counters in the link closure,
which tests can read but tools cannot.

**`shape_mismatch` is the payoff of the lock.** It catches the case the lock
exists for: a peer built from a contracts crate where someone changed
`Temperature@2` without bumping the version. Today that peer is discovered
when a dashboard goes quiet; with the envelope it shows up as a named
counter on the first value.

### 7.1 Where the shape hash comes from

`aimdb.lock` already holds a hash per `NAME@VERSION`. `catalog!` will
`include_str!` the lock file and resolve each contract's hash in a `const
fn` (a byte loop over the file, no allocation), emitting:

```rust
impl ContractShape for Temperature { const SHAPE: Option<u64> = Some(0x9c41_e2d0_0000_0a1f); }
```

A contract outside a catalog, or a version missing from the lock, has
`SHAPE = None`; the envelope then carries no shape and the check is
skipped.

**The lock file must exist.** `include_str!` of a missing file is a
compile error, and the lock is written by the crate's own tests, which
must compile first. So `catalog!` requires an `aimdb.lock`, and a new
contracts crate starts from a seed that holds only the header
(`version = 1`). The first `AIMDB_UPDATE_LOCK=1 cargo test` fills it.
Cargo tracks the included file, so updating the lock rebuilds the
contracts crate. A version added to the code but not yet to the lock gets
`None`, and the lock test fails in CI until it is added.

**The hash sent is the locked one, not one computed from the compiled
struct.** It is true only if the lock test ran against the code that was
built. A build that skips the test can send a stale hash and pass the
check on the other side. So every firmware and hub build runs its
contracts crate's lock test in CI. The embedded check lane and the
rolling-upgrade fixture (063 §7) do, and the usage guide states it as a
requirement of the shape check.

### 7.2 Migration without a probe

The version comes from the envelope, so the chain stops reading it from the
payload:

```rust
pub trait MigrationChain: SchemaType + Sized {
    const MIN_VERSION: u32;
    /// The locked shape of `version`, for every version in the chain.
    fn shape_of(version: u32) -> Option<u64>;
    /// Decode `payload`, written as `version`, and upgrade to `Self`.
    fn migrate_from<D: PayloadDecoder>(version: u32, payload: &[u8], d: &D)
        -> Result<Self, MigrationError>;
    /// Downgrade to `version` and encode with `e` into `out`.
    fn migrate_into<E: PayloadEncoder>(&self, version: u32, e: &E, out: &mut [u8])
        -> Result<usize, MigrationError>;
}
```

`PayloadDecoder`/`PayloadEncoder` are the generic (per-type) halves of the
built-in `LinkCodec`s (JSON, Postcard, CDR), so every codec migrates. The
macro loses `version_field`:

```rust
migration_chain! {
    type Current = Sensor;
    steps { SensorV1ToV2: SensorV1 => Sensor }
}
```

One decode per value, of the right type: the probe parse disappears, and
039 PR 3 (a `Value`-free probe for small heaps) is no longer needed.

### 7.3 Sending to an older peer

An outbound link that declares an older contract version
(`.sends_as::<1>()`, 063 §6) encodes with
`migrate_into(1, …)` and writes `version = 1` in the header, or sends a
bare v1 payload on a statically bound link. The choice is made once at
registration; there is no per-message lookup (045's rule).

The header of a downgraded value carries the shape of the version sent,
from `shape_of` (§7.2), so the older peer can check it.

`MigrationStep::down` takes the newer value by value, so a downgraded
send clones the current value first: one allocation per value for
contracts with heap fields, on a path 054 keeps allocation-free. A
`down_ref(&Newer)` is a later option.

The scratch buffer of an inline outbound link must hold the header too:
the codec's `N` plus up to 19 bytes (no stamp, no extensions).

## 8. Deferred

None of these is needed to read a v1 device from a v2 hub. Each builds on
the envelope without changing the header format.

| Item | Note |
|---|---|
| Stamp and trace context in the header | Bits 1 and 2 and extension tag 1 are reserved (§5). 058 and 060 keep their own carriers until then. |
| AimX carrier (a `c` field on values) | AimX links stay statically bound. |
| MQTT 5 user-property carrier | Needs MQTT 5 on the `rumqttc` backend, which speaks 3.1.1 today. Inline covers MQTT. |
| Zenoh attachment carrier | 060's attachment format is not changed by this design. |
| Persistence columns for contract and version | History stays as it is. |
| Tool views (versions seen per route, `aimdb decode`) | The counters of §7 are available through the existing metrics. |
| Origin instance and build revision extensions | Tags stay reserved. |
| Application-defined extensions | Tags 64 and above stay unassigned. |

## 9. What breaks

Nothing here has a deprecation period, an alias or a fallback. The old
forms are removed in the release that adds the new ones.

| Item | Change |
|---|---|
| `migration_chain!` | `version_field` removed; using it is a compile error that names this section |
| `MigrationChain` | `migrate_from_bytes`/`migrate_to_version` replaced by `migrate_from`/`migrate_into`; `shape_of` added |
| The JSON version probe (`__VersionProbe`, 039) | removed |
| Contract structs | `schema_version` removed from every contract and example in the workspace. A field of that name in a user's struct is ordinary data |
| Inbound deserializer error | `String` becomes an error with a countable reason (§7) |
| Link builders | `envelope = "inline"` and `with_contract` (new) |

Nothing changes on the wire for a link that does not opt in.

**Deployed fleets.** Devices in the field send bare JSON with an in-band
`schema_version` and no envelope. There is no probe for them. They are
read like any other deployed device, through a static binding (§6.1):

```rust
r.link_from("mqtt://sensors/old/temperature")
    .with_link_codec(Json)
    .with_contract::<TemperatureV1>();
```

The hub's `TemperatureV1` does not need the `schema_version` field: serde
ignores a JSON field the struct does not declare, and the spike read
`{"schema_version":1,"temp":21.5}` this way. What is lost is one case:
bare JSON senders of *several* versions on one topic. Nothing tells them
apart without the probe. Such a fleet gives each version its own topic,
or moves its new firmware to the inline header and binds the old topic
to the old version.

### 9.1 Phases

1. **Core types.** `aimdb-core::envelope`: `ContractRef`, `Envelope`,
   `HeaderWriter`; parse skips stamp and extensions; property tests; no
   wire change.
2. **Identity.** `name_id`, `ContractShape` via `catalog!`; catalog check
   fails on a `name_id` collision.
3. **Admission.** `admit` and its counters; inbound route stats in core
   and a countable ingest error (§7); new `MigrationChain` with
   `PayloadDecoder`/`Encoder`; the JSON probe and `version_field` are
   removed in the same change.
4. **Carriers.** `with_contract` static binding; inline header on MQTT,
   both backends. Both run inside the link's codec closures; no connector
   change (§6).
5. **Proof.** The rolling-upgrade test of 063 §7 passes. `schema_version`
   fields are removed from every contract and example in the workspace.

## 10. Tests

- Round trip: contract with and without shape, every codec; parse of
  `HeaderWriter` output is identity.
- Fuzz `Envelope::parse` (cargo-fuzz target): no panic, no read past the
  frame, bounded work for any input.
- Forward compatibility: a hand-built header with a stamp and with unknown
  extension tags parses, and the value is admitted; unknown flag bits or
  format nibble are rejected as `UnsupportedFormat`.
- Admission table (§7): one test per row, counters checked.
- Carrier mismatch (§6): each row of the table, including an inline frame
  on a bound Postcard link.
- Shape: a contract changed under the same `NAME@VERSION` in a second
  contracts crate is rejected as `shape_mismatch` by a peer built from the
  first.
- Migration: v1 Postcard and v1 CDR payloads upgrade to v2 (impossible
  today), through the inline header and through a static binding; v2 → v1
  downgrade on an outbound link.
- Allocation gate: the 054 outbound and inbound zero-allocation benches stay
  at zero with headers on; `b0_alloc_migration` shows one decode per value.
- `thumbv7em-none-eabihf` check lane builds `envelope` and `admit`; code
  size of the envelope module recorded in the PR.
- Deployed JSON: a bare JSON v1 payload that still carries
  `schema_version` is upgraded through a static binding, with no probe.
- Foreign: a `ros2://` publication and a KNX telegram are unchanged byte for
  byte.

## 11. Alternatives considered

1. **Keep the version in the payload.** It is what exists. It ties
   migration to JSON, needs a second parse, and makes every contract author
   responsible for a field the framework depends on. Rejected.
2. **Self-describing payloads (CBOR, 046).** Solves field naming, not
   identity: a CBOR map still does not say which contract or version it is,
   and costs bytes on every value. Orthogonal; it can be a `PayloadCodec`.
3. **Always wrap the payload.** Simplest to implement, but breaks every
   non-AimDB consumer and every deployed device. Rejected: inline is opt-in
   per link, static binding is the default.
4. **Negotiate per link, no per-value header.** Fails during exactly the
   situation the product exists for: a rollout, where v1 and v2 producers
   publish on one MQTT topic at the same time. Rejected.
5. **Full contract name in the binary header.** 10–30 bytes per value for
   something a 4-byte ID identifies within a catalog. Rejected.
6. **Random or registry-assigned contract IDs.** Need coordination or
   storage. A hash of `NAME` is stable, free, and collisions are checked
   where all names are known: the catalog.
7. **A 64-bit `name_id`.** Makes collisions across unrelated fleets
   unlikely, at 4 more bytes per value. Rejected: one fleet has one
   catalog, and the catalog check is exact.
8. **A header with only the contract, no reserved fields.** Smaller to
   specify. But the next metadata field would need a second header format
   on firmware that is already deployed. Rejected: reserve now, implement
   later (§8).

## 12. Open questions

1. Should the shape hash be on by default for inline links (8 bytes per
   value), or opt-in per link? Proposed: on; `without_shape()` per link.
2. Which carrier do AimX links (serial, TCP, UDS, WebSocket) get when one
   must carry several versions at once: the deferred `c` field, or the
   inline header in front of the value's bytes? Not needed for the
   acceptance test.
3. Should the inline header be the default carrier, with the static
   binding as the opt-out? This design keeps bare payloads as the default
   (§6, R5). Everything else in 062 and 063 makes the new form the only
   form, so the question is whether the wire should follow. Not decided.

   | | Default stays bare (this design) | Default becomes inline |
   |---|---|---|
   | Existing AimDB links | bytes unchanged | bytes change on every link at the upgrade; both ends must move together |
   | Links read by other software (`ros2://`, KNX, dashboards) | unchanged | each must opt out, or the consumer breaks |
   | Deployed devices that cannot be reflashed | read through a static binding | the same, but the binding is now the exception to write down |
   | Version and shape checked on arrival | only on links that opt in | on every AimDB link |
   | Several versions on one topic | only on links that opt in | everywhere |
   | A forgotten setting | a link silently carries no version | a foreign consumer silently receives a header |
   | Cost per value | none | 7 bytes, or 15 with the shape |

   What the spike adds: the inline carrier needs no connector change and
   costs about 1.3 KB of code on Cortex-M (§13), so nothing technical
   forces either answer. A carrier mismatch is detected in every
   direction once the whole-payload rule of §6 is in place. A middle
   option is to choose the default by scheme: inline for links between
   AimDB instances, bare for schemes that name a foreign protocol. That
   needs a way to tell the two apart on MQTT, where both use `mqtt://`.

## 13. Spike

A throwaway branch implemented the envelope module in `aimdb-core`,
admission, probe-free migration, the shape lookup and the 063 link
declarations in a separate crate, and the rolling upgrade of 063 §7 over
MQTT. A second pass reviewed the code against this document, which found
the last four findings below. It did not implement the `migration_chain!`
changes (the chain was written by hand, as the macro would generate it),
the whole-payload rule of §6, inbound route stats in core, the alloc
benches, cargo-fuzz or the AimX, MQTT 5 and Zenoh
carriers.

| Claim | Result |
|---|---|
| Header sizes: contract only 7 bytes, with shape 15 | hold; the largest without stamp or extensions is 19 (version ≥ 2²⁸) |
| Round trip for every codec, version 0 to `u32::MAX`, with and without shape | holds |
| A stamp and unknown extension tags are skipped; unknown flag bit, codec or format nibble → `UnsupportedFormat` | holds |
| `parse` never panics or reads past the frame | holds for 2 million pseudo-random frames (a unit test in place of the fuzz target) |
| `no_std`, no allocation, builds for `thumbv7em-none-eabihf` | holds; `parse` 788 B, `HeaderWriter::finish` 264 B, LEB128 256 B in a release build |
| `0xA1` can start a Postcard payload | confirmed: a `u32` first field of `0xA1` encodes as `A1 01` |
| `name_id` collisions are caught where all names are known | holds as a `const` assertion; `costarring` and `liquid` collide |
| Shape hash read from `aimdb.lock` in a `const fn` | holds; a byte loop over the file. A missing lock file fails to compile (§7.1) |
| v1 → v2 upgrade with no probe, on Postcard, CDR and JSON, inline and statically bound | holds |
| Admission table: every row of §7 except the remainder check | holds |
| Inline header over MQTT, MCU on `mountain-mqtt`, hub on `rumqttc` | holds; 19-byte frame (15 header + 4 payload), upgraded on the hub, no connector change (§6) |
| Over MQTT into the hub: `too_new`, `too_old`, `shape_mismatch`, `contract_mismatch`; upgrade with CDR; bare Postcard on a link bound to v1 | hold |
| `sends_as::<1>()` over MQTT; the v1 side decodes it | holds; the header carries v1's locked shape (§7.3) |
| A carrier mismatch is rejected, not mis-decoded | bare → inline: `malformed`. Inline → bound CDR: `decode_error`. Inline → bound Postcard: **admitted with a wrong value** (§6) |
| Bare JSON from old firmware, with `schema_version` in the payload, on a link bound to v1 | upgraded with no probe (§9) |
| Per-route rejection counters through the existing metrics | does not hold: inbound routes have none (§7) |
| `PayloadDecoder` as the generic half of the built-in codecs | holds; a 15-line delegation to `LinkCodec<U>`. A user codec that implements `LinkCodec<T>` for one `T` only cannot migrate |

Findings that changed this document: no connector SPI change for the two
carriers (§6, §9); inbound counters and a countable ingest error (§7); the
lock file must exist (§7.1); the hash bytes (§4); the clone on downgrade
and the scratch size (§7.3). From the review: the Postcard carrier
mismatch and the whole-payload rule (§6); the shape check for older
versions and `shape_of` (§7, §7.2, §7.3); the `codec_mismatch` and
`unsupported_format` rows and the order of the table (§7); `VersionOf`
and the binding as the accepted version (§6.1).
