# Design 046: CBOR as a self-describing remote-access representation

**Status:** investigation complete — no-go for a production CBOR IR

**Issue:** [#156](https://github.com/aimdb-dev/aimdb/issues/156)

**Related:** issue #155 / PR #176, issue #177 / PR #180, issue #178 / PR #183,
designs [032](032-M16-aimx-json-codec.md),
[042](042-remote-access-via-connectors.md), and
[045](045-per-link-codec-selection.md)

**Measured revision:** `627ebea6315111b1a4feaaafdad9b9fcebcce012`

## 1. Decision

Do **not** replace `serde_json::Value` with `ciborium::Value`, add CBOR to the
production remote-access API, or change an existing wire protocol as part of
issue #156.

The evidence supports two narrower conclusions:

1. Existing JSON consumers should stay JSON. Converting a CBOR tree back to
   JSON is consistently slower and allocates more than serializing JSON
   directly.
2. A native binary edge can be worthwhile for a concrete byte-heavy consumer,
   but it needs its own negotiated protocol, resource limits, compatibility
   contract, and target budget. That is a new implementation issue, not a
   hidden change to AimX v2 or to typed `Linkable` routes.

The immediately useful follow-up is to investigate removing the intermediate
`serde_json::Value` from the current record encode/decode seam while preserving
all public JSON behavior. Direct JSON was between 2.06x and 2.49x faster to
encode, and between 2.41x and 3.54x faster to decode, than the measured JSON
tree path in the serializer microbenchmark.

This is a **no-go now**, not a claim that CBOR can never be useful. The missing
piece is a named native consumer whose workload and MCU budget justify the
extra protocol and code size.

## 2. The two representation boundaries

Issue #155 made typed connector links format-neutral. It did not make the
type-erased remote API format-neutral.

### 2.1 Typed connector routes

```text
known T at registration
        |
        v
LinkCodec<T> / Linkable
        |
        +-- JSON for one route
        +-- Postcard for another route
        `-- custom opaque bytes
        |
        v
connector payload
```

This boundary was completed by issues #155, #177, and #178. Codec selection is
fused into typed closures while `T` is known. Adding a CBOR marker here would
be a separate convenience feature and would not answer #156.

### 2.2 Type-erased remote access

The source-verified path is still JSON-specific:

```text
TypedRecord<T>::with_remote_access()
        |
        v
Arc<dyn JsonCodec<T>>
        |
        | get / set / subscribe
        v
serde_json::Value
        |
        v
serde_json bytes in Payload = Arc<[u8]>
        |
        +-- AimX-v2 NDJSON
        +-- WebSocket JSON
        +-- aimdb-client / CLI
        +-- MCP JSON-RPC
        `-- WASM / browser
```

The session substrate is already byte-opaque. `Payload` is `Arc<[u8]>`, and
`EnvelopeCodec` owns outer framing. `AimxCodec` even splices record bytes with
`RawValue` instead of rebuilding the record tree. The JSON commitment is at
the record codec and consumers, not in the transport abstraction itself.

That distinction determines the scope: #156 may evaluate a new type-erased
representation, but it must not reopen typed per-link codec selection.

## 3. Questions and hypotheses

The investigation recorded four hypotheses before measuring:

- direct JSON bytes should beat `T -> serde_json::Value -> bytes`;
- CBOR-to-JSON transcoding should not beat direct JSON for JSON consumers;
- CBOR should help most when both ends are native and records contain byte
  strings;
- serializer wins do not imply connector or system-latency wins.

All four were supported by the measurements. The last remains a claim boundary:
the host benches exclude transport reads/writes, syscalls, task scheduling,
backpressure, retransmission, and connector-owned copies.

## 4. Reproducible benchmark design

Three deterministic fixtures are shared across allocation and Criterion runs:

- `numeric_telemetry`: integers, floats, a flag, and sequence metadata;
- `nested_state`: nested structs, strings, arrays, an option, and a map;
- `byte_heavy`: metadata plus a deterministic 1,024-byte native byte string.

Five representation paths isolate different costs:

| ID | Encode | Decode | Question isolated |
|---|---|---|---|
| J-tree | `T -> JSON Value -> JSON bytes` | `bytes -> Value -> T` | Current record model |
| J-direct | `T -> JSON bytes` | `bytes -> T` | Cost of the JSON tree |
| C-transcode | `T -> CBOR Value -> JSON Value -> JSON bytes` | reverse conversion | CBOR behind a JSON edge |
| C-native | `T -> CBOR Value -> CBOR bytes` | `bytes -> CBOR Value -> T` | Dynamic native-CBOR path |
| C-direct | `T -> CBOR bytes` | `bytes -> T` | Cost of the CBOR tree |

The byte-heavy JSON representation uses an explicit tagged integer-array
convention during CBOR/JSON conversion. Native CBOR keeps a byte string. The
benchmark never silently treats the two dynamic value spaces as identical.

The conditional envelope prototype adds three in-memory logical flows:

```text
request: client encode -> envelope -> server decode -> dynamic record
write:   client encode -> envelope -> server decode -> dynamic record
event:   server encode -> envelope -> client decode -> dynamic record
```

Its JSON paths run the production `AimxCodec`. `aimx_json_tree` includes the
current record tree; `aimx_json_direct` is the required optimized control. The
CBOR path is an isolated `aimx-cbor-prototype-v1` frame carrying an opaque CBOR
record payload in a CBOR byte string. It does not change or implement the
production `EnvelopeCodec`.

## 5. Measurement environment

Host measurements were captured on:

```text
OS       Linux 7.1.2-arch3-1 x86_64
CPU      AMD Ryzen 7 4800H, 8 cores / 16 threads, 1.4-4.3 GHz
rustc    1.97.1 (8bab26f4f 2026-07-14), LLVM 22.1.6
cargo    1.97.1 (c980f4866 2026-06-30)
profile  cargo bench (optimized)
```

Criterion used 50 samples with a 1-second warmup and 3-second measurement for
the representation bench. The envelope prototype used 30 samples, a 1-second
warmup, and a 2-second measurement. Values below are point-estimate medians
from this machine; they are evidence for relative direction, not portable
latency guarantees.

## 6. Representation results

### 6.1 Payload bytes

| Shape | JSON tree/direct | CBOR native/direct | CBOR vs JSON | CBOR-transcoded JSON |
|---|---:|---:|---:|---:|
| Numeric telemetry | 131 | 96 | -26.7% | 131 |
| Nested state | 362 | 266 | -26.5% | 362 |
| Byte-heavy | 3,787 | 1,119 | -70.5% | 3,809 |

CBOR's ordinary structured-data saving is about one quarter for these
fixtures. The large byte-heavy saving comes from a real model difference:
CBOR has byte strings, while JSON expands bytes into a convention.

### 6.2 Encode median, microseconds per operation

| Shape | J-tree | J-direct | C-transcode | C-native | C-direct |
|---|---:|---:|---:|---:|---:|
| Numeric telemetry | 0.804 | 0.354 | 0.998 | 0.546 | 0.308 |
| Nested state | 2.465 | 0.992 | 4.095 | 1.692 | 0.651 |
| Byte-heavy | 12.016 | 5.839 | 12.241 | 0.735 | 0.449 |

### 6.3 Decode median, microseconds per operation

| Shape | J-tree | J-direct | C-transcode | C-native | C-direct |
|---|---:|---:|---:|---:|---:|
| Numeric telemetry | 1.025 | 0.307 | 1.174 | 1.435 | 0.530 |
| Nested state | 4.892 | 1.380 | 5.597 | 5.175 | 2.669 |
| Byte-heavy | 40.243 | 16.673 | 26.965 | 2.190 | 1.028 |

Important comparisons:

- J-direct beats J-tree for every fixture and direction.
- C-transcode is 2.10x-4.13x slower than J-direct on encode and 1.62x-4.06x
  slower on decode. It offers no reason to replace an internal IR while every
  edge still requires JSON.
- C-native is not a general decode win. It loses to direct JSON for numeric and
  nested records. Its decisive result is byte-heavy data.
- C-direct shows that some of C-native's cost belongs to materializing and
  consuming a dynamic tree rather than to CBOR bytes themselves.

### 6.4 Allocation calls / allocated bytes per operation

Encode:

| Shape | J-tree | J-direct | C-transcode | C-native | C-direct |
|---|---:|---:|---:|---:|---:|
| Numeric | 10 / 1,082 | 2 / 384 | 18 / 1,596 | 13 / 762 | 5 / 248 |
| Nested | 39 / 5,117 | 3 / 896 | 75 / 6,794 | 43 / 2,661 | 7 / 1,016 |
| Byte-heavy | 14 / 42,027 | 6 / 8,064 | 26 / 44,903 | 14 / 5,214 | 6 / 3,371 |

Decode:

| Shape | J-tree | J-direct | C-transcode | C-native | C-direct |
|---|---:|---:|---:|---:|---:|
| Numeric | 16 / 1,396 | 0 / 0 | 17 / 1,532 | 9 / 834 | 0 / 0 |
| Nested | 74 / 9,138 | 14 / 819 | 86 / 7,069 | 50 / 2,816 | 14 / 792 |
| Byte-heavy | 27 / 101,974 | 9 / 2,064 | 32 / 70,911 | 12 / 3,275 | 2 / 1,048 |

The zero numeric decode rows are expected: the concrete fixture owns no
strings or vectors. They are not zero-allocation claims for dynamic remote
access. The allocator counter's positive control performed one 64-byte
allocation after all measurement windows and was observed correctly.

## 7. Native-consumer envelope prototype

### 7.1 Complete frame bytes

The three flows differ by only a few envelope bytes, so ranges are clearer:

| Shape | AimX JSON range | Native CBOR range | Reduction |
|---|---:|---:|---:|
| Numeric telemetry | 181-184 | 132-140 | 23.9-27.1% |
| Nested state | 412-415 | 303-311 | 25.1-26.5% |
| Byte-heavy | 3,837-3,840 | 1,156-1,164 | 69.7-69.9% |

Tree and direct JSON produce the same frame sizes. Their difference is CPU and
allocation, not wire compatibility.

### 7.2 Full in-memory flow median, microseconds

| Shape / flow | AimX JSON tree | AimX JSON direct | Native CBOR |
|---|---:|---:|---:|
| Numeric request | 2.572 | 1.966 | 2.601 |
| Numeric write | 2.445 | 2.139 | 2.618 |
| Numeric event | 2.408 | 2.054 | 2.556 |
| Nested request | 7.281 | 5.254 | 5.696 |
| Nested write | 6.755 | 5.127 | 5.809 |
| Nested event | 6.705 | 5.128 | 5.520 |
| Byte-heavy request | 46.934 | 41.237 | 3.442 |
| Byte-heavy write | 46.969 | 44.350 | 3.370 |
| Byte-heavy event | 46.531 | 42.118 | 3.490 |

For numeric and nested records, direct JSON is the fastest production-compatible
path. Native CBOR is close to or faster than today's tree path for nested data,
but still 7.6-13.3% slower than direct JSON. These differences are small enough
that connector I/O could dominate them.

For the byte-heavy fixture, native CBOR is 12.1x-13.6x faster than the direct
JSON flow and reduces frame bytes by about 70%. This survives envelope work
because JSON's byte-array representation is the dominant cost. It justifies a
future binary-edge discussion for that workload; it does not justify changing
all remote records.

## 8. Cortex-M linked footprint

The isolated `no_std` fixture links two `thumbv7em-none-eabihf` images with
`opt-level = "z"`, LTO, one codegen unit, and the same nested record and 32 KiB
heap region:

```text
JSON: T -> JSON bytes -> serde_json::Value
CBOR: T -> CBOR bytes -> ciborium::Value
```

`rust-size -A` reported:

| Section | JSON image | CBOR image | Delta |
|---|---:|---:|---:|
| `.vector_table` | 1,024 | 1,024 | 0 |
| `.text` | 18,896 | 28,540 | +9,644 |
| `.rodata` | 4,992 | 4,940 | -52 |
| Flash-like total | 24,912 | 34,504 | **+9,592 (+38.5%)** |
| `.data` | 0 | 0 | 0 |
| `.bss` | 32,796 | 32,796 | 0 |

Equal `.bss` mostly reflects the deliberately identical static heap. It says
nothing about peak heap or stack. Those require an on-target high-water mark,
malformed-input cases, and the final decoder configuration. The linked result
is enough to reject any unsupported claim that CBOR is a free embedded
dependency.

## 9. Compatibility and safety findings

### 9.1 The dynamic value spaces differ

Safe common conversion covers null, booleans, finite numbers in JSON's range,
text, arrays, and maps with unique text keys. The benchmark rejects:

- non-string map keys;
- duplicate map keys;
- integers outside JSON's `i64`/`u64` representation;
- NaN and infinities;
- CBOR tags;
- byte strings unless the named tagged-byte policy is selected.

A production migration cannot silently cast these cases. It must either define
a versioned convention or return a typed incompatibility error.

### 9.2 `EnvelopeCodec` has a borrowing constraint

`EnvelopeCodec::decode_outbound` returns `Outbound<'a>` whose `sub` or `topic`
borrows from the input frame. `AimxCodec` achieves that with borrowed JSON
strings and `RawValue`. The straightforward Ciborium/Serde prototype decodes
owned strings, so it cannot honestly implement that trait without changing
ownership, adding an arena, or writing a borrowing CBOR frame parser.

That is why the experiment is an isolated consumer rather than a fake
production codec. Any future issue must choose explicitly among:

- a CBOR parser that exposes validated borrowed string slices;
- an owned outbound message variant and its allocation cost;
- a protocol-specific arena with a proven lifetime;
- a revised session contract accepted as a cross-protocol trade-off.

### 9.3 A frame limit is necessary but insufficient

The prototype rejects complete frames over 16 KiB before decoding. Ciborium's
generic Serde decoder does not enforce all required limits during allocation.
A production decoder also needs limits for nesting depth, collection elements,
text/byte lengths, tag nesting, and cumulative allocation.

```text
untrusted stream
    -> bounded framing / partial-read handling
    -> validate declared length before allocation
    -> bounded structural decoder
    -> logical envelope
    -> bounded record payload
```

Malformed input must not allocate from an attacker-controlled length before
the relevant limit is checked. Connector backpressure and resynchronization
remain transport responsibilities.

## 10. Rejected directions

### Replace the JSON IR with CBOR but keep JSON edges

Rejected. It adds conversion, allocation, value-model policy, and a dependency
while losing to direct JSON across the measured matrix.

### Add CBOR to `LinkCodec` as the answer to #156

Rejected as scope confusion. Typed routes already have a format-neutral seam;
the issue concerns a peer that consumes type-erased records.

### Switch AimX v2 or WebSocket frames in place

Rejected. Existing clients, CLI, MCP, browser, and JSON-RPC consumers require
an explicit migration/version boundary. Serializer measurements do not
authorize a wire break.

### Infer MCU suitability from host payload wins

Rejected. The linked Cortex-M image was 9.6 KiB larger before measuring stack
or peak heap. A target owner must supply an actual budget.

## 11. Follow-up scope

### Recommended now: direct JSON record bytes

Open a narrow issue to remove unnecessary JSON-tree materialization while
preserving `.with_remote_access()`, AimX v2, WebSocket, client, CLI, MCP, and
WASM behavior. The implementation must remain dyn-safe and should measure both
the record codec seam and a real remote flow. This work should not rename the
public JSON capability until its API and compatibility impact are understood.

### Conditional later: native binary remote edge

Open only when there is a named consumer and byte-heavy or bandwidth-bound
workload. Its acceptance criteria must include:

1. an explicit protocol identifier/content negotiation and fallback story;
2. no implicit CBOR-to-JSON transcode in the hot path;
3. a decision for borrowed versus owned envelope fields;
4. bounded frame, depth, collection, string, byte-string, tag, and cumulative
   allocation limits enforced during parsing;
5. malformed/truncated/fuzz tests and duplicate-key policy;
6. actual Cortex-M flash, stack, peak heap, and latency budgets;
7. compatibility that leaves AimX v2 and existing JSON consumers unchanged.

The likely placement is a new edge protocol over the already opaque session
`Payload`, not a global core IR and not a `Linkable` change.

## 12. Validation performed

```bash
cargo test -p aimdb-bench
cargo build -p aimdb-bench --benches
cargo clippy -p aimdb-bench --all-targets -- -D warnings
cargo fmt --all -- --check
cargo bench -p aimdb-bench --bench b0_alloc_remote_ir
cargo bench -p aimdb-bench --bench b1_b2_remote_ir
cargo bench -p aimdb-bench --bench b1_b2_remote_envelope
cargo bench -p aimdb-bench --bench b1_b2_remote_envelope -- --test

cd aimdb-bench/fixtures/remote-representation-footprint
cargo build --offline --release --target thumbv7em-none-eabihf \
  --features json --bin remote-repr-json
cargo build --offline --release --target thumbv7em-none-eabihf \
  --features cbor --bin remote-repr-cbor
rust-size -A target/thumbv7em-none-eabihf/release/remote-repr-json
rust-size -A target/thumbv7em-none-eabihf/release/remote-repr-cbor

cd ../../../..
make check
git diff --check
```

The host tests verify every fixture/path round-trip, malformed-input failure,
byte-policy preservation, and rejection of tags, non-finite floats, non-string
keys, and duplicate keys. No hardware execution was performed; the embedded
evidence is a target-architecture link and section-size comparison only.
The comprehensive repository check passed after it was run with permission to
bind the Unix-domain sockets used by the existing integration tests; the first
sandboxed attempt was blocked by the environment rather than a test failure.
