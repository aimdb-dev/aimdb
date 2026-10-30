# 057 — Build identity in the binary

**Status:** 📝 Proposed

**Scope:** [#331](https://github.com/aimdb-dev/aimdb/issues/331). A
fixed-layout build-info section produced by a new leaf crate
`aimdb-build-info`, placed at a fixed flash address on boards; the AimX
methods `instance.info` and `instance.post_link` (protocol 3.1); their use in
`aimdb instance info` and the MCP `get_instance_info` tool.

---

## 1. Problem

A running instance cannot report which build it is. `hello` carries only the
protocol version, `server: "aimdb"`, permissions and writable records; no AimX
method reports a build, and nothing in the workspace records a source
revision. Operators need to identify the build on a device, including through
a debug probe when AimX is unavailable.

## 2. Requirements

| # | Requirement |
|---|---|
| R1 | Readable without a running server: from flash at a board-defined address, and from a binary by section name. |
| R2 | Layout, payload schema and AimX method are versioned independently; readers skip what they do not know. |
| R3 | Reproducible: no time, host or user in the section. |
| R4 | A fixed-size, erased post-link region can be written in place after linking, without relinking or changing the file size. |
| R5 | `no_std`, allocation-free parsing; the server never builds a JSON tree from the payload (`build()` validates it without one). |
| R6 | Every reply fits the smallest AimX frame (serial, 8 KiB) and contains no CR/LF (UDS frames are newline-delimited). |
| R7 | Additive on the wire: 3.0 clients work against 3.1 servers; 3.1 clients report "not available" against 3.0 servers. |
| R8 | Same answer on UDS, serial, TCP and WebSocket. |

## 3. Section layout (format 1)

Little-endian, offsets from the section start.

```text
off  size  field
  0     8  magic        b"AIMDB-BI"
  8     2  format       u16 = 1
 10     2  header_len   u16 = 32
 12     4  payload_len  u32
 16     4  region_off   u32, 8-aligned
 20     4  region_len   u32
 24     4  total_len    u32 = region_off + region_len
 28     4  reserved     0
  hl    …  payload      at header_len (32 in format 1's initial layout);
                        UTF-8 JSON (§3.1), zero-padded to region_off
  …     …  region       region_len bytes, 0xFF until written (§3.2)
```

Reader rules:

1. Wrong magic → no build info.
2. `format != 1` → reject. `format` changes only when a field changes meaning.
3. `header_len >= 32`; header bytes past 32 are ignored (room for new header
   fields). The payload starts at `header_len`, never at a hard-coded 32.
4. Require `header_len + payload_len <= region_off`, `region_off % 8 == 0`,
   `region_off + region_len == total_len`, `total_len <= SECTION_MAX` (64 KiB),
   and `total_len <= available bytes`.
5. Widen to `u64` / use checked arithmetic. Malformed input returns an error,
   never panics.

Probe tools read the 32-byte header first, validate it, then read
`total_len - 32` more bytes.

### 3.1 Payload (schema 1)

A JSON object, at most 4 KiB, **compact (no CR or LF bytes)**.

| Field | Type | Default source |
|---|---|---|
| `schema` | `1` | build helper |
| `name` | string | `CARGO_PKG_NAME` |
| `version` | string | `CARGO_PKG_VERSION` |
| `revision` | string (full hash) or `null` | `git rev-parse HEAD` |
| `dirty` | bool or `null` | `git status --porcelain` non-empty |
| `kind` | string or `null` | application (e.g. `release`, `simulation`) |
| `config_hash` | `"<alg>:<hex>"` or `null` | application |
| `profile` | string | `PROFILE` |
| `target` | string | `TARGET` |
| `ext` | object, optional | application, free-form |

- `null` means unknown; values are never guessed.
- Readers ignore unknown top-level fields. `schema` changes only when a field
  changes type or meaning. Other top-level names are reserved; application
  data goes in `ext`.

### 3.2 Post-link region

AimDB defines only the envelope:

- **Erased:** first four bytes `0xFF`.
- **Written:** `u32` length `L <= region_len - 4`, then `L` opaque bytes.
- **Malformed:** anything else, including a region of 1–3 bytes.

Default size 1 KiB, may be 0. Writers patch at the section's file offset; on
Apple platforms they must re-sign afterwards (`codesign -f -s -`).

## 4. Crate `aimdb-build-info`

Leaf crate, `no_std`, no dependencies by default.

| Part | Feature | Contents |
|---|---|---|
| Layout | default | `MAGIC`, `FORMAT`, `SECTION_MAX`, `parse(&[u8]) -> Result<Section<'_>, ParseError>`, `Section { format, payload, region }`, `Section::post_link() -> PostLink` (`None` / `Erased` / `Written(&[u8])` / `Malformed`) |
| Macro | default | `section!() -> &'static [u8]` (§4.2) |
| Build helper | `build` (std; `serde_json`, `sha2`) | `BuildInfo`, writes `OUT_DIR/aimdb_build_info.bin` (§4.1) |
| Linker fragment | – | `aimdb_build_info.x`, copied to `OUT_DIR` and added to the link search path (`links = "aimdb-build-info"`) |

### 4.1 Build helper

```toml
[dependencies]
aimdb-build-info = { version = "…" }

[build-dependencies]
aimdb-build-info = { version = "…", features = ["build"] }
```

```rust
// build.rs
fn main() {
    aimdb_build_info::BuildInfo::from_cargo_env()
        .kind("release")
        .config_hash_of_file("config/gateway.toml") // sha256, plus rerun-if-changed
        .ext("board", "nucleo-h563zi")
        .post_link_len(1024)
        .write()
        .expect("build info");
}
```

Other builder methods: `watch(path)` (extra input, file or recursive
directory), `track_dirty(bool)`.

**Overrides** (all `rerun-if-env-changed`):

- `AIMDB_BUILD_REVISION`, `AIMDB_BUILD_DIRTY` replace the git values.
- `AIMDB_BUILD_INFO=<path>` uses the file as the complete payload. The helper
  re-serializes it compactly, validates it against schema 1 (standard field
  types, size, no line breaks) and fails the build otherwise. The file is
  `rerun-if-changed`.

**Rerun tracking.** All git work happens in `write()`.

- Revision: watch `HEAD`, the ref it points to and `packed-refs` (via
  `git rev-parse --git-path`).
- `dirty` (unless `track_dirty(false)`): from the repository top, list known
  files (`git ls-files --cached --others --exclude-standard`) and ignored paths
  (`--others --ignored --directory`). Emit `rerun-if-changed` for each
  top-most directory with no ignored path below it, and for each known file in
  the remaining directories (always including the repository root).
- Run `git --no-optional-locks status --porcelain`; do not watch `.git/index`.
  A plain `git status` rewrites the index and causes a spurious rerun.
- `track_dirty(false)` disables only dirty tracking: it records `dirty: null`
  and skips the repository-wide watch, falling back to the package's `src/`,
  `Cargo.toml` and `build.rs`. Revision tracking, the override variables and
  file, `config_hash_of_file` inputs and `watch()` paths stay active.
- Known limit: a new untracked file in a directory that also contains ignored
  output is not detected. Use `watch()` for such inputs.

**Errors.** No `git` or not a repository → `null`. Invalid override or payload
over 4 KiB → error.

### 4.2 `section!()`

The section bytes must be opaque to Rust. A plain `static` lets the compiler
constant-fold the erased region, so a patched binary would still read it as
erased. The macro therefore assembles the bytes and declares them as an
`extern` static:

```rust
// Expansion on ELF (simplified)
mod __aimdb_build_info {
    pub const LEN: usize = include_bytes!(concat!(env!("OUT_DIR"), "/aimdb_build_info.bin")).len();
    core::arch::global_asm!(
        ".pushsection .aimdb_build_info,\"aR\",%progbits",
        ".globl __AIMDB_BUILD_INFO",
        "__AIMDB_BUILD_INFO:",
        concat!(".incbin \"", env!("OUT_DIR"), "/aimdb_build_info.bin\""),
        ".popsection",
    );
    unsafe extern "C" { pub static __AIMDB_BUILD_INFO: [u8; LEN]; }
}
unsafe { &*core::ptr::addr_of!(__aimdb_build_info::__AIMDB_BUILD_INFO) }
```

- `include_bytes!` provides the length and registers the file as a compile
  input.
- The asm text includes an FNV-1a hash of the bytes (comment fed by a `const`
  operand). Otherwise an incremental build reuses the old object when a new
  section has the same length, since `.incbin` is only read at assembly time.
- The symbol is global: a second `section!()` is a duplicate-symbol link
  error, and tools can locate the section by symbol.
- Missing `write()` → build fails on the missing file; macro docs name the
  cause. An `OUT_DIR` containing `"` is rejected at compile time.

| Target | Section | Retained by |
|---|---|---|
| ELF (Linux, `target_os = "none"`) | `.aimdb_build_info`, flags `aR` | `SHF_GNU_RETAIN`; on boards also `KEEP` in the fragment |
| Mach-O | `__DATA,__aimdb_bi` | `.no_dead_strip` |
| PE, wasm | none; ordinary static, served over AimX only; no post-link region | – |

### 4.3 Fixed address on boards

```ld
SECTIONS
{
  .aimdb_build_info AIMDB_BUILD_INFO_ADDR :
  {
    KEEP(*(.aimdb_build_info));
  } > FLASH
}
INSERT AFTER .vector_table;

ASSERT(AIMDB_BUILD_INFO_ADDR >= ADDR(.vector_table) + SIZEOF(.vector_table),
  "aimdb-build-info: AIMDB_BUILD_INFO_ADDR overlaps the vector table");
ASSERT(AIMDB_BUILD_INFO_ADDR + SIZEOF(.aimdb_build_info) <= _stext,
  "aimdb-build-info: the section runs into .text; move _stext up");
ASSERT(SIZEOF(.aimdb_build_info) > 0,
  "aimdb-build-info: no section; call aimdb_build_info::section!() in the binary");
```

The board defines the address and moves `.text` past it; the crate has no
default. For `embassy-serial-connector-demo` (STM32H563), replacing
`--defsym=_stext=0x8000250`:

```rust
println!("cargo:rustc-link-arg-bins=--nmagic");
println!("cargo:rustc-link-arg-bins=--defsym=AIMDB_BUILD_INFO_ADDR=0x08000400");
println!("cargo:rustc-link-arg-bins=--defsym=_stext=0x08002000"); // second 8 KiB sector
println!("cargo:rustc-link-arg-bins=-Tlink.x");
println!("cargo:rustc-link-arg-bins=-Taimdb_build_info.x");
println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
```

This leaves up to 7 KiB for the section. Other examples are unchanged.

## 5. Instance identity in core

```rust
AimDbBuilder::new()
    .runtime(runtime)
    .build_info(aimdb_build_info::section!()) // &'static [u8]
    .instance_uid(embassy_stm32::uid::uid())  // &[u8], ≤ 32 bytes
    .instance_name("gateway-3");              // ≤ 128 bytes UTF-8
```

- All optional. `aimdb-core` depends on `aimdb-build-info` without features.
  When one is not set: no name → `instance.name: null` and no
  `instance_name` in `hello`; no UID → `instance.uid: null`; no build info →
  `build: null` and `instance.post_link` returns `not_found`.
- `build()` parses the section and checks: payload ≤ 4 KiB, no CR/LF, is a
  JSON object (validated with `serde::de::IgnoredAny`). Failures become
  `config_errors` → `DbError::InvalidConfiguration`.
- UID is rendered as lowercase hex in the given byte order.
- `AimDb::identity() -> &Identity` for in-process use.

## 6. AimX protocol 3.1

`PROTOCOL_VERSION` becomes `"3.1"`. Compatibility is by major version, so
3.0 and 3.1 interoperate; the WebSocket `?v=` gate is unchanged.

### 6.1 `hello`

`WelcomeMessage` gains `instance_name: Option<String>`, omitted when unset
(`skip_serializing_if`). UID and build info stay out of `hello`.

### 6.2 `instance.info`

No params, `read` permission.

```json
{
  "aimdb_version": "2.1.0",
  "instance": { "name": null, "uid": "0035002e3233510f37363436" },
  "build": {
    "format": 1,
    "info": { "schema": 1, "name": "boiler-gateway", "version": "1.4.0", "…": "…" },
    "post_link": { "state": "written", "len": 1472 }
  }
}
```

- `aimdb_version`: `aimdb-core`'s `CARGO_PKG_VERSION`.
- `build`: `null` without a section.
- `build.info`: payload bytes spliced verbatim via `DispatchReply::RawJson`.
- `post_link`: `null` if `region_len == 0`, else `{"state":"erased"}`,
  `{"state":"written","len":L}` or `{"state":"malformed"}`. No region bytes.
- `INSTANCE_INFO_REPLY_MAX` = 6 KiB (payload 4 KiB + escaped name ≤ 768 B +
  UID ≤ 64 hex chars + fixed fields).

### 6.3 `instance.post_link`

Params `{"offset": u32, "len": u32}`, both optional, `read` permission. Reply
`{"offset": u32, "total": u32, "data": "<lowercase hex>"}`; `not_found` when
no section is registered or the region is not written.

| Input | Behavior |
|---|---|
| `offset` missing | 0 |
| `len` missing | 1 KiB |
| `len` a valid `u32` > `POST_LINK_CHUNK_MAX` (1 KiB) | clamped to 1 KiB |
| `len: 0` | empty `data`, `total` present |
| `offset >= total` | empty `data` |
| `offset` or `len` present but not a JSON integer in `0..=u32::MAX` (negative, fractional, string, `null`, too large) | rejected with `internal`, like other methods' malformed params; no clamping or defaulting |

Clients read from 0 until they hold `total` bytes.

### 6.4 Implementation

- Serde types `InstanceInfoReply`, `BuildReply`, `PostLinkReply`,
  `PostLinkChunk` and both limits live in `aimdb_core::remote::protocol`.
- `aimdb_core::remote::{instance_info_reply(&AimDb), post_link_chunk_reply(&AimDb, offset, len)}`
  serve both the core AimX dispatch and the WebSocket dispatch.
- `post_link_chunk_reply` returns `Result<Vec<u8>, PostLinkError>` with
  `PostLinkError::{NotWritten, InvalidParams}` (`remote` does not depend on
  `connector-session`, so it cannot return `RpcError`). Both dispatches map
  `NotWritten` → `not_found` and `InvalidParams` → `internal`. Params are
  validated before the region is checked; `null`/absent params equal `{}`, any
  other non-object is `InvalidParams`.

## 7. Clients

- **`aimdb-client`:** `AimxConnection::instance_info() -> ClientResult<Option<InstanceInfoReply>>`
  (`None` on `not_found` from a 3.0 server);
  `AimxConnection::post_link() -> ClientResult<Option<Vec<u8>>>` reads all
  chunks. `InstanceInfo` gains `name: Option<String>` from the welcome.
- **`aimdb instance list`:** Name column (`–` if unset) and `name` in JSON.
- **`aimdb instance info`:** `--format table|json|json-compact`;
  `--post-link` also fetches the region bytes (hex, `post_link_data` in JSON).
  Without `--connect`, uses the endpoint from `find_instance`.
  Table shows `Build: not reported (server speaks AimX 3.0)` against 3.0
  (exit 0) and `Build: not embedded` without a section.

  ```text
  Instance Information
    Endpoint: serial:///dev/ttyACM0?baud=115200
    AimDB: 2.1.0 (protocol 3.1)
    Name: –
    UID: 0035002e3233510f37363436
    Authenticated: no
    Max Subscriptions: 8

  Build:
    Application: boiler-gateway 1.4.0
    Revision: 3f2a9c1e5d… (uncommitted changes)
    Kind: release
    Profile: release · thumbv8m.main-none-eabihf
    Config: sha256:7d53…ff33
    Post-link: 1472 bytes
    board: nucleo-h563zi

  Permissions:
    - read
  ```

- **MCP:** `get_instance_info` adds `instance`, `build`, `aimdb_version`
  (absent against 3.0); `discover_instances` adds `name`.

## 8. Tests

| Where | What |
|---|---|
| `aimdb-build-info` | Encode/parse round trip; each header rule in §3 rejected; `header_len > 32` accepted and the payload read from `header_len`; region states incl. 1–3 byte regions and length near `u32::MAX`; no panic on any truncation or byte flip. |
| `aimdb-build-info` (`build`) | With and without git; the three overrides; override validation and compaction; identical bytes across runs; payload > 4 KiB rejected. |
| Build-script freshness | Temp git repo, two-crate workspace, dependency generating code from `assets/`, debug build. Only cargo's behavior; the watched paths, overrides and `track_dirty` are unit tests. Steps: commit → `false`; rebuild → no rerun; add asset (with old mtime) → `true`; remove → `false`; ignored file in `assets/` → documented limit, then `watch()` → `true`. After each step the binary's section equals the build-script output. |
| Host link | Release + fat LTO binary without AimX: section present by name (ELF, Mach-O) and by symbol, parses; patch region in file → size unchanged, reads `written`. |
| Host run after patch | Run the patched binary; it reports `written` with the patched bytes (catches constant folding). |
| Board layout (CI) | `embassy-serial-connector-demo` for `thumbv8m.main-none-eabihf`: section at `0x08000400`, `.text` at `0x08002000`, section parses. |
| `aimdb-core` | `instance.info` with/without section, name, UID, each region state; `build()` rejects malformed sections and payloads with line breaks; `instance_name` in welcome present/omitted; 3.0 `WelcomeMessage` parses a 3.1 welcome. |
| `aimdb-client` | `instance_info()` → `None` against a dispatch without the method; discovery fills `name`. |
| WebSocket e2e | Same `instance.info` reply as UDS; `post_link` chunking edge cases from §6.3, including each invalid-parameter case. |
| UDS round trip | Payload with whitespace and an escaped newline; `instance_name` in welcome; 1,500-byte region via `post_link()`. |
| Serial bounds | Worst case (4 KiB payload, 128-byte control-character name, 32-byte UID, full 4 KiB region): `instance.info` ≤ `INSTANCE_INFO_REPLY_MAX`, every framed reply within the serial cap, full region read over a slow pipe. |
| CLI | `instance info` table/JSON for 3.1 with build, without, and 3.0; `instance list` Name column. |
| Hardware (manual) | NUCLEO-H563ZI: probe-read section equals `build.info` over AimX; ELF with region written post-link, flashed, `--post-link` returns those bytes. |

## 9. Non-goals

- A format for the post-link region contents.
- Writing the section at run time or during on-device updates.
- Reading build info from ELF/probe in the `aimdb` CLI (possible later as
  `aimdb build-info <file>`).
- Build info or UID in `instance list`.
- Addresses for boards other than the H563 example.
- `aimdb-wasm-adapter` bindings.
- Run-time state (uptime, boot count) in `instance`.
