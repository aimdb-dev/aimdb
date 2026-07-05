# 039 — `no_std` Typed Migrations: Lift the `std` Gate from `migratable`

**Status:** Proposed 2026-07-05. Follows the claims assessment that found `migratable` to be the only capability trait that cannot ship to an MCU. Small change set; no breaking window required beyond a minor version bump of `aimdb-data-contracts`.

---

## 1. Problem

The `Migratable` story — typed, compile-time-validated schema evolution — is pitched as working "across deployed fleets", but the feature is gated on `std`:

```toml
# aimdb-data-contracts/Cargo.toml (today)
migratable = ["std", "serde_json"]
```

Consequences on a `no_std` target (Embassy on thumbv7em etc.):

- An MCU node cannot call `migrate_from_bytes` — it cannot ingest a payload one schema version behind its own contract.
- An MCU node cannot call `migrate_to_version` — it cannot downgrade its output for an older peer.
- The `Linkable`-with-migration pattern (see [`temperature.rs`](../../examples/weather-mesh-demo/weather-mesh-common/src/contracts/temperature.rs), `impl Linkable for TemperatureV2`) compiles only on std nodes, so schema evolution in a mixed fleet works only where the *ingesting* side is std.

## 2. Audit: what actually needs `std`? Nothing.

| Component | Requirement | `no_std`-clean? |
|---|---|---|
| [`MigrationStep`](../../aimdb-data-contracts/src/migratable.rs) trait | none (associated types + consts) | ✅ |
| [`MigrationError`](../../aimdb-data-contracts/src/migratable.rs) | `core::fmt`, `&'static str` payloads | ✅ |
| [`MigrationChain`](../../aimdb-data-contracts/src/migratable.rs) trait | `alloc::vec::Vec<u8>`, serde traits | ✅ (alloc) |
| `migration_chain!` generated code | `serde_json::{Value, from_slice, from_value, to_vec}` | ✅ with `serde_json/alloc` (available since serde_json 1.0.60) |
| Crate scaffolding | `#![cfg_attr(not(feature = "std"), no_std)]` + `extern crate alloc` already in place ([`lib.rs`](../../aimdb-data-contracts/src/lib.rs)) | ✅ |

There is not a single `std::` path in [`migratable.rs`](../../aimdb-data-contracts/src/migratable.rs). The gate exists only because the crate's local `serde_json` dependency is declared with default features (which imply `serde_json/std`):

```toml
# aimdb-data-contracts/Cargo.toml (today)
serde_json = { version = "1.0", optional = true }
```

The workspace already solved this — the root [`Cargo.toml`](../../Cargo.toml) pins `serde_json = { version = "1.0", default-features = false, features = ["alloc"] }`, and `aimdb-core`'s `remote` feature (JSON codec, AimX data model) already compiles on `no_std + alloc` against exactly that configuration. `aimdb-data-contracts` simply never adopted the workspace dependency.

Heap use is consistent with the engine's cost model: `alloc` is mandatory on every supported target (aimdb-core requires it; the embedded examples install `embedded-alloc`), and migration runs per *ingested wire payload*, not per in-process message — the zero-alloc steady-state guarantee of design 037 is untouched.

## 3. Change set

### W1 — Feature re-plumbing (the actual fix)

`aimdb-data-contracts/Cargo.toml`:

```toml
[features]
alloc       = ["serde/alloc"]
std         = ["alloc", "serde/std", "serde_json?/std"]
linkable    = ["alloc", "serde_json"]
migratable  = ["alloc", "serde_json"]          # was: ["std", "serde_json"]

[dependencies]
serde_json = { workspace = true, optional = true }   # was: local, default-features on
```

Notes:

- `serde_json?/std` (weak dependency feature) keeps std builds on serde_json's std implementation without forcing the dependency on.
- `linkable` today silently drags `serde_json/std` into any build that enables it, making it std-only in practice. Adopting the workspace dependency fixes `linkable` for `no_std` as a side effect — no separate work needed.
- No source change in `migratable.rs` is required for this step.

### W2 — Macro hygiene (`$crate::__private`)

The `migration_chain!` expansion references bare `serde_json::` and `alloc::` paths, which resolve in the *caller's* namespace. Today every user crate must (a) depend on `serde_json` with compatible features and (b) declare `extern crate alloc` (see the doctest header in [`migratable.rs`](../../aimdb-data-contracts/src/migratable.rs) and [`temperature.rs`](../../examples/weather-mesh-demo/weather-mesh-common/src/contracts/temperature.rs)). On `no_std` this becomes an easy footgun: a caller declaring plain `serde_json = "1"` re-enables `std` via feature unification and breaks the target build.

Fix: re-export through the defining crate and route the macro through it.

