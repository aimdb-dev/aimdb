# aimdb-build-info

Build identity in the binary: a section with a fixed layout that
says which build a binary is. AimDB serves it over AimX (`instance.info`), and
a tool can read it from a binary by section name or from flash through a debug
probe, without a running server.

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
 hl     …  payload      UTF-8 JSON at header_len, zero-padded to region_off
  …     …  region       region_len bytes, 0xFF until written after linking
```

All integers are little-endian. The crate is `no_std` with no dependencies,
so readers need nothing else:

```rust
let bytes: &[u8] = &[0u8; 0]; // a binary's section, or a flash dump
match aimdb_build_info::parse(bytes) {
    Ok(section) => println!("{}", core::str::from_utf8(section.payload).unwrap_or("?")),
    Err(e) => println!("no build info: {e}"),
}
```

A probe reads the 32-byte header first, checks it, then reads
`total_len - 32` more bytes. Sections over `SECTION_MAX` (64 KiB) are rejected.

## Writing the section

Feature `build` adds the build-script helper:

```toml
[dependencies]
aimdb-build-info = "0.1"

[build-dependencies]
aimdb-build-info = { version = "0.1", features = ["build"] }
```

```rust,ignore
// build.rs
fn main() {
    aimdb_build_info::BuildInfo::from_cargo_env()
        .kind("release")
        .config_hash_of_file("config/gateway.toml") // sha256, and watched
        .ext("board", "nucleo-h563zi")
        .post_link_len(1024)
        .write()
        .expect("build info");
}
```

The payload is compact JSON of at most 4 KiB:

| Field | Type | Source |
|---|---|---|
| `schema` | `1` | this crate |
| `name`, `version` | string | `CARGO_PKG_NAME`, `CARGO_PKG_VERSION` |
| `revision` | string or `null` | `git rev-parse HEAD` |
| `dirty` | bool or `null` | `git status --porcelain` non-empty |
| `kind` | string or `null` | `kind()` |
| `config_hash` | `"<alg>:<hex>"` or `null` | `config_hash()`, `config_hash_of_file()` |
| `profile`, `target` | string | `PROFILE`, `TARGET` |
| `ext` | object, optional | `ext()` |

`null` means unknown. Readers ignore fields they do not know; application
data goes in `ext`. Builds without `.git` set `AIMDB_BUILD_REVISION` and
`AIMDB_BUILD_DIRTY`, or name a complete payload file in `AIMDB_BUILD_INFO`.

## Placing the section

`section!()` places the image the build script wrote and returns its bytes.
Call it once per binary:

```rust,ignore
let build_info: &'static [u8] = aimdb_build_info::section!();
```

| Target | Section | Kept by |
|---|---|---|
| ELF (Linux, bare-metal `none`) | `.aimdb_build_info` | the `R` (retain) flag; `KEEP` in the board fragment |
| Mach-O (Apple) | `__DATA,__aimdb_bi` | `.no_dead_strip` |
| PE, wasm | none: an ordinary `static`, without a post-link region | – |

On ELF and Mach-O the bytes are opaque to the compiler, so a post-link region
written into the binary after linking is what the program reads. Tools find
the section by name or by the global symbol `__AIMDB_BUILD_INFO`. On Apple
platforms, re-sign a patched binary (`codesign -f -s -`).

## A fixed address on boards

The crate ships the linker fragment `aimdb_build_info.x`. It places the
section at `AIMDB_BUILD_INFO_ADDR`, between the vector table and `.text`, and
fails the link if the section overlaps either or is missing. The board defines
the address and moves `.text` past it; there is no default:

```rust,ignore
// build.rs of a cortex-m-rt binary
println!("cargo:rustc-link-arg-bins=--defsym=AIMDB_BUILD_INFO_ADDR=0x08000400");
println!("cargo:rustc-link-arg-bins=--defsym=_stext=0x08002000");
println!("cargo:rustc-link-arg-bins=-Tlink.x");
println!("cargo:rustc-link-arg-bins=-Taimdb_build_info.x");
```

A debug probe then reads the 32-byte header at that address, checks it, and
reads `total_len - 32` more bytes.
