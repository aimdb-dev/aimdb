# 062 — Value envelope

**Status:** 📝 Proposed

**Scope:** One logical envelope for every record value that leaves a
buffer: on a link, in an AimX frame, in a persisted row. The envelope
carries the value's contract identity (name, version, shape), its write
metadata (058, 060) and an opaque payload in the link's codec. It defines
one binary header for byte carriers and one mapping to each transport's
native header slot. It also removes the schema version from inside the
payload, so migration (`migration_chain!`) works with every codec and
needs no probe parse. Breaking for `migration_chain!`, `MigrationChain`,
contract structs that carry `schema_version`, and the persistence schema.
Additive on AimX (minor version) and on MQTT 5. Changes the Zenoh
attachment format proposed in 060 before 060 ships.

**Related:** 039 (`no_std` migrations), 041 (data contracts), 045
(per-link codecs), 054 (zero-allocation connector boundary), 057 (build
identity), 058 (write timestamps, `Stamped`/`WriteMetadata`), 060 (trace
context), `aimdb.lock` (`aimdb-data-contracts/src/lock.rs`).
**Declaration half:** [063](063-typed-record-keys.md). 063 says what a
record accepts (typed keys, per-link accepted versions); this design says
what a value is, so the receiver can compare the two at ingress.

---

## 1. Problem

AimDB promises typed contracts, evolution without a flag day, and one place
to see every node. All three depend on one question that a receiver must
answer for every value: **which contract, in which version and shape, is
this, and what do we know about the write that produced it?** Today the
answer is assembled in a different place on each path:

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
  (045) has no field names to probe, so a v1 MCU on a Postcard serial link
  cannot be upgraded on arrival.
- **Write metadata is attached one design at a time.** The stamp rides as
  `"ts"` in AimX (058); the trace context gets its own carrier per
  transport: an AimX field, an MQTT 5 user property, a Zenoh attachment in
  a text format of its own (060 §5). Each future field (origin instance,
  build identity from 057) would repeat that work across every connector.
- **The shape check never leaves the build.** `aimdb.lock` hashes each
  `NAME@VERSION`'s wire shape and fails CI on a silent change, but the hash
  is not available at run time and is not sent. A peer built from an older
  or modified contracts crate is not detected; a decode either fails with a
  serde error or, worse, succeeds on a compatible-looking layout.
- **History forgets its version.** `aimdb-persistence-sqlite` stores
  `record_name, value_json, stored_at`. After a contract upgrade, old rows
  can only be read by luck.
- **Tools must know the Rust types.** The CLI and MCP server learn a
  record's `schema_version` from `RecordMetadata`, i.e. from the instance's
  own configuration, not from what is actually arriving on a link. "Which
  device sends which version" is not observable.

The common root: there is no stable, codec-independent structure around a
value at the boundary. Every wire format ever shipped is permanent for a
deployed fleet, so this is the one place to get right before more metadata
and more codecs land.

## 2. The rule

> Every record value that crosses a boundary (link, AimX, persistence)
> travels as an **envelope**: a contract reference, write metadata and an
> opaque payload. The envelope is the same logical object on every path;
> only its **carrier** differs.

> The contract version lives in the envelope and nowhere else. A payload is
> exactly the serialization of the contract struct for that version.

> A receiver can identify, version-check, route and reject a value without
> decoding its payload, and without knowing its Rust type.

> Metadata never costs a value. A carrier that cannot fit the metadata
> drops metadata before it drops the value (as 060 §5 already requires for
> the trace).

## 3. Requirements

| # | Requirement |
|---|---|
| R1 | Codec-independent: the header is the same for JSON, Postcard and CDR payloads, and is parsed without parsing the payload. |
| R2 | `no_std`, zero allocation: parse borrows from the input; encode writes into caller storage, so 054's scratch-buffer outbound path keeps its allocation targets. |
| R3 | Extensible: new metadata fields are added without a format version bump, and old receivers skip what they do not know. |
| R4 | Compact: the minimal header fits in 8 bytes, so serial links and MCUs pay little. |
| R5 | Foreign consumers are not broken: where a link is read by non-AimDB software (ROS 2, KNX, MQTT dashboards), the payload stays bare and the envelope travels in a header slot or is implied by link configuration. |
| R6 | One decision procedure: a single inbound function decides accept, upgrade or reject for every carrier, and every rejection is counted per route. |
| R7 | Self-locating: a captured byte string can be recognised as an envelope and decoded by a tool that has only the contracts catalog. |

**Non-goals:**

- Transport framing. Lengths, COBS, MQTT packets and WebSocket frames stay
  where they are; the envelope begins where `LinkCodec::decode` begins
  today and ends at the end of the frame.
