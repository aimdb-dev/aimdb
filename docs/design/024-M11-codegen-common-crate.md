# Design: Extending aimdb-codegen to Generate Common Crates

**Status:** Draft
**Date:** 2025-02-25
**Scope:** `aimdb-codegen` — extend Rust code generation to produce a complete,
compilable `xx-xx-common` crate from `.aimdb/state.toml`

---

## 1. Problem Statement

Today `aimdb-codegen` generates a single `generated_schema.rs` file containing
value structs, key enums, and a `configure_schema()` function. But every real
AimDB project ends up with a hand-written **common crate** that adds:

- `no_std` / `std` feature gating
- `Linkable` trait implementations (serialisation)
- `SchemaType` trait implementations (identity + versioning)
- `Observable` trait implementations (signal extraction, icons, units)
- Serialiser/deserialiser wiring in `configure_schema()` (currently TODO stubs)
- Convenience re-exports
- A `Cargo.toml` with correct dependencies and feature flags

This boilerplate is near-identical across projects. The codegen should produce it.

### Evidence — existing hand-written common crates

| Crate | Location | Key enums | Custom types | Trait impls |
|-------|----------|-----------|-------------|-------------|
| `weather-mesh-common` | `examples/weather-mesh-demo/` | 2 | 0 (re-exports) | 0 |
| `demo-weather-common` | `aimdb-pro/demo/` | 5 | 3 (Forecast*) | SchemaType, Observable, Linkable |
| `mqtt-connector-demo-common` | `examples/` | 2 | 2 | SchemaType, custom ser/de |
| `knx-connector-demo-common` | `examples/` | 3 | 3 | SchemaType, custom display |

Every one follows the same structure. The codegen should eliminate 80%+ of this.

---

## 2. Design Principles

1. **Generate what's derivable, leave extension points for what's not.**
   Serialisation format (JSON) is derivable. Domain-specific simulation logic is
   not — it is either hand-written or LLM-generated via inline enrichment
   (see Section 7).

2. **Generated code must compile without edits.**
   No TODO stubs in the output. If the codegen produces it, it builds.
   The agent verifies this by running `cargo check` (and optionally `clippy`)
   after generation and iterates until the crate compiles cleanly.

3. **`no_std` compatible by default.**
   Generated common crates use `#![cfg_attr(not(feature = "std"), no_std)]`
   with `alloc` for String/Vec types. Feature-gated `serde_json` for std.

4. **Single source of truth stays in `state.toml`.**
   New TOML fields drive generation. No second config file.

5. **Opt-in enrichment, not mandatory.**
   New TOML fields have sensible defaults. Existing `state.toml` files continue
   to generate valid (if minimal) output.

---

## 3. TOML Schema Extensions

### 3.1 Project-level metadata (new `[project]` block)

```toml
[project]
name = "weather-sentinel"          # used for crate naming: weather-sentinel-common
edition = "2024"                    # Rust edition, default "2024"
```

Drives the generated `Cargo.toml` crate name: `{project.name}-common`.

### 3.2 Record-level enrichment (new optional fields on `[[records]]`)

```toml
[[records]]
name = "WeatherObservation"
buffer = "SpmcRing"
capacity = 256
key_prefix = "weather.observation."
key_variants = ["Vienna", "Munich", "Berlin"]

# ── New fields ──────────────────────────────────────────────
schema_version = 2                 # SchemaType::VERSION, default 1
serialization = "json"             # "json" | "postcard" | "custom", default "json"

# Observable trait metadata (optional — omit to skip Observable impl)
[records.observable]
signal_field = "temperature_celsius"   # field name to use as signal()
icon = "🌡️"                            # Observable::ICON
unit = "°C"                            # Observable::UNIT
```

### 3.3 Field-level enrichment (new optional fields on `[[records.fields]]`)

```toml
[[records.fields]]
name = "temperature_celsius"
type = "f32"
description = "Air temperature at 2m above ground in °C"

# ── New fields ──────────────────────────────────────────────
settable = true         # include this field in Settable::Value tuple, default false
```

### 3.4 Full example — extended `state.toml`

