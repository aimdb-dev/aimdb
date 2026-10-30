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
