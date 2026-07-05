# 039 — `no_std` Typed Migrations: Lift the `std` Gate from `migratable`

**Status:** Proposed 2026-07-05. Follows the claims assessment that found `migratable` to be the only capability trait that cannot ship to an MCU. Split into two PRs: **PR 1** lifts the `std` gate (small change set; minor version bump of `aimdb-data-contracts`). **PR 2** removes the hardcoded 3-step ceiling by moving `migration_chain!` to a proc-macro — separable, and orthogonal to the `no_std` work.

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

## 3. PR 1 — Change set: lift the `std` gate

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
- `linkable` today silently drags `serde_json/std` into any build that enables it, making it std-only in practice. Adopting the workspace dependency fixes `linkable` for `no_std` **in this crate** as a side effect. Consumers that declare their own `serde_json` (default features) still leak `std` for `linkable` — see W5.
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

The Makefile already builds, tests, and lints `aimdb-data-contracts` under `--no-default-features --features alloc`. Extend those lanes (build ~line 68, test ~line 111, clippy ~line 198) to cover the newly unlocked features, and add a host **test** lane — there is none for the no-std feature set today:

```make
cargo build  --package aimdb-data-contracts --no-default-features --features alloc,linkable,migratable
cargo test   --package aimdb-data-contracts --no-default-features --features alloc,migratable
cargo clippy --package aimdb-data-contracts --no-default-features --features alloc,linkable,migratable -- -D warnings
```

Caveat on the test lane: the dev-dependency `serde_json = "1.0"` ([`Cargo.toml`](../../aimdb-data-contracts/Cargo.toml) `[dev-dependencies]`) carries default (std) features, so `cargo test` unifies `serde_json` to std regardless of the flags above. This lane therefore proves **behavior**, not `no_std`-ness. The target lane below is the actual `no_std` gate — it links no dev-dependencies, so it is what catches accidental `std` reintroduction.

Target compile checks, mirroring the existing thumbv7em lane (~line 308, `cargo check` into `$(EMBEDDED_CHECK_TARGET_DIR)`):

```make
cargo check --package aimdb-data-contracts --target thumbv7em-none-eabihf \
    --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc,linkable,migratable

# Real chain on-target with no consumer serde_json dep (see W5, criterion 3):
cargo check --package weather-mesh-common --target thumbv7em-none-eabihf \
    --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features migratable
```

### W4 — Docs

- [`lib.rs`](../../aimdb-data-contracts/src/lib.rs) compatibility table and the `SchemaType` versioning docs: note that `migration_chain!` works on `no_std + alloc`.
- README capability-trait table: `Migratable` row gains "std, no_std" runtime coverage, same notation as the connector table.

### W5 — Consumer-side plumbing (land W2 where it's consumed)

W1/W2 fix the *library*. They do not fix a consumer that carries its own `serde_json` dependency with default (std) features — which the demo's shared crate does today: [`weather-mesh-common/Cargo.toml`](../../examples/weather-mesh-demo/weather-mesh-common/Cargo.toml) declares `serde_json = { version = "1.0", optional = true }` (default features → `std`) and pulls it into **both** `linkable` and `migratable`. Without this step the doc's own acceptance crate cannot demonstrate the fix, and `migratable` still drags `std` in via the consumer's graph.

`weather-mesh-common/Cargo.toml`:

```toml
[features]
linkable   = ["aimdb-data-contracts/linkable", "serde_json"]   # unchanged: hand-written Linkable impls serialize JSON directly
migratable = ["aimdb-data-contracts/migratable"]               # was: [..., "serde_json"] — macro no longer needs a consumer dep

[dependencies]
serde_json = { workspace = true, optional = true }             # was: version = "1.0" (default features → std)
```

- Dropping `serde_json` from `migratable` is the load-bearing change: after W2 the `migration_chain!` expansion routes through `aimdb-data-contracts`'s own re-export, so a `migratable`-only build needs no consumer `serde_json` at all.
- `linkable` keeps its `serde_json` because the `Linkable` impls in [`temperature.rs`](../../examples/weather-mesh-demo/weather-mesh-common/src/contracts/temperature.rs) (`from_bytes`/`to_bytes`, and the migration-backed `TemperatureV2` impl) call `serde_json::{from_slice, to_vec}` in hand-written code — the macro fix does not reach them. Switching the dep to the workspace pin (`alloc`, no default `std`) makes that path `no_std`-clean too, so this step also unblocks `linkable` on-target for the demo, not just `migratable`.
- No source change in `temperature.rs` is required; the hand-written `serde_json` calls resolve against the now-`no_std` consumer dep.