```toml
[project]
name = "weather-sentinel"

[meta]
aimdb_version = "0.5.0"
created_at = "2026-02-24T21:39:15Z"
last_modified = "2026-02-25T10:00:00Z"

[[records]]
name = "WeatherObservation"
buffer = "SpmcRing"
capacity = 256
key_prefix = "weather.observation."
key_variants = ["Vienna", "Munich", "Berlin", "Rome", "Zurich", "Paris"]
producers = ["station_vienna", "station_munich", "station_berlin",
             "station_rome", "station_zurich", "station_paris"]
consumers = ["sentinel_agent"]
schema_version = 1
serialization = "json"

[records.observable]
signal_field = "temperature_celsius"
icon = "🌡️"
unit = "°C"

[[records.fields]]
name = "timestamp"
type = "u64"
description = "Unix timestamp in milliseconds"

[[records.fields]]
name = "temperature_celsius"
type = "f32"
description = "Air temperature at 2m above ground in °C"
settable = true

[[records.fields]]
name = "humidity_percent"
type = "f32"
description = "Relative humidity at 2m in %"
settable = true

[[records.connectors]]
protocol = "mqtt"
direction = "inbound"
url = "sensors/{variant}/observation"
```

---

## 4. Generated Output — Common Crate Structure

Running `aimdb generate --common-crate` produces a directory:

```
weather-sentinel-common/
├── Cargo.toml              # deterministic
└── src/
    ├── lib.rs              # deterministic
    └── schema.rs           # deterministic skeleton + user-accepted LLM enrichments
```

### 4.1 Generated `Cargo.toml`

```toml
# Regenerate with `aimdb generate`
[package]
name = "weather-sentinel-common"
version = "0.1.0"
edition = "2024"

[features]
default = ["std"]
std = ["aimdb-data-contracts/std", "serde_json"]
alloc = []

[dependencies]
aimdb-core = { version = "0.5", default-features = false, features = ["derive", "alloc"] }
aimdb-data-contracts = { version = "0.5", default-features = false }
serde = { version = "1.0", default-features = false, features = ["derive", "alloc"] }
serde_json = { version = "1.0", optional = true }
```

**Note:** `aimdb-executor` is intentionally absent. The common crate is
platform-agnostic and carries only data contracts. `configure_schema` and any
runtime registration code live in the application crate, not here.

### 4.2 Generated `lib.rs`

```rust
// @generated — do not edit manually.
#![cfg_attr(not(feature = "std"), no_std)]
extern crate alloc;

mod schema;

// Re-export all public types for downstream crates
pub use schema::*;
```

### 4.3 Generated `schema.rs` — extended

The existing generation (value structs, key enums, `configure_schema`) stays.
The following sections are **added**.

---

## 5. New Generated Code — Trait Implementations

### 5.1 `SchemaType` implementation

Generated for **every** record. Uses `schema_version` from TOML (default 1).

```rust
impl SchemaType for WeatherObservationValue {
    const NAME: &'static str = "weather_observation";  // snake_case of record name
    const VERSION: u32 = 1;
}
```

**Derivation rule:** `name` field → `to_snake_case()` → `SchemaType::NAME`.

### 5.2 `Linkable` implementation (serialisation)

Generated when `serialization` is set (default `"json"`).

**For `serialization = "json"`:**

```rust
impl Linkable for WeatherObservationValue {
    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        #[cfg(feature = "std")]
        {
            serde_json::to_vec(self)
                .map_err(|e| alloc::format!("serialize {}: {e}", Self::NAME))
        }
        #[cfg(not(feature = "std"))]
        {
            // Fallback: serde_json not available in no_std
            // Users should override this impl or enable the std feature
            Err(alloc::format!("no_std serialization not implemented for {}", Self::NAME))
        }
    }

    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        #[cfg(feature = "std")]
        {
            serde_json::from_slice(data)
                .map_err(|e| alloc::format!("deserialize {}: {e}", Self::NAME))
        }
        #[cfg(not(feature = "std"))]
        {
            let _ = data;
            Err(alloc::format!("no_std deserialization not implemented for {}", Self::NAME))
        }
    }
}
```

**For `serialization = "postcard"`:** generates `postcard::to_allocvec` /
`postcard::from_bytes` calls. Adds `postcard` to Cargo.toml dependencies.

**For `serialization = "custom"`:** no deterministic `Linkable` impl is
generated. Instead, the agent proposes a `Linkable` impl as an inline
enrichment, using field types and descriptions to infer the serialisation
format. If the format is ambiguous (e.g. mixed binary and text fields), the
agent asks the user to clarify before generating.

### 5.3 `Observable` implementation

Generated **only** when `[records.observable]` block is present.