- Authentication or encryption of values.
- Negotiating versions between peers. The envelope makes the version
  visible; choosing what to send to an older peer stays link configuration
  (§7.3).
- Changing the stamp rule of 058: a stamp is never imported. The envelope
  carries the sender's stamp for observers only.

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
    stamp: Option<Stamp>,          // sender's stamp (058), observers only
    ext: Extensions<'a>,           // TLV iterator, unknown tags skipped
    payload: &'a [u8],
}

impl<'a> Envelope<'a> {
    pub fn parse(frame: &'a [u8]) -> Result<Self, EnvelopeError>;
    pub fn contract(&self) -> ContractRef;
    pub fn payload(&self) -> &'a [u8];
    pub fn stamp(&self) -> Option<Stamp>;
    pub fn trace(&self) -> Option<TraceContext>;   // ext tag 1, feature `trace-context`
    pub fn ext(&self) -> Extensions<'a>;
}

/// Writes a header into caller storage; the codec writes the payload after it.
pub struct HeaderWriter<'b> { /* ... */ }
impl HeaderWriter<'_> {
    pub fn new(out: &mut [u8]) -> HeaderWriter<'_>;
    pub fn contract(self, c: ContractRef) -> Self;
    pub fn codec(self, c: PayloadCodec) -> Self;
    pub fn stamp(self, s: Stamp) -> Self;
    pub fn ext(self, tag: u16, value: &[u8]) -> Result<Self, EnvelopeError>;
    /// Returns the header length; the payload starts at that offset.
    pub fn finish(self) -> Result<usize, EnvelopeError>;
}
```

Fields are private behind accessors for the same reason `WriteMetadata`'s
are (058): a later field adds an accessor and breaks no caller.

`ContractRef` replaces the ad-hoc pairs in use today: `ContractId { name,
version }` in `catalog.rs` gains `name_id()` and `shape()` and converts into
it; `RecordMetadata::schema_version` is filled from it.

## 5. Binary header

Used where a carrier is a plain byte string: framed links, the Zenoh
attachment, persistence, captures.

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
| + stamp (≈ 1 h uptime, 6–7 bytes) | 22 |
| + trace extension (tag 1, 34 bytes binary) | 58 |

For comparison, the 060 text carrier alone is 79–81 bytes on AimX.

**Rules:**

- `0xA1` cannot start a JSON document or a CDR encapsulation header
  (`0x00`/`0x01`), but it can start a Postcard payload (a varint first field
  ≥ 128). So a frame is never *guessed* to be an envelope: a link is either
  configured as enveloped or not (§6).
  The magic exists so tools and captures can recognise envelopes (R7) and so
  a misconfigured peer is rejected with `NotEnvelope` instead of a serde
  error.
- A different low nibble means a header format this receiver cannot read:
  `UnsupportedFormat`, counted, value dropped. Format 1 is expected to last;
  growth goes into extensions.
- Unknown flag bits set → `UnsupportedFormat`. Unknown extension tags →
  skipped. That is the whole compatibility contract.
- Extensions are bounded at 255 bytes so an outbound route can size its
  scratch buffer statically (054 §4.7): `ENCODE_BUFFER_CAPACITY + 64 +
  max_ext`.

**Extension tags:**

| Tag | Content | From |
|---|---|---|
| 0 | reserved | |
| 1 | trace context: 16 B trace ID, 1 B depth, 8 B span, 8 B parent, 1 B flags | 060 |
| 2 | origin instance tag (u16 LE) | 060 §4.2 |
| 3 | build revision (first 8 bytes of the 057 source revision) | 057 |
| 4–63 | reserved for AimDB | |
| 64–1023 | application-defined, registered on the builder | |

## 6. Carriers

The envelope is always present as a logical object at the connector
boundary. How it travels depends on what the transport offers:

| Link | Carrier | Payload |
|---|---|---|
| AimX over TCP, UDS, WebSocket, Serial | JSON fields on every value: `ts` (058), `trace` (060), new `c` (§6.1) | `value` as today |
| MQTT 5 | user properties: `aimdb-c` (§6.1), `aimdb-trace` (060); MQTT `content-type` = codec | bare |
| MQTT 3.1.1 | static binding, or binary header in the payload when the link sets `envelope = "inline"` | bare / enveloped |
| Zenoh `zenoh://` | binary header (§5) as the publication's attachment; the payload stays in the publication body | bare |
| Zenoh `ros2://` | static binding (attachment belongs to rmw_zenoh) | bare CDR |
| KNX | static binding (the DPT is the contract) | bare |
| Persistence | columns (§8) | bytes |
| Captures / probe dumps | binary header + payload | |

