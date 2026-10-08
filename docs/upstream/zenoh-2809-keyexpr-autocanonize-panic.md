# zenoh#2809 — `autocanonize` panics on a doubled `$*`

- **Upstream:** [eclipse-zenoh/zenoh#2809](https://github.com/eclipse-zenoh/zenoh/issues/2809),
  fix in [#2810](https://github.com/eclipse-zenoh/zenoh/pull/2810)
- **Filed:** 2026-09-24, by another user; we found it independently and did not
  file a duplicate
- **Affects:** zenoh-keyexpr 1.10.1
- **Status:** issue and fix PR open

## The bug

The `$*$*` branch of `canonize` (`commons/zenoh-keyexpr/src/key_expr/canon.rs`,
line 76 in 1.10.1) counts the rest of a `$*` run two bytes too late, one byte at
a time. #2809 has the full analysis. Our reproduction, `OwnedKeyExpr::autocanonize`
on zenoh-keyexpr 1.10.1:

| Input | Result |
|---|---|
| `a$*` | `Ok("a$*")` |
| `a$*$*` | **panic**: range start index 7 out of range for slice of length 5 |
| `a$*$*b` | **panic**: range start index 7 out of range for slice of length 6 |
| `a$*$*$*` | `Err`: it reduces to `a$*$*`, which its own validator rejects |
| `x$*$*$*y` | `Err`: reduces to `x$*$*y` |
| `a$*$*/b` | `Ok("a$*/b")` |
| `$*$*`, `a/$*$*` | `Ok("*")`, `Ok("a/*")` |

## Why it matters to AimDB

`ZenohGrammar` produces canonical filters itself, including collapsing `$*$*`
(`filter_is_canonical`). The backends must declare subscribers with that
`filter()`, through `keyexpr::new` or `KeyExpr::try_from`, and must never pass
a user's pattern through `autocanonize` (053 §4.3, and the s08 notes in
`docs/design/053-implementation-plan.md`). The grammar oracle skips canon
checks where `autocanonize` panics.