```rust
impl Observable for WeatherObservationValue {
    type Signal = f32;  // inferred from the field type of signal_field
    const ICON: &'static str = "🌡️";
    const UNIT: &'static str = "°C";

    fn signal(&self) -> f32 {
        self.temperature_celsius
    }

    fn format_log(&self, node_id: &str) -> alloc::string::String {
        alloc::format!(
            "{} [{}] {}: {:.1}{} at {}",
            Self::ICON,
            node_id,
            Self::NAME,
            self.signal(),
            Self::UNIT,
            self.timestamp,  // uses first u64 field as timestamp, or computed_at
        )
    }
}
```

**`format_log` heuristic for the timestamp field:** Use the first field named
`timestamp`, `computed_at`, or `fetched_at`. If none found, omit the timestamp
portion from the format string.

### 5.4 `Settable` implementation

Generated when **any** field has `settable = true`.

```rust
impl Settable for WeatherObservationValue {
    // Tuple of all settable fields, in order
    type Value = (f32, f32);  // (temperature_celsius, humidity_percent)

    fn set(value: Self::Value, timestamp: u64) -> Self {
        Self {
            timestamp,
            temperature_celsius: value.0,
            humidity_percent: value.1,
        }
    }
}
```

**Rules:**
- `Value` is a tuple of the field types marked `settable = true`
- If only one field is settable, `Value` is the bare type (not a 1-tuple)
- The `timestamp` parameter fills the first `u64` field named `timestamp` /
  `computed_at` / `fetched_at`

---

## 6. Closing the TODO Gap — `configure_schema()` with Real Serialisers

The current generated `configure_schema()` has TODO stubs. With `Linkable`
impls available, the codegen can wire real serialisers:

### Before (current)

```rust
if let Some(addr) = key.link_address() {
    let _ = "TODO: add .with_deserializer(...)";
    reg.link_from(addr);
}
```

### After (proposed)

```rust
if let Some(addr) = key.link_address() {
    reg.link_from(addr)
        .with_deserializer(WeatherObservationValue::from_bytes)
        .finish();
}
```

For outbound:

```rust
if let Some(addr) = key.link_address() {
    reg.link_to(addr)
        .with_serializer(|v: &WeatherObservationValue| {
            v.to_bytes()
                .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
        })
        .finish();
}
```

**Condition:** Only generated when `serialization != "custom"`. When
`serialization = "custom"`, the TODO stubs remain (user provides their own).

---

## 7. LLM-Assisted Enrichment — Inline Suggestions

Some code cannot be deterministically derived from `state.toml` alone — but it
*can* be generated by an LLM, because the schema already contains rich semantic
context: field names, types, units, descriptions, buffer semantics, and
producer/consumer relationships.

### Single-file model — no `extensions.rs`

Instead of generating a separate file for LLM-produced code, the MCP tool
returns the deterministic `schema.rs` back to the calling LLM agent along with
structured context about what enrichments are possible. The agent then proposes
**inline edits** to `schema.rs` — presented as standard IDE inline suggestions
(Copilot-style) that the user can accept, reject, or modify per-suggestion.

This means:
- **One file** — no module wiring, no feature-gating between generated and
  hand-written modules, no "which file owns this impl?" confusion
- **User stays in control** — each suggestion is individually reviewable in the
  IDE diff view, not a black-box file drop
- **Incremental** — on schema changes the deterministic codegen re-runs and the
  agent can propose new inline enrichments for just the changed/added records
- **No markers, no ceremony** — the agent uses `git diff` to understand what
  changed between codegen runs. No `@generated` comments polluting the source.
  The file reads like normal Rust code (see Section 7.2)

### 7.1 Invocation flow (MCP-first)

