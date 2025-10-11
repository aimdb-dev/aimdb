# 🚀 Design: Unified Runtime Integration & Executor Abstractions (M1.1)

**Milestone**: Runtime Independence & Multi-Platform Execution (follow-up to M1)  
**Goal**: Establish a clean, trait-based execution & time abstraction layer powering both `std` (Tokio) and `no_std` (Embassy) environments while keeping `aimdb-core` free of concrete runtime dependencies.  
**Scope of Rework**: This document retrofits experimental work done on branch `8-runtime-adapter-integration-#1` (≈5K LOC delta) into a structured, incremental upstream plan.

---

## 🎯 Problem Statement

AimDB must run the same logical services across:
- MCU / embedded boards (`no_std` + Embassy executor, static tasks, limited heap)
- Edge & Cloud (`std` + Tokio, dynamic spawning, richer timing APIs)

Early spike code mixed concerns (database, runtime selection, spawning models, timing utilities) and added crates in one large change set, making review, testing, and stabilization difficult.

We need a minimal, opinionated runtime abstraction that:
- ✅ Keeps `aimdb-core` runtime-agnostic (traits only)
- ✅ Supports both dynamic (Tokio) and static (Embassy) spawn models
- ✅ Unifies time APIs (now, sleep, delayed spawn) without leaking concrete types
- ✅ Scales to additional runtimes (custom executor, wasm) later
- ✅ Works under `no_std` (no hidden allocations) & `std` (full ergonomics)
- ✅ Enables service definition via macro (`#[service]`) that remains runtime-neutral

---

## ✅ Success Criteria

Functional:
- [ ] `aimdb-executor` crate defines core traits: `RuntimeAdapter`, `SpawnDynamically`, `SpawnStatically`, `TimeSource`
- [ ] `aimdb-core` depends only on `aimdb-executor` (no Tokio/Embassy direct deps)
- [ ] Tokio & Embassy adapters implement full trait surface behind feature flags
- [ ] `#[service]` macro generates a runtime-neutral service struct + spawn helpers
- [ ] Unified sleep / timestamp access via `TimestampProvider` + `SleepCapable` (or consolidated `TimeSource`)
- [ ] Database run loop uses only trait bounds (no cfg spaghetti internally)

Non-Functional:
- [ ] <50ms end-to-end service reaction across platforms
- [ ] Zero heap allocations in Embassy path (tracked via tests / `alloc` feature absence)
- [ ] No increase in core public API surface instability (documented semver intent)
- [ ] Code size growth of `no_std` binary attributable per feature flag
- [ ] CI matrix covers: core (no_std), core+tokio, core+embassy

---

## 🧩 Architecture Overview

Mermaid Diagram:

```mermaid
flowchart TB
    App[Application] -->|expands| Macro[[#[service] macro]]
    Macro --> Service[Generated Service Struct]
    Service --> Core[aimdb-core]
    Core --> Executor[aimdb-executor traits]
    AdapterTokio[Tokio Adapter] --> Executor
    AdapterEmbassy[Embassy Adapter] --> Executor
    subgraph Adapters
       AdapterTokio
       AdapterEmbassy
    end
```

Key Points:
1. `aimdb-core` depends only on trait contracts (`aimdb-executor`).
2. Adapters implement those traits; core never imports adapter symbols.
3. Services are generic; macro output remains runtime-neutral.
4. Application chooses which adapter(s) to instantiate and inject.

### Core Trait Set (from `aimdb-executor`)
| Trait | Responsibility | Notes |
|-------|----------------|-------|
| `RuntimeAdapter` | Identity + construction | Minimal: `new()`, `runtime_name()` |
| `SpawnDynamically` | `spawn(Future)` returning JoinHandle | Tokio style |
| `SpawnStatically` | Static task provisioning (`Spawner`) | Embassy style |
| `TimeSource` | `now()`, `sleep(duration)`, conversion helpers | Abstracts time types |

> Layering Clarification: Adapters DO NOT "implement aimdb-core". They implement the trait contracts defined in `aimdb-executor`. The `aimdb-core` crate depends only on those traits (inverting the dependency) and remains unaware of adapter concrete types. Wording elsewhere should be: “Tokio/Embassy adapters implement the executor & time traits consumed by aimdb-core,” not “implement aimdb-core.”

### Time Abstraction
Two dominant ecosystems (Tokio `std::time`, Embassy `embassy_time`). `TimeSource` unifies:
```rust
trait TimeSource: RuntimeAdapter {
    type Instant;    // runtime-specific
    type Duration;   // std::time::Duration | embassy_time::Duration
    fn now(&self) -> Self::Instant;
    fn sleep(&self, d: Self::Duration) -> impl Future<Output = ()>;
    fn millis(&self, ms: u64) -> Self::Duration; // constructor helpers
}
```

### Service Macro (`aimdb-macros::service`)
Input: `async fn my_service(ctx: RuntimeContext<R>) -> DbResult<()> { ... }`
Generates:
- Original function retained
- `struct MyService;`
- `MyService::spawn_tokio(&TokioAdapter)` (feature gated)
- `MyService::spawn_embassy(&'static EmbassyAdapter)` (feature gated)
Ensures service logic stays executor-agnostic.

### Runtime Context
`RuntimeContext<R>` wraps runtime adapter instance and exposes unified helpers for services (sleep, spawn child tasks, timestamps) w/out leaking adapter concrete type outside.

---