### 6.1 Text form of the contract reference

For carriers whose headers are strings (AimX JSON, MQTT 5 user
properties):

```text
<name>@<version>[#<shape: 16 hex>]
temperature@2#9c41e2d000000a1f
```

The full name is sent here, not the 32-bit ID: these carriers are not
byte-starved, and a human reading an MQTT trace or an AimX log sees the
contract by name. Receivers map it to the same `ContractRef`.

AimX example:

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,
 "c":"temperature@2#9c41e2d000000a1f","data":{"celsius":21.5}}
```

`c` is optional and additive: AimX decoders ignore unknown fields, so this
is a minor protocol version. A server sends it when the record's type has a
`SchemaType`. `record.set` from a client may send it; if it does, §7 applies
to the write.

### 6.2 Static binding

When a carrier has no room for a header, the link configuration *is* the
envelope:

```rust
registrar.linked_to_with("ros2://cell/reading", Cdr::<128>)
    .with_contract::<ReadingV2>();      // contract@version this link carries
```

Inbound, the connector builds the envelope from the binding (contract,
version, codec) and passes it through the same decision procedure as any
other. Without `with_contract`, the binding defaults to the record's own
`T::NAME@T::VERSION`, which is today's implicit behaviour. The accepted
range of an inbound link is declared with `accepts_from::<V>()` (063 §6). Static binding is
how foreign producers stay first-class: nothing they send changes.

## 7. Receiving: one decision procedure

```rust
pub enum Admit<T> { Value(Stamped<T>), Reject(Rejection) }