Scope note: criterion 3's "no direct `serde_json` dependency" is literally true only for the `migratable`-without-`linkable` build — `linkable` genuinely serializes JSON by hand and will always carry the dep. The `weather-mesh-common` target lane in W3 builds exactly that configuration (`--features migratable`, no `linkable`), so `serde_json` is absent from its dependency graph.

## 4. PR 2 — Variable-arity `migration_chain!` (proc-macro)

Promotes the old non-goal #1 into a scoped follow-up. Today [`migration_chain!`](../../aimdb-data-contracts/src/migratable.rs) is a `macro_rules!` with three hand-unrolled arms (1-, 2-, 3-step). The ceiling is a `macro_rules!` limitation, not a semantic one: generating "for source version *k*, apply steps *k..N*" for arbitrary *N* needs counting, identifier concatenation, and token-list reversal — none available in declarative macros on stable. A function-like **proc-macro** does all three with ordinary Rust iteration.

**`no_std` invariance.** A proc-macro runs at build time on the host and *emits* the same `no_std + alloc` code the declarative macro does — it adds a build-time dependency, never a target/runtime one. So PR 2 is orthogonal to PR 1 and does not touch the `no_std` guarantee. [`aimdb-derive`](../../aimdb-derive/src/lib.rs) already exists (`syn` 2 / `quote`, `proc-macro = true`) and already emits foreign-crate trait paths — its `RecordKey` derive targets `aimdb_core` without depending on it — so this is additive, not new infrastructure.

### W6 — Proc-macro in `aimdb-derive`

Add `#[proc_macro] pub fn migration_chain`. A custom `syn::parse::Parse` impl reads the existing grammar (`type Current = …; version_field = "…"; steps { Step: Older => Newer, … }`) into a `Vec<StepDef>`. Emit, by iterating that vec:

- **Validation** — the same `const _: () = { assert!(…) }` block as today, generated by zipping adjacent steps: each `TO == FROM + 1`, first `FROM == 1`, `step[i].TO == step[i+1].FROM`, last `TO == Current::VERSION`. Evaluated at the *consumer's* compile time exactly as now.
- **`migrate_from_bytes`** — const-guard arms (`v if v == <Step_k as MigrationStep>::FROM_VERSION => …`, the pattern already used for the current-version arm), each parsing `Older_k` then folding `up` over steps *k..N*.
- **`migrate_to_version`** — const-guard arms folding `down` from `Current` over steps *N..k*, then `to_vec`; bounds checks unchanged.
- **O(N) code, not O(N²)** — emit one `fn __up_k` / `fn __down_k` free function per step and have each arm call the entry point, rather than re-inlining the full walk in every arm. This is the factoring `macro_rules!` could not do (it can't synthesize the identifiers).

Generated code refers back to the contracts crate by absolute path via the W2 re-export — `::aimdb_data_contracts::__private::{serde_json, alloc}` and `::aimdb_data_contracts::{MigrationStep, MigrationChain, MigrationError}` — since a proc-macro has no `$crate`. Use [`proc-macro-crate`](https://crates.io/crates/proc-macro-crate) to resolve the dependency's real name if rename-robustness is wanted; otherwise hardcode the path (matching the `RecordKey` precedent).

### W7 — Re-export wiring, delete the declarative macro

- `aimdb-data-contracts/Cargo.toml`: `migratable = ["alloc", "serde_json", "dep:aimdb-derive"]` (build-time dep; no target/runtime effect). No cycle — `aimdb-derive` emits token paths to `aimdb_data_contracts` but does not depend on it (same as `RecordKey` → `aimdb_core`).
- `lib.rs`: `#[cfg(feature = "migratable")] pub use aimdb_derive::migration_chain;` so the call path `aimdb_data_contracts::migration_chain! { … }` is unchanged for every existing caller.
- Delete the `macro_rules! migration_chain` from [`migratable.rs`](../../aimdb-data-contracts/src/migratable.rs); the `MigrationStep` / `MigrationChain` / `MigrationError` items stay hand-written and untouched.

### W8 — Tests / CI

- Add a 4- and 5-step chain to the host test suite (round-trip: upgrade from every historical version, downgrade to every target) — proves arity is unbounded.
- Keep compile-fail coverage for malformed chains (gap, wrong start, non-sequential, wrong end); if the repo lacks a compile-fail harness, add `trybuild` as a dev-dependency for these.
- Extend the thumbv7em lane so a >3-step chain compiles on-target.

### W9 — Docs

- Drop the "3 steps" ceiling from the `migration_chain!` docs and the grammar sketch in [`migratable.rs`](../../aimdb-data-contracts/src/migratable.rs); state that arity is unbounded.
- Note the macro now lives in `aimdb-derive` and is re-exported from `aimdb-data-contracts`.

### Acceptance criteria (PR 2)

1. A ≥4-step chain compiles and round-trips on host under `--features migratable`.
2. The same ≥4-step chain compiles on `thumbv7em-none-eabihf` under `--no-default-features --features alloc,migratable` — arity works on target.
3. Malformed chains fail at compile time with the existing assertion messages.
4. Every existing caller (e.g. weather-mesh-common's 1-step chain) compiles unmodified — the `aimdb_data_contracts::migration_chain!` grammar and call path are preserved.
5. Generated dispatch is O(N) in code size (per-step free functions), confirmed by `cargo expand`.

## 5. Follow-up (separate PR): `Value`-free version probe

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

**Sequencing:** this probe rewrites the same `migrate_from_bytes` body that PR 2 relocates into the proc-macro. Land PR 2 first and implement the probe once against the proc-macro; otherwise whichever lands second must re-apply the change against the other's form.

## 6. Compatibility

- **Semver:** removing `std` from `migratable`'s implied features is observable by a crate that enabled only `migratable` and relied on it transitively activating `std`. Release as `aimdb-data-contracts` 0.2.0. In-repo consumers (weather-mesh, examples) enable `std` by default and are unaffected.
- **Behavior:** none of W1–W3 changes any runtime behavior on std targets; W2 changes only name resolution inside the macro expansion.
- **MSRV:** weak dependency features (`serde_json?/std`) need Rust ≥ 1.60; the workspace is on edition 2021 and already uses `dep:`-style features in `aimdb-core`, so no MSRV movement.
- **PR 2:** `migration_chain!` moves from `macro_rules!` to a re-exported proc-macro. Source-compatible — the grammar and the `aimdb_data_contracts::migration_chain!` call path are preserved. Adds a build-time `aimdb-derive` dependency to `migratable`, with no target/runtime/`no_std` impact; `aimdb-derive` already uses `syn` 2, so no MSRV movement. Can ship in the same 0.2.0 or a later minor.

## 7. Risks

| Risk | Assessment |
|---|---|
| JSON parse cost on MCU | Bounded by wire payload size; runs only at connector ingest, not on the in-process hot path. W4 reduces peak further. |
| Feature unification re-enabling `serde_json/std` from another crate in a firmware build | Only the firmware's own dependency graph can do this; W2 removes the *need* for a caller `serde_json` dep and W5 removes the actual one from the in-repo demo. The two thumbv7em CI lanes (data-contracts + weather-mesh-common) catch regressions in-repo. |
| Float handling in `serde_json` alloc mode | Fully supported (serialization and parsing); no `std` math required. |

## 8. Non-goals

- Migration for non-JSON wire formats (KNX telegrams, compact serial framing).
- Automatic migration of persisted data (`aimdb-persistence*`) — migration remains an ingest-boundary concern wired via `Linkable::from_bytes` or explicit calls.

## 9. Acceptance criteria (PR 1)

1. `cargo check -p aimdb-data-contracts --no-default-features --features alloc,linkable,migratable --target thumbv7em-none-eabihf` succeeds in CI (the `no_std` gate; links no dev-dependencies).
2. Existing `migration_chain!` tests pass unchanged on host under `--no-default-features --features alloc,migratable`. Behavioral coverage only — the dev-dependency `serde_json` unifies to std on host, so this does **not** assert `no_std`; criterion 1 does.
3. `cargo check -p weather-mesh-common --no-default-features --features migratable --target thumbv7em-none-eabihf` compiles the real `TemperatureV1ToV2` chain with no `extern crate serde_json` and — after W5 — `serde_json` absent from the crate's dependency graph (`cargo tree -p weather-mesh-common --no-default-features --features migratable` shows no `serde_json`). The `linkable` build legitimately still carries the dep and is out of scope for this criterion.
4. After W5, `weather-mesh-common`'s `migratable` feature no longer activates `serde_json`, and its `linkable` build compiles on `thumbv7em-none-eabihf` (workspace-pinned `serde_json`, `alloc` only).
5. No new allocations on the produce/consume hot path (B0 lanes unchanged).
