# Upstream issues

Bugs found in dependencies while building AimDB, one file each, with how they
affect AimDB.

| Upstream | Summary | Filed by | Status |
|---|---|---|---|
| [zenoh#2836](zenoh-2836-keyexpr-intersects-stack-overflow.md) | zenoh-keyexpr `intersects` overflows the stack on long keys | us | open |
| [zenoh#2809](zenoh-2809-keyexpr-autocanonize-panic.md) | zenoh-keyexpr `autocanonize` panics on a doubled `$*` | another user | open, fix in #2810 |