pub fn admit<T: MigrationChain>(env: &Envelope, link: &LinkContract) -> Admit<T>;
```

| Condition | Result | Counter |
|---|---|---|
| `name_id` ≠ the record's contract | reject | `contract_mismatch` |
| `version == T::VERSION`, shape absent or equal to the locked shape | decode payload as `T` | |
| `version == T::VERSION`, shape differs | reject | `shape_mismatch` |
| `MIN_VERSION ≤ version < T::VERSION` | decode as that version's type, walk `up` | `upgraded{from}` |
| `version > T::VERSION` | reject (no downgrade on ingest) | `too_new` |
| `version < MIN_VERSION` | reject | `too_old` |
| payload decode fails | reject | `decode_error` |
| header unparseable / not an envelope | reject | `malformed` |

Every counter is per route, readable through the existing buffer and route
metrics (013) and the probe (059). Rejections never panic and never stop the
route. The ingested value is stamped locally (058); an incoming trace is
continued (060).

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
None`; the envelope then carries no shape and the check is skipped. The
lock test already guarantees the file and the code agree; a stale lock
fails CI as today.

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
`migrate_into(1, …)` and writes `version = 1` in the header. The choice is
made once at registration; there is no per-message lookup (045's rule).

## 8. Persistence

```sql
CREATE TABLE record_history (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    record_name      TEXT    NOT NULL,
    contract_name    TEXT,             -- NULL for records without SchemaType
    contract_version INTEGER,
    contract_shape   INTEGER,
    codec            INTEGER NOT NULL, -- PayloadCodec; 1 for existing rows
    value            BLOB    NOT NULL,
    stored_at        INTEGER NOT NULL
);
```

Queries run each row through §7, so history written by v1 reads back as v2
after an upgrade, and a row of a shape that no longer exists is reported,
not mis-decoded. The migration adds the columns and backfills existing rows
as JSON with `contract_version = NULL` (treated as "use the legacy probe",
§10).

## 9. Tools

- `aimdb` CLI and MCP: route listings show, per inbound route, the contract
  versions and shapes actually seen (from §7 counters), next to the record's
  own. "Which station still sends `temperature@1`" becomes a query, not an
  investigation.
- `aimdb decode <file|hex>` decodes a captured envelope with only a
  contracts catalog: name from `name_id`, version, shape check, payload
  pretty-printed by codec. No running instance needed (R7).
- The probe (059) reports the envelope counters over a debug link.

## 10. What breaks, and the path through it

| Item | Change |
|---|---|
| `migration_chain!` | `version_field` removed (accepted and ignored with a deprecation warning for one minor) |
| `MigrationChain` | `migrate_from_bytes`/`migrate_to_version` replaced by `migrate_from`/`migrate_into` |
| Contract structs | `schema_version` field no longer needed; keeping it is harmless but it is no longer read |
| AimX | `c` field added (minor version) |
| MQTT 5 | `aimdb-c` user property and `content-type` added |
| Zenoh | 060's text attachment replaced by the §5 binary header with the trace as extension tag 1, before 060 ships |
| Persistence | new columns, backfilled |
| Connector SPI | inbound callbacks receive `Envelope<'_>` instead of `&[u8]`; outbound scratch writers write the header first |

**Deployed fleets.** Devices in the field send bare JSON with an in-band
`schema_version` and no envelope. They stay readable: a link with no
envelope carrier and no `with_contract` binding, on a record whose chain was
generated with the legacy feature, falls back to the 039 probe once per
value. The fallback lives behind `aimdb-data-contracts/legacy-version-probe`
for one major release, and its use is counted (`legacy_probe`), so an
operator can see when the last old device is gone.

### 10.1 Phases

1. **Core types.** `aimdb-core::envelope`: `ContractRef`, `Envelope`,
   `HeaderWriter`, extensions; property tests; no wire change.
2. **Identity.** `name_id`, `ContractShape` via `catalog!`; catalog check
   fails on a `name_id` collision.
3. **Admission.** `admit` and its counters; new `MigrationChain` with
   `PayloadDecoder`/`Encoder`; legacy probe behind its feature.
4. **Carriers.** AimX `c`; MQTT 5 properties; Zenoh attachment aligned
   with 060; `with_contract` static binding; connector SPI change.
5. **Persistence and tools.** Columns, backfill, `aimdb decode`, CLI/MCP
   route views.
6. **Examples.** Remove `schema_version` fields from examples and the
   README.

## 11. Tests

- Round trip: every header field combination, every codec; parse of
  `HeaderWriter` output is identity.
- Fuzz `Envelope::parse` (cargo-fuzz target): no panic, no read past the
  frame, bounded work for any input.
- Forward compatibility: a header with unknown extension tags parses and the
  value is admitted; unknown flag bits or format nibble are rejected as
  `UnsupportedFormat`.
- Admission table (§7): one test per row, counters checked.
- Shape: a contract changed under the same `NAME@VERSION` in a second
  contracts crate is rejected as `shape_mismatch` by a peer built from the
  first.
- Migration: v1 Postcard and v1 CDR payloads upgrade to v2 (impossible
  today); v2 → v1 downgrade on an outbound link.
- Allocation gate: the 054 outbound and inbound zero-allocation benches stay
  at zero with headers on; `b0_alloc_migration` shows one decode per value.
- `thumbv7em-none-eabihf` check lane builds `envelope` and `admit`; code
  size of the envelope module recorded in the PR.
- Legacy: a bare JSON v1 payload with `schema_version` is admitted through
  the probe and counted.
- Foreign: a `ros2://` publication and a KNX telegram are unchanged byte for
  byte.

## 12. Alternatives considered

1. **Keep the version in the payload.** It is what exists. It ties
   migration to JSON, needs a second parse, and makes every contract author
   responsible for a field the framework depends on. Rejected.
2. **Self-describing payloads (CBOR, 046).** Solves field naming, not
   identity: a CBOR map still does not say which contract or version it is,
   and costs bytes on every value. Orthogonal; it can be a `PayloadCodec`.
3. **Always wrap the payload.** Simplest to implement, but breaks every
   non-AimDB consumer (the reason 060 §11.10 rejected in-payload traces).
   Rejected: carriers plus static binding.
4. **Negotiate per link, no per-value header.** Fails during exactly the
   situation the product exists for: a rollout, where v1 and v2 producers
   publish on one MQTT topic at the same time. Rejected.
5. **Full contract name in the binary header.** 10–30 bytes per value on a
   serial link for something a 4-byte ID identifies within a catalog.
   Rejected for the binary form, kept for text carriers (§6.1).
6. **Random or registry-assigned contract IDs.** Need coordination or
   storage. A hash of `NAME` is stable, free, and collisions are checked
   where all names are known: the catalog.
7. **Protobuf `Any`-style type URLs.** Same bytes cost as (5), plus a
   dependency on a URL scheme the project does not otherwise use.
8. **One carrier per metadata field (status quo of 058/060).** Each field
   is designed and reviewed per connector. Rejected: one envelope, one
   extension registry.

## 13. Open questions

1. Should the shape hash be on by default for serial links (8 bytes per
   value), or opt-in per link? Proposed: on everywhere; `without_shape()`
   per link.
2. `name_id` is unique within one catalog. A fleet with two contracts
   crates could collide across them. Is "one contracts crate per fleet"
   (README step 1) a rule we enforce, or do we widen to 64 bits?
3. Should AimX `record.set` without `c` be rejected once a record has a
   chain, or always treated as current version? Proposed: current version,
   counted, for one major.
4. Does the probe (059) want the binary header for its own record dumps,
   making §5 the only offline format?