```
┌──────────────────────────────────────────────────────────────┐
│  LLM Agent (Claude Code / IDE / MCP client)                  │
│                                                              │
│  1. Agent creates/updates .aimdb/state.toml                  │
│                                                              │
│  2. Agent calls MCP tool: generate_common_crate              │
│     └─► aimdb-mcp delegates to aimdb-codegen                 │
│         └─ writes: schema.rs, Cargo.toml, lib.rs             │
│         └─ returns to agent:                                 │
│            ├─ list of generated types + keys                 │
│            ├─ enrichment opportunities (per record):         │
│            │   • Simulatable: field ranges, units, semantics │
│            │   • format_log: richer formatting possible      │
│            │   • task scaffolds: producer/consumer stubs      │
│            └─ the generated schema.rs content                │
│                                                              │
│  3. Agent diffs state.toml + schema.rs against git HEAD      │
│     └─ identifies added/changed/removed records             │
│     └─ proposes cleanup for stale user-owned code            │
│                                                              │
│  4. Agent proposes inline edits to schema.rs:                │
│     ┌─────────────────────────────────────────────┐          │
│     │  // After the deterministic Observable impl │          │
│     │+ impl Simulatable for WeatherObservation... │ ◄ accept │
│     │                                             │   reject │
│     │- impl Simulatable for RemovedRecord...      │   modify │
│     │+ async fn station_producer(...)             │          │
│     └─────────────────────────────────────────────┘          │
│                                                              │
│  5. User accepts/rejects each suggestion in IDE              │
│                                                              │
│  6. Agent runs `cargo check` to verify compilation           │
│     └─ on error: reads diagnostics, proposes fixes           │
│     └─ iterates until the crate builds cleanly               │
└──────────────────────────────────────────────────────────────┘
```

The key insight: the LLM that created `state.toml` and `memory.md` already understands the
domain. The MCP server feeds the deterministic schema back to that same LLM
as structured context. The agent proposes enrichments as normal code edits —
no second LLM, no API key, no separate file. The agent in the loop *is* the
generator, and the IDE's diff view *is* the review surface.

### 7.2 Re-generation strategy — git diff, not markers

After the user accepts inline suggestions, `schema.rs` contains a mix of
deterministic code and user-accepted enrichments. On subsequent codegen runs,
the agent must update the deterministic parts without destroying user code.

**Strategy: the agent uses `git diff` to reason about changes.**

Both `state.toml` and `schema.rs` are version-controlled. When the agent
re-generates after a schema change:

1. **Deterministic codegen** produces a fresh `schema.rs` from `state.toml`
   (to a temporary location or in-memory)
2. **Agent diffs** the fresh output against the committed `schema.rs`
3. **Agent merges** — it understands which blocks are deterministic (structs,
   key enums, trait impls derived from TOML) vs user-owned (Simulatable impls,
   helper methods, task scaffolds). It replaces the former and preserves the
   latter.
4. **Agent adapts** user code if needed — e.g. if a field was renamed, the
   agent updates references in user-owned `Simulatable` impls
5. **Agent verifies** with `cargo check` and iterates until clean

This avoids polluting the source with marker comments. The file reads like
normal Rust code. The agent's semantic understanding of the codebase replaces
what traditional codegen tools solve with markers.

**Why this works:** the MCP path guarantees an LLM is always in the loop.
The deterministic codegen never runs blind — there's always an agent to
perform the merge intelligently. For the CLI fallback (no LLM), the codegen
simply overwrites `schema.rs` entirely — the user can `git diff` and restore
any enrichments they want to keep.

### 7.3 What the LLM enriches

| Concern | LLM input signals | Inline suggestion |
|---------|------------------|-------------------|
| `Simulatable` impl | field types + units + descriptions | `impl Simulatable for ...` with plausible ranges (e.g. −30..50 °C for temperature) |
| `format_log` override | field names + observable metadata | Richer formatting than the deterministic heuristic |
| Producer task scaffold | record name + MQTT topic + producers list | `async fn station_producer(...)` with polling skeleton |
| Consumer/agent scaffold | record name + consumers list + buffer type | `async fn sentinel_agent(...)` draining the ring |
| Helper methods | field semantics + descriptions | `fn is_anomaly(&self) -> bool` based on field descriptions |

**What the LLM does NOT enrich:**
- Custom nested types (e.g. `Vec<HourlyForecast>`) — domain modelling beyond
  `state.toml`
- `Streamable` impl (aimdb-pro) — different crate boundary
- Business logic inside tasks — the agent's reasoning algorithm is not
  derivable from schema alone

### 7.4 CLI fallback

The CLI (`aimdb generate --common-crate`) runs deterministic-only codegen.
LLM enrichment requires the MCP path because it needs an LLM in the loop.
The CLI output is a fully compilable crate — just without `Simulatable` impls
or task scaffolds.

---

## 8. Implementation Plan

### Phase 1 — TOML schema extensions + `Cargo.toml` generation

