# zenoh#2589 — `lz4_flex` 0.10.0 advisory in zenoh-transport

- **Upstream:** [eclipse-zenoh/zenoh#2589](https://github.com/eclipse-zenoh/zenoh/issues/2589),
  bump in [#2499](https://github.com/eclipse-zenoh/zenoh/pull/2499)
- **Filed:** 2026-05-02, by another user
- **Affects:** zenoh 1.10.1 (`zenoh-transport` requires `lz4_flex = "0.10.0"`);
  unchanged on `main` as of 2026-10-08
- **Status:** issue and PR open

## The advisory

[RUSTSEC-2026-0041](https://rustsec.org/advisories/RUSTSEC-2026-0041):
decompressing invalid data can leak uninitialized memory or a reused output
buffer. Fixed in `lz4_flex` 0.11.6 and 0.12.1, both outside zenoh's `0.10`
range, so neither `cargo update` nor `[patch]` can pull the fix in.

## Why it matters to AimDB

`cargo audit` fails on it as soon as `aimdb-zenoh-connector` depends on
`zenoh`. The connector is not exposed:

- zenoh-transport decompresses only in `WBatch::decompress`
  (`src/common/batch.rs`), behind `#[cfg(feature = "transport_compression")]`.
  Its other `lz4_flex` calls size buffers (`get_maximum_output_size`).
- The connector depends on `zenoh` without default features and never enables
  `transport_compression`. `make test-embedded` asserts it, with every
  connector feature on.

So `.cargo/audit.toml` and `deny.toml` ignore the advisory. The crate docs warn
that an application which enables zenoh's compression itself, for example
through `zenoh`'s default features, compiles the path in. Drop the ignores when
a zenoh release takes the bump.