```rust
// aimdb-data-contracts/src/lib.rs
#[doc(hidden)]
pub mod __private {
    pub extern crate alloc;
    #[cfg(any(feature = "linkable", feature = "migratable"))]
    pub use serde_json;
}
```

In the macro body: `serde_json::…` → `$crate::__private::serde_json::…`, `alloc::vec::Vec` → `$crate::__private::alloc::vec::Vec`. Pure hygiene; expansion behavior is identical. Existing callers keep working (their own `serde_json` dep becomes unnecessary rather than wrong).

### W3 — CI / verification lanes

The Makefile already builds, tests, and lints `aimdb-data-contracts` under `--no-default-features --features alloc`. Extend those three lanes (build ~line 68, test ~line 111, clippy ~line 198) to cover the newly unlocked features:

```make
cargo build  --package aimdb-data-contracts --no-default-features --features alloc,linkable,migratable
cargo clippy --package aimdb-data-contracts --no-default-features --features alloc,linkable,migratable -- -D warnings
```

Plus one true-target compile check, mirroring the existing thumbv7em lanes:

```make
cargo build --package aimdb-data-contracts --no-default-features \
    --features alloc,linkable,migratable --target thumbv7em-none-eabihf
```

Host tests (`cargo test`) continue to run the behavioral suite; the target lane is compile-only, which is what catches accidental `std` reintroduction.

### W4 — Docs

- [`lib.rs`](../../aimdb-data-contracts/src/lib.rs) compatibility table and the `SchemaType` versioning docs: note that `migration_chain!` works on `no_std + alloc`.
- README capability-trait table: `Migratable` row gains "std, no_std" runtime coverage, same notation as the connector table.

## 4. Follow-up (separate PR): `Value`-free version probe

Not required for MCU enablement, but worth doing for small heaps. `migrate_from_bytes` currently parses the payload into a full `serde_json::Value` tree to read one integer, then converts with `from_value`. Peak heap is O(payload tree). The macro knows the version field name at expansion time, so it can generate a two-pass, tree-free path:

```rust
#[derive(serde::Deserialize)]
struct __VersionProbe { #[serde(rename = $version_field)] version: u32 }

let version = serde_json::from_slice::<__VersionProbe>(data)?.version;  // pass 1: scan, no tree
match version {
    1 => <Step1>::up(serde_json::from_slice::<$older1>(data)?),          // pass 2: direct to struct
    ...
}
```

Peak allocation drops from O(json tree) to O(concrete struct); two linear scans replace parse + `from_value` conversion. Gate the switch on a `b0`-style allocation measurement in `aimdb-bench` so the win is recorded, and keep the wire behavior identical (same errors for missing/unknown versions).

## 5. Compatibility

- **Semver:** removing `std` from `migratable`'s implied features is observable by a crate that enabled only `migratable` and relied on it transitively activating `std`. Release as `aimdb-data-contracts` 0.2.0. In-repo consumers (weather-mesh, examples) enable `std` by default and are unaffected.
- **Behavior:** none of W1–W3 changes any runtime behavior on std targets; W2 changes only name resolution inside the macro expansion.
- **MSRV:** weak dependency features (`serde_json?/std`) need Rust ≥ 1.60; the workspace is on edition 2021 and already uses `dep:`-style features in `aimdb-core`, so no MSRV movement.

## 6. Risks

| Risk | Assessment |
|---|---|
| JSON parse cost on MCU | Bounded by wire payload size; runs only at connector ingest, not on the in-process hot path. W4 reduces peak further. |
| Feature unification re-enabling `serde_json/std` from another crate in a firmware build | Only the firmware's own dependency graph can do this; W2 removes the most likely source (callers' duplicate `serde_json` dep). The thumbv7em CI lane catches regressions in-repo. |
| Float handling in `serde_json` alloc mode | Fully supported (serialization and parsing); no `std` math required. |

## 7. Non-goals

- Extending `migration_chain!` beyond 3 steps (hardcoded arms) — separate concern, same for std and `no_std`.
- Migration for non-JSON wire formats (KNX telegrams, compact serial framing).
- Automatic migration of persisted data (`aimdb-persistence*`) — migration remains an ingest-boundary concern wired via `Linkable::from_bytes` or explicit calls.

## 8. Acceptance criteria

1. `cargo build -p aimdb-data-contracts --no-default-features --features alloc,linkable,migratable --target thumbv7em-none-eabihf` succeeds in CI.
2. Existing `migration_chain!` tests pass unchanged on host under `--no-default-features --features alloc,migratable`.
3. A `no_std` example crate (or the embassy demo's common crate) compiles a real chain — `TemperatureV1ToV2` from weather-mesh-common is the natural candidate — with no `extern crate serde_json` and no direct `serde_json` dependency.
4. No new allocations on the produce/consume hot path (B0 lanes unchanged).