1. Add `[project]` block to `state.rs` (`ProjectDef` struct, optional)
2. Add `schema_version`, `serialization` to `RecordDef`
3. Add `[records.observable]` as `Option<ObservableDef>` to `RecordDef`
4. Add `settable` flag to `FieldDef`
5. Extend `validate.rs` to validate new fields:
   - **Error** if `serialization` is an unrecognised string
   - **Warning** if `schema_version = 0` (versions must be ≥ 1)
   - **Warning** if any field has `settable = true` but no `u64` timestamp field
     exists (`timestamp`, `computed_at`, or `fetched_at`) — `Settable::set()`
     will fall back to `Default::default()` for the timestamp slot
   - **Error** if `observable.signal_field` does not name a field in the record
   - **Warning** if `observable.signal_field` has a non-numeric type
6. Add `generate_cargo_toml()` to codegen
7. Add `generate_lib_rs()` to codegen

### Phase 2 — Trait implementation generation

1. Add `emit_schema_type_impl()` to `rust.rs`
2. Add `emit_linkable_impl()` to `rust.rs` (json / postcard branches)
3. Add `emit_observable_impl()` to `rust.rs`
4. Add `emit_settable_impl()` to `rust.rs`
5. Wire all into the existing `generate_rust()` pipeline

### Phase 3 — Close the TODO gap

1. Update `emit_configure_schema()` to call `.with_serializer()` /
   `.with_deserializer()` with real `Linkable` methods
2. Add `.finish()` calls after link setup
3. Conditionally generate TODO stubs only for `serialization = "custom"`

### Phase 4 — CLI integration

1. Add `--common-crate` flag to `aimdb generate`
2. Default output directory: `{project.name}-common/` alongside `.aimdb/`
3. CLI produces clean `schema.rs` without markers — just normal Rust code
4. Update `aimdb generate` to produce both the flat file (backward compat) and
   the crate structure (new default)

### Phase 5 — MCP-driven LLM enrichment

1. Add `generate_common_crate` tool to `aimdb-mcp`
2. Tool runs deterministic codegen (Phases 1–4) via `aimdb-codegen`
3. Agent diffs the fresh output against the committed `schema.rs` (git diff)
4. Agent merges deterministic changes while preserving user-owned enrichments
5. Agent proposes new inline enrichments for added/changed records
6. Agent verifies with `cargo check`, iterates until clean
7. CLI fallback: `aimdb generate --common-crate` overwrites `schema.rs`
   entirely; the user can `git diff` to restore enrichments manually

---

## 9. Migration Path

> **Out of scope for this alpha.** `aimdb-codegen` is not yet used in
> production. Migration tooling (version diffing, `Migratable` stub generation,
> backward-compatible output guarantees) will be designed once the codegen is
> stable and adopted. Do not implement or design migration features at this stage.

When the codegen graduates from alpha, migration will need to address:
- Projects with `state.toml` files predating the `[project]` block
- Schema version bumps between `state.toml` revisions
- Backward compatibility of generated `Cargo.toml` dependency versions

These are deferred entirely.

---

## 10. Example — Full Generated Output

Given the `state.toml` from Section 3.4, `aimdb generate --common-crate`
produces:

```
weather-sentinel-common/
├── Cargo.toml              # deterministic
└── src/
    ├── lib.rs              # deterministic
    └── schema.rs           # deterministic skeleton + user-accepted enrichments
```

**`schema.rs`** after deterministic codegen + accepted LLM enrichments:

```rust
use aimdb_core::RecordKey;
use aimdb_data_contracts::{Linkable, Observable, SchemaType, Settable};
use serde::{Deserialize, Serialize};

// ── WeatherObservation ──────────────────────────────────────────────────

/// Value type for `WeatherObservation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherObservationValue {
    /// Unix timestamp in milliseconds
    pub timestamp: u64,
    /// Air temperature at 2m above ground in °C
    pub temperature_celsius: f32,
    /// Relative humidity at 2m in %
    pub humidity_percent: f32,
}

#[derive(Debug, RecordKey, Clone, Copy, PartialEq, Eq)]
#[key_prefix = "weather.observation."]
pub enum WeatherObservationKey {
    #[key = "Vienna"]
    #[link_address = "sensors/Vienna/observation"]
    Vienna,
    #[key = "Munich"]
    #[link_address = "sensors/Munich/observation"]
    Munich,
    // ... remaining variants ...
}

impl SchemaType for WeatherObservationValue {
    const NAME: &'static str = "weather_observation";
    const VERSION: u32 = 1;
}

