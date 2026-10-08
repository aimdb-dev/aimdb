# Upstream issues

Bugs found in dependencies while building AimDB, one file each, with how they
affect AimDB.

| Upstream | Summary | Filed by | Status |
|---|---|---|---|
| [zenoh#2836](zenoh-2836-keyexpr-intersects-stack-overflow.md) | zenoh-keyexpr `intersects` overflows the stack on long keys | us | open |
| [zenoh#2809](zenoh-2809-keyexpr-autocanonize-panic.md) | zenoh-keyexpr `autocanonize` panics on a doubled `$*` | another user | open, fix in #2810 |
| [zenoh#2589](zenoh-2589-lz4-flex-advisory.md) | zenoh-transport pins `lz4_flex` 0.10.0 (RUSTSEC-2026-0041) | another user | open, bump in #2499 |
