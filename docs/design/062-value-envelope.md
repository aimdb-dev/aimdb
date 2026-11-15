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
    /// Low 64 bits of the `aimdb.lock` shape hash for `NAME@VERSION`.
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
unrelated fleets and does not need to be.

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
- Unknown flag bits set → `UnsupportedFormat`. Unknown extension tags →
  skipped. That is the whole compatibility contract.
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
protocol version. Both ends of a link must be configured the same way; a
mismatch is rejected as `malformed` (§7), not mis-decoded.

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
| `name_id` ≠ the record's contract | reject | `contract_mismatch` |
| `version == T::VERSION`, shape absent or equal to the locked shape | decode payload as `T` | |
| `version == T::VERSION`, shape differs | reject | `shape_mismatch` |
| `accepted_min ≤ version < T::VERSION` | decode as that version's type, walk `up` | `upgraded{from}` |
| `version > T::VERSION` | reject (no downgrade on ingest) | `too_new` |
| `version < accepted_min` | reject | `too_old` |
| payload decode fails | reject | `decode_error` |
| header unparseable / not an envelope | reject | `malformed` |

Every counter is per route, readable through the existing buffer and route
metrics (013). Rejections never panic and never stop the route. The
ingested value is stamped locally (058).

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

A contract outside a catalog, or a lock not yet written, has `SHAPE =
None`; the envelope then carries no shape and the check is skipped.

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

## 9. What breaks, and the path through it

| Item | Change |
|---|---|
| `migration_chain!` | `version_field` removed (accepted and ignored with a deprecation warning for one minor) |
| `MigrationChain` | `migrate_from_bytes`/`migrate_to_version` replaced by `migrate_from`/`migrate_into` |
| Contract structs | `schema_version` field no longer needed; keeping it is harmless but it is no longer read |
| Connector SPI | inbound callbacks receive `Envelope<'_>` instead of `&[u8]`; outbound scratch writers write the header first on inline links |
| Link builders | `envelope = "inline"` and `with_contract` (additive) |

Nothing changes on the wire for a link that does not opt in.

**Deployed fleets.** Devices in the field send bare JSON with an in-band
`schema_version` and no envelope. They stay readable: a statically bound
link with no `with_contract`, on a record whose chain was generated with
the legacy feature, falls back to the 039 probe once per value. The
fallback lives behind `aimdb-data-contracts/legacy-version-probe` for one
major release, and its use is counted (`legacy_probe`), so an operator can
see when the last old device is gone.

### 9.1 Phases

1. **Core types.** `aimdb-core::envelope`: `ContractRef`, `Envelope`,
   `HeaderWriter`; parse skips stamp and extensions; property tests; no
   wire change.
2. **Identity.** `name_id`, `ContractShape` via `catalog!`; catalog check
   fails on a `name_id` collision.
3. **Admission.** `admit` and its counters; new `MigrationChain` with
   `PayloadDecoder`/`Encoder`; legacy probe behind its feature.
4. **Carriers.** `with_contract` static binding; inline header on MQTT,
   both backends; connector SPI change.
5. **Proof.** The rolling-upgrade test of 063 §7 passes. Remove
   `schema_version` fields from the examples it touches.

## 10. Tests

- Round trip: contract with and without shape, every codec; parse of
  `HeaderWriter` output is identity.
- Fuzz `Envelope::parse` (cargo-fuzz target): no panic, no read past the
  frame, bounded work for any input.
- Forward compatibility: a hand-built header with a stamp and with unknown
  extension tags parses, and the value is admitted; unknown flag bits or
  format nibble are rejected as `UnsupportedFormat`.
- Admission table (§7): one test per row, counters checked.
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
- Legacy: a bare JSON v1 payload with `schema_version` is admitted through
  the probe and counted.
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