impl Linkable for WeatherObservationValue {
    fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        #[cfg(feature = "std")]
        { serde_json::to_vec(self).map_err(|e| alloc::format!("serialize {}: {e}", Self::NAME)) }
        #[cfg(not(feature = "std"))]
        { Err(alloc::string::String::from("no_std serialization not available")) }
    }

    fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
        #[cfg(feature = "std")]
        { serde_json::from_slice(data).map_err(|e| alloc::format!("deserialize {}: {e}", Self::NAME)) }
        #[cfg(not(feature = "std"))]
        { let _ = data; Err(alloc::string::String::from("no_std deserialization not available")) }
    }
}

impl Observable for WeatherObservationValue {
    type Signal = f32;
    const ICON: &'static str = "🌡️";
    const UNIT: &'static str = "°C";

    fn signal(&self) -> f32 {
        self.temperature_celsius
    }

    fn format_log(&self, node_id: &str) -> alloc::string::String {
        alloc::format!(
            "{} [{}] {}: {:.1}{} at {}",
            Self::ICON, node_id, Self::NAME, self.signal(), Self::UNIT, self.timestamp
        )
    }
}

impl Settable for WeatherObservationValue {
    type Value = (f32, f32);

    fn set(value: Self::Value, timestamp: u64) -> Self {
        Self {
            timestamp,
            temperature_celsius: value.0,
            humidity_percent: value.1,
        }
    }
}

// ── LLM enrichment (accepted by user, preserved on re-generation) ───────

impl Simulatable for WeatherObservationValue {
    fn simulate<R: rand::Rng>(
        config: &SimulationConfig,
        previous: Option<&Self>,
        rng: &mut R,
        timestamp: u64,
    ) -> Self {
        let base = config.params.base as f32;
        let variation = config.params.variation as f32;
        let step = config.params.step as f32;
        let celsius = match previous {
            Some(prev) => {
                let delta = (rng.gen::<f32>() - 0.5) * variation * step;
                (prev.temperature_celsius + delta).clamp(base - variation, base + variation)
            }
            None => base + (rng.gen::<f32>() - 0.5) * variation,
        };
        Self {
            timestamp,
            temperature_celsius: celsius,
            humidity_percent: (50.0 + (rng.gen::<f32>() - 0.5) * 40.0).clamp(0.0, 100.0),
        }
    }
}
```

**Note:** `configure_schema` is **not** generated in `schema.rs`. The common
crate is platform-agnostic. Application crates import the types and call
`builder.configure::<WeatherObservationValue>(key, |reg| { ... })` directly,
using `WeatherObservationValue::from_bytes` / `to_bytes` from the `Linkable`
impl for connector wiring.

---

## 11. Open Questions

1. **Should `postcard` be a first-class serialisation target?**
   It's the natural choice for `no_std` / Embassy. The TOML field supports it,
   but implementation can be deferred to Phase 2+.

2. **Should the codegen generate `Debug` formatting for key enums?**
   Currently missing from the derive list. The hand-written crates include it.
   → Recommendation: yes, add `Debug` to key enum derives.

3. **Should `configure_schema` be generic over the adapter?**
   Currently it's `<R: Spawn>`. Some registrar methods (like `.tap()`) require
   adapter-specific extension traits. The generated function should stay generic
   and only call the adapter-agnostic subset.

4. **MCP tool return contract for `generate_common_crate`.**
   The tool needs to return enough context for the calling LLM to propose
   inline enrichments. Options: (a) return the full generated `schema.rs`
   content inline, (b) return a summary + file path and let the agent read it,
   (c) return a structured list of enrichment opportunities with insertion
   points. → Recommend (c): return enrichment opportunities as structured data
   (record name, missing traits, field semantics) so the agent can propose
   targeted edits without re-reading the full file.

5. **What happens to user code referencing a removed record?**
   When a record is deleted from `state.toml`, its `@generated` marker blocks
   are removed. Since both `state.toml` and `schema.rs` are versioned, the
   agent can `git diff` the before/after to identify exactly which records were
   added, changed, or removed. For removed records, the agent proactively
   proposes deletion of stale user-owned code (e.g. orphaned `Simulatable`
   impls). For changed records, it proposes updates to enrichments that
   reference renamed or retyped fields. After applying changes, the agent runs
   `cargo check` to verify the crate compiles — if it doesn't, it reads the
   compiler errors and proposes fixes. This closes the loop: the agent doesn't
   just generate code, it verifies the result and iterates until the crate
   builds cleanly.