## 🛠 current branch DELTA (summary)

| Category | Added | Modified | Notes |
|----------|-------|----------|-------|
| New Crates | `aimdb-executor`, `aimdb-macros` | – | Provide traits + macro |
| Core | context.rs, runtime.rs, time.rs, database.rs | lib.rs, error.rs | Pulls traits + exposes reexports |
| Adapters | embassy + tokio runtime/time/database modules | existing Cargo.toml | Feature gating WIP |
| Examples | tokio + embassy runtime demos | removed single `quickstart.rs` | Split per-runtime |
| Tooling | `.gitmodules` (Embassy), Makefile edits | CI workflows | Introduced external dependency |

Risk: Monolithic merge obscures review; unstable patterns might ossify.

---

## 🔄 Refactoring / Reimplementation Plan

Phased approach to upstream clean, reviewable slices:

### Phase 0 – Baseline Preparation
1. Extract ONLY `aimdb-executor` crate with minimal traits (no macros yet)
2. Wire feature flags at workspace root (ensure no implicit default coupling)
3. CI: add matrix (core-only, tokio, no_std check build)

### Phase 1 – Core Integration
4. Refactor `aimdb-core` to depend on `aimdb-executor` traits; remove any direct runtime references
5. Introduce `RuntimeContext` (lightweight) exposing time + spawn abstractions
6. Add doc tests showing generic service using `RuntimeContext<R>`

### Phase 2 – Tokio Adapter
7. Implement `TokioAdapter` with `SpawnDynamically + TimeSource`
8. Provide basic database run example (tokio demo) – no macro yet
9. Add integration test: spawn service + verify sleep timing tolerance

### Phase 3 – Embassy Adapter
10. Vendor Embassy submodule (document licensing + update CONTRIBUTING)
11. Implement `EmbassyAdapter` with static spawn API (behind `embassy-runtime`)
12. Provide `no_std` build test (thumb target) in CI (compile-only)

### Phase 4 – Service Macro
13. Introduce `aimdb-macros` with `#[service]` generating service struct
14. Gate spawn helpers per runtime feature, ensure no accidental std usage in embassy path
15. Add macro snapshot tests (compiletest/minitest harness)

### Phase 5 – Unified Time Utilities
16. Finalize `TimeSource` shape (evaluate folding SleepCapable/TimestampProvider)
17. Replace ad-hoc time modules in adapters with canonical implementation
18. Bench micro-latency: spawn + immediate sleep vs native runtime baseline (<5% overhead)

### Phase 6 – Examples & Docs
19. Replace experimental examples with curated `examples/{tokio,embassy}-runtime-demo`
20. Add README section “Choosing a Runtime” linking to design docs
21. Add architecture diagram asset if needed

### Phase 7 – Cleanup & Deprecation
22. Remove legacy spike modules not used after refactor
23. Ensure semver guardrails: mark new APIs experimental via doc `#[doc(cfg(...))]`
24. Close tracking issues & retire integration branch

---

## 🧮 Feature Flags (proposed normalized set)
```toml
[features]
default = ["tokio-runtime"]          # Consider removing default to force explicit choice
tokio-runtime = ["dep:tokio"]
embassy-runtime = ["dep:embassy-executor", "dep:embassy-time"]
macros = ["aimdb-macros"]
services = ["macros"]                # Higher-level convenience grouping
std = []                              # Forwarded where needed
```

---

## ⚠️ Risks & Mitigations
| Risk | Impact | Mitigation |
|------|--------|------------|
| Trait proliferation | API bloat | Keep minimal initial trait set; postpone advanced scheduling |
| Embassy static model mismatch | Hard to generalize spawn | Keep separate trait (`SpawnStatically`) vs forcing unification |
| Macro hides complexity | Debug difficulty | Generate readable code; provide `--emit=expanded` doc snippet |
| Time abstraction cost | Performance regression | Micro-bench spawn + sleep vs raw runtime |
| Submodule churn | CI instability | Pin commit hash + document update procedure |

---

## 📌 Issue Breakdown (to create)
Reference each to this design doc section.

1. Add `aimdb-executor` crate (Phase 0–1)
2. Integrate executor traits into `aimdb-core` (Phase 1)
3. Introduce `RuntimeContext` with docs (Phase 1)
4. Implement `TokioAdapter` + basic example (Phase 2)
5. Add Tokio integration tests for spawn & sleep (Phase 2)
6. Vendor Embassy submodule + licensing notes (Phase 3)
7. Implement `EmbassyAdapter` skeleton (Phase 3)
8. Add `thumbv7em` compile check in CI (Phase 3)
9. Introduce `aimdb-macros` crate + `#[service]` (Phase 4)
10. Add macro expansion tests (Phase 4)
11. Finalize `TimeSource` and remove redundant traits (Phase 5)
12. Add performance benchmarks (Phase 5)
13. Replace examples with curated runtime demos (Phase 6)
14. Update README & diagrams (Phase 6)
15. Remove spike code & retire branch (Phase 7)
16. Add semver stability notes & doc cfg attributes (Phase 7)

---

## 🔓 Open Questions
- Should `RuntimeContext` own or borrow the adapter? (Current spike clones; may prefer Arc for std & &'static for no_std)
- Provide a unified delayed-spawn API or leave to runtime-specific extension traits?
- Should service macro support concurrency limits / restart policies now or later?
- Do we need a mock runtime crate for deterministic tests? (Recommended Phase 2.5 optional)

---

