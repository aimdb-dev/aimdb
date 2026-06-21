# 038 — The `aimdb-bench` Crate: Structured Benchmarking Infrastructure (W8 Companion)

**Status:** Design Doc 2026-06-13. Infrastructure for **continuous performance measurement** throughout AimDB development. Companion to [037-zero-alloc-consume-path.md](037-zero-alloc-consume-path.md), which uses these tools to validate the W8 changes. The crate is **not** W8-specific; it's a foundational framework for ongoing performance tracking and regression detection. Intentionally lands **before** 037 to capture the pre-W8 baseline; see §4 for sequencing detail.

---

## 1. Overview

`aimdb-bench` is a **sustainable, long-term benchmarking infrastructure** for AimDB development. It captures three classes of measurements — B0 (allocations, hard gate), B1 (leaf latency, trend), B2 (throughput, trend) — plus B3 (on-target embedded profiling). Unlike PR-specific benchmarks, this framework runs continuously and flags regressions through baseline drift.

**Design philosophy:**
- Measure the same workloads repeatedly; detect regressions organically
- Baseline capture + optional regression gates per metric
- Sustainable data collection, not W8-specific before/after
- Land before 037 (W8); first B0 baseline documents the pre-W8 allocation cost; 037 acceptance criterion is B0 reaching 0
- Extensible to new profiles, adapters, and measurement classes as AimDB evolves

**Key constraints:**
- Host-only; dev-dependency fenced from `no_std` graph
- Uses Criterion for B1/B2; hand-rolled `CountingAllocator<A>` for B0 (zero external deps, generic inner allocator for future embedded adaptation)
- Profiles: **Telemetry** (SpmcRing/`broadcast`), **State** (SingleLatest/`watch`), **Command** (Mailbox)
- Adapters: **Tokio** (std) and **Embassy** (on-host via test stubs)
- Isolated from production code; no leaked measurements or side effects

**Non-goals:**
- PR-specific before/after comparisons (use git history and result snapshots instead)
- System-level throughput or end-to-end workload simulation
- WASM benchmarking (unit tests cover WASM allocation changes)
- Real-time OS benchmarking or external scheduler effects
- Third-party trend storage (results captured in repo; CI integration optional)

---

## 2. Repo-aligned scope

`aimdb-bench` should be a host-only, opt-in workspace crate for measuring the current AimDB API surface. The first cut should stay on the codepaths that already exist in the repo:

- `TokioBuffer<T>::push` / `TokioBuffer<T>::subscribe` — used directly (not via the full `AimDb` stack) to isolate the buffer layer from database initialization noise
- `BufferReader::recv`
- `BufferReader::try_recv`
- existing buffer metrics and profiling gates in `aimdb-core`

It should not introduce a new runtime harness, a per-adapter spawn abstraction, or production-facing API changes. Tokio is the first target. Embassy is a follow-up only if it can be exercised through the existing host-test stubs without enabling `embassy-runtime`; otherwise it belongs in a separate embedded or hardware-only follow-up.

A separate bench file (`benches/b_aimdb_e2e.rs`) exercises the same three profiles through the full `AimDb` + `Producer<T>` + `Consumer<T>` stack. It is informational only (not a CI gate) and serves as a comparison point for the overhead added by the database initialization and record-lookup layers. It does **not** replace B0/B1/B2: those measure the buffer layer in isolation precisely because that is what 037 changes.

### 2.1 Suggested crate shape

```
aimdb-bench/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── profiles.rs
│   ├── alloc.rs
│   └── reports.rs
├── benches/
│   ├── b0_alloc_tokio.rs
│   ├── b1_b2_tokio.rs          # B1 latency (time/iter) + B2 throughput (msgs/sec)
│   └── b_aimdb_e2e.rs          # full AimDb spinup comparison (informational, not a CI gate)
└── data/
    └── baselines/
        └── (JSON baseline files checked into repo)
```

### 2.2 Dependency rules and workspace placement

- Add `aimdb-bench` as a workspace member in `Cargo.toml` (under `members = [...]`).
- Exclude it from `default-members` so `cargo build` does not build it by default.
- Depend on `aimdb-core` and the Tokio adapter only.
- Avoid pulling Embassy runtime features into the host benchmark crate.
- If Embassy coverage is added later, prefer the existing host-test stubs or a separate hardware example, not a host-side `embassy-runtime` executor.
- Update the Makefile `fmt` and `fmt-check` package loops to include `aimdb-bench` so CI formatting checks cover it.

## 3. Phased delivery plan

### Phase 1 — scaffold and measurement primitives

- Add `aimdb-bench` as a workspace member (not in `default-members`).
- Add `aimdb-bench` to the Makefile `fmt` and `fmt-check` package loops.
- Implement workload profiles and deterministic message factories.
- Implement allocation tracking and result structs.
- B0/B1/B2 suites use `TokioBuffer<T>` directly; `b_aimdb_e2e.rs` uses the full `AimDb` + `Producer<T>` + `Consumer<T>` stack.

### Phase 2 — B0 allocation gate on Tokio

- Add the first benchmark suite for allocation counting on the Tokio adapter.
- Measure Telemetry, State, and Command profiles using `TokioBuffer<T>` directly.
- Exclude warmup from the measured window.
- Compare against baseline and apply profile-specific regression budgets (optionally strict zero for W8-specific goals).

### Phase 3 — B1 latency and B2 throughput

- Add a combined latency + throughput bench (`b1_b2_tokio`) for SPSC and 1→4
  fan-out. A throughput-annotated Criterion bench reports both the per-iteration
  time (B1 latency) and msgs/sec (B2 throughput) from the same runs, so the two
  classes share one bench rather than duplicating the same push→recv loop.
- Keep raw Tokio broadcast/channel measurements only as context, not as a gate.
- Store results in repo-local baselines.

### Phase 4 — Embassy follow-up — **DONE**

Resolved: Embassy coverage **can** run through the existing host-test stubs without `embassy-runtime`, so it lands in `aimdb-bench` alongside Tokio (no host-side Embassy executor).

- **Host B0/B1/B2** (`b0_alloc_embassy`, `b1_b2_embassy` — the latter covers both B1 latency and B2 throughput): drive the real `EmbassyBuffer` backend (enabled via the adapter's `embassy-sync` + `embassy-time` features — **not** `embassy-runtime`, which would pull the cortex-m executor) through `futures::executor::block_on` over embassy-sync's poll methods. Buffer constructors live in `src/profiles_embassy.rs`. Each bench expands `aimdb_embassy_adapter::host_test_stubs!()` for the defmt logger / panic-handler / time-driver stubs, and links `critical-section/std` for `CriticalSectionRawMutex`.
  - **Lazy-subscriber gotcha:** an Embassy `SpmcRing` reader registers its `Subscriber` lazily on first poll, so a message pushed before that poll is missed and `recv()` blocks forever. The benches call `profiles_embassy::prime()` (a `try_recv`) on each reader before the first `push`. Harmless for Watch/Mailbox.
  - B0 result: **0 allocs/msg** across all three profiles (`data/baselines/b0_alloc_embassy.json`), matching the Tokio suite.
- **On-target B3** (`examples/embassy-bench-stm32h5`): the measurements that *cannot* run on a host — DWT `CYCCNT` cycle-per-message counts on an STM32H563ZI (Cortex-M33 @ 250 MHz) for the same three profiles plus a 1→4 fan-out, plus a counting allocator around `embedded-alloc` to re-validate 0 allocs/msg on real hardware. Flashed/run via `probe-rs`; results stream over RTT as defmt logs.

### Phase 5 — CI and baselines

- Make B0 the required CI gate for buffer and connector changes.
- Keep B1 and B2 informational.
- Add baseline capture and comparison scripts.
- Treat B3 as a manual, hardware-facing follow-up rather than a blocker for landing the crate.

## 4. Measurement model

### Workload profiles

- Telemetry: small, high-frequency values over SPMC Ring.
- State: larger latest-state snapshots over SingleLatest.
- Command: overwrite-style control payloads over Mailbox.

### Measurement semantics

- B0 counts allocations per message.
- B1 measures latency from publish to `recv` return.
- B2 measures steady-state messages per second for SPSC and fan-out.
- B3 is deferred to a separate embedded profiling workflow.

B1 and B2 are produced by a single Criterion bench (`b1_b2_tokio`, and its
Embassy twin `b1_b2_embassy`): a throughput-annotated bench reports the
per-iteration time (B1 latency) and msgs/sec (B2 throughput) from the same runs,
so there is no separate `b1_latency` target — the SPSC loops would be identical.

For B0, the primary purpose is comparison and regression detection over time, not a universal absolute-zero invariant across all contexts. The benchmark should measure a stable post-warmup window, report allocation metrics (for example total allocations and allocations/message), and compare those results against a committed baseline. CI pass/fail is then policy-driven from delta-to-baseline budgets per profile (strict or relaxed), while still hard-failing invalid runs (for example missing baseline, incomplete run, or invalid sample window).

**Pre-037 sequencing:** The initial B0 baseline (captured before 037 lands) will show **1 alloc/msg** — the `Box::pin(async move { ... })` constructed on every `BufferReader::recv()` call in the Tokio adapter. This is the expected pre-W8 state and a valid, useful baseline. After 037 lands and replaces the async trait method with `poll_recv` + `core::future::poll_fn`, B0 should drop to 0. The delta — from 1 to 0 — is the acceptance criterion for the 037 SPI break.

**B2 fan-out and lagging:** For `SpmcRing` 1→4 fan-out benchmarks, the bench must be designed to avoid `BufferLagged` errors; lagging is a distinct overload scenario, not part of peak throughput. Two rules guarantee a clean-path measurement: (1) subscribe all consumers *before* producing any messages — a `broadcast::Receiver` created after sends are in flight misses them, but one registered before holds its read position; (2) set `capacity >= messages_per_iteration` so no consumer can fall behind within a single bench iteration. If the lagging path is of interest, add a separate explicitly-named bench file (e.g., `b1_b2_saturated.rs`); mixing it into the main B2 suite conflates peak throughput with overload handling. Note: the error path in `TokioBufferReader` allocates unconditionally (`buffer_name: "broadcast".to_string()`), so its B0 will always be > 0 regardless of 037.

**B2 Mailbox and `notify_waiters` semantics:** The Mailbox buffer uses `tokio::sync::Notify::notify_waiters()`, which wakes currently registered waiters but does **not** store a permit — a notification fired with no current waiters is silently dropped. This is not a bug: `TokioBufferReader::Notify::recv()` checks the value slot *before* calling `notified().await`, so a value written while no consumer is parked remains in the slot and is retrieved on the next `recv()` call without blocking. For B2, the correct measurement pattern is a tight 1:1 push→recv loop (producer sends one message, consumer calls `recv()`, repeat). Do **not** batch pushes ahead of the consumer in the Mailbox B2 suite: the slot holds exactly one value, intermediate writes overwrite earlier ones, and only the last value survives — which conflates Mailbox overwrite semantics with throughput measurement. A full `AimDb` spinup (see `b_aimdb_e2e.rs`) goes through exactly the same `Notify` + slot mechanism and does **not** change these semantics; it cannot work around them.

### Allocation counter

B0 uses a hand-rolled `CountingAllocator<A: GlobalAlloc>` wrapping the platform allocator — no external crates. The generic inner allocator `A` makes it adaptable for a future embedded B3 target without rework (swap `System` for `embedded-alloc` or similar). Byte tracking is included alongside call counting: it costs nothing extra and tells you the size of the future being eliminated, which is useful context when evaluating the 037 SPI break.

```rust
// aimdb-bench/src/alloc.rs
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

pub static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
pub static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

pub struct CountingAllocator<A>(pub A);

unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        self.0.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.0.dealloc(ptr, layout)
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator<System> = CountingAllocator(System);
```

**Production isolation:** `#[global_allocator]` is a per-binary link-time declaration. `CountingAllocator` exists only in the bench binaries produced by `aimdb-bench`. Nothing in the production dependency graph — `aimdb-core`, `aimdb-tokio-adapter`, application binaries — depends on `aimdb-bench` or is affected in any way. The dependency graph runs one way only: `aimdb-bench → aimdb-core, aimdb-tokio-adapter`.

**Noise reduction:** B0 bench binaries use `tokio::runtime::Builder::new_current_thread()`. On a current-thread executor there are no work-stealing threads; Tokio's scheduler does not allocate per-poll in the hot path. The counter then cleanly isolates AimDB's per-message contribution.

**Measurement window:** Reset counters after warmup (≥100 recv iterations), then measure a batch of N messages and divide by N. Single-call measurement is less reliable even post-warmup; the batch average is the stable signal.

## 5. Running order

- `cargo bench -p aimdb-bench --bench b0_alloc_tokio`
- `cargo bench -p aimdb-bench --bench b1_b2_tokio`
- `cargo bench -p aimdb-bench --bench b0_alloc_embassy`
- `cargo bench -p aimdb-bench --bench b1_b2_embassy`

On-target B3 (cannot run on a host) — from `examples/embassy-bench-stm32h5/`, with a Nucleo-H563ZI attached:

- `cargo run --release` (flashes via `probe-rs`; defmt/RTT reports cycles/msg + allocs/msg)

## 6. CI and baselines

- Store baselines in `data/baselines/`.
- Compare new runs against the last known-good commit locally and in CI.
- Gate only B0 on PRs that touch buffer or connector code.
- Keep B1 and B2 as trend data for regressions, not hard failures.

**Phase-1 governance (run in CI + manual PR review):** B0/B1/B2 always run and produce comparison artifacts; CI never hard-fails on regression deltas. PR reviewers apply manual judgment: B0 regressions warrant investigation, B1/B2 within expected variance proceed, sustained regressions trigger follow-up or mitigation notes. Hard gates are deferred until baseline history stabilizes (typically after 2–4 weeks of data).

## 7. What stays out of scope for v1

- Custom `RuntimeHarness` and `spawn_producer`
- Host-side `embassy-runtime`
- A new production-facing runtime layer
- Embedded DWT profiling in the initial crate
- PR-specific before/after automation as the primary workflow

---

## 14. Non-goals, future work, and extensibility

**Not included (post-phase 5 or out of scope):**
- Historical trending database or cloud storage (repo-local JSON + git is sufficient; external systems optional)
- Automated per-commit trend analysis or ML-based regression detection
- Real-time system benchmarking or workload simulation (L2+ pyramid; future work)
- WASM benchmarking (unit tests cover WASM allocation changes)
- Multi-node or distributed AimDB benchmarks (not yet supported)
- GPU or FPGA acceleration profiling

**Future work (extensible by design):**
- **L2 pyramid**: Workload simulation (e.g., sensor fusion, multi-record chains, fan-out stress tests)
- **Hot-path profiling**: Flame graph generation (using `perf` or `cargo-flamegraph`) for CPU-bound investigation
- **Memory profiling**: Heap fragmentation analysis (post-B3)
- **Regression dashboards**: Web UI for browsing baseline trends (integrates with repo JSON)
- **New adapters**: Add benchmarks as new runtimes ship (e.g., `smol`, `async-std`)
- **New profiles**: Domain-specific workloads (e.g., geospatial, financial timeseries)
- **Adaptive sampling**: For noisy CI runners (reduce sample size dynamically)
- **Platform-specific gates**: Per-architecture B0 budgets (e.g., stricter on MCU targets)

**Extensibility**:
- The profile model and report structs can grow with new measurement classes without changing the published API.
- `WorkloadProfile` can be expanded with additional canonical workloads as AimDB grows.
- `AllocationTracker` can be swapped for alternative instrumentation (e.g., `prophesize`, `valgrind`).
- Result format (JSON) is human-readable and tool-friendly for downstream analysis.

---

## 15. Measurement assumptions and caveats

1. **Microbenchmark scope**: B0–B2 isolate the consume path; no claim to system-level throughput or realistic workload behavior.
2. **Single-threaded schedulers**: Criterion and B1/B2 use current-thread executors to isolate noise; results do not predict multi-threaded behavior or work-stealing schedulers.
3. **Criterion noise**: p50 medians are robust; p99 may vary ±5–10% on noisy CI runners (acceptable for trend tracking, not hard gates).
4. **Warmup excluded**: Criterion and manual setup both exclude warmup samples from the measured set; first-run cache effects are not included.
5. **Allocation tracker precision**: Counter-based (not true memory profiling); reports allocation count and byte total per-message aggregate, not per-call precision or heap fragmentation. Production binaries are fully isolated — `#[global_allocator]` is a per-binary link-time declaration scoped only to bench binaries.
6. **B3 hardware variation**: DWT cycle counts vary by CPU frequency scaling and pipeline state; B3 baseline must be recorded with CPU governor fixed to max frequency.
7. **Host platform dependence**: Results are specific to the host CPU; cross-platform comparison requires normalization (not automated).
8. **Development vs. release builds**: `--release` vs. debug optimizations can differ by 5–50×; always specify build flags when storing baselines.

---

## 16. Implementation and adoption

### Pre-implementation checklist

- [ ] Crate directory structure approved
- [ ] B0 allocation gate budget (target: 0 allocs/msg) finalized
- [ ] Criterion configuration tuned on target hardware (current-thread executor, pinning strategy)
- [ ] Developer workflow documented

### Post-implementation checklist

- [x] B0–B2 scaffold + Criterion wiring compiles and runs
- [x] B0 gate captures pre-W8 baseline (1 alloc/msg from `Box::pin` in `recv()`); **confirmed dropped to 0 once 037 / W8 landed** — `data/baselines/b0_alloc_tokio.json` now records 0 allocs/msg across all three tokio profiles
- [x] **Embassy host B0/B1/B2 land** (`b0_alloc_embassy`, `b1_b2_embassy` — the latter covers both B1 latency and B2 throughput); `data/baselines/b0_alloc_embassy.json` records 0 allocs/msg across all three Embassy profiles
- [ ] B1/B2 produce stable p50/p99 across multiple runs (\< 5% variance)
- [x] **B3 harness runs on hardware** (`examples/embassy-bench-stm32h5`, Nucleo-H563ZI @ 250 MHz, release): first cycle-count baseline captured in `data/baselines/b3_cycles_stm32h5.json` — Telemetry 2013, State 2009, Command 1661, Telemetry 1→4 6239 cycles/msg, and **0 allocs/msg confirmed on the real `embedded-alloc` heap** across all profiles
- [x] `data/baselines/` directory initialized with HEAD measurements
- [ ] README, troubleshooting, and comparison workflow complete
- [x] One-command workflow verified: `cargo bench -p aimdb-bench`

### Initial rollout

1. Land infrastructure (phases 1–3) in foundational PR
2. Capture baseline measurements on HEAD; commit baseline JSON files to `data/baselines/`
3. Integrate B0/B1/B2 CI job (advisory; no hard gate in phase 1)
4. Document developer workflow: run `cargo bench -p aimdb-bench` locally, inspect JSON output in `target/criterion/`, or review action logs
5. Use on next performance-critical PR (e.g., W8); refine based on developer feedback
6. Schedule B3 baseline capture on hardware rig during next release preparation

### Phase-1 CI Governance

**Rationale:** Manual review balances regression visibility with implementation maturity. Criterion benches can be noisy on shared CI runners, so hard gates prematurely flag false positives. After 2–4 weeks of baseline history, the team can transition to policy-driven automated gates.

**Phase-1 rules:**
1. CI always runs B0/B1/B2 for PRs touching `aimdb-core/src/buffer/`, adapter `buffer.rs` files, or `connector.rs`.
2. CI emits comparison JSON (new vs baseline) but never hard-fails.
3. PR reviewer manually examines deltas:
   - **B0 regression** (allocations/msg increased): investigate root cause before merge.
   - **B1/B2 variance** (within ±5–10%): acceptable; proceed unless sustained across multiple runs.
   - **Clear sustained regression** (>10% or repeated across commits): request mitigation or document in commit message.
4. After stable baseline history, migrate to policy-driven thresholds (for example fail if B0 delta > +0.05 allocs/msg).

### Ongoing usage

- **Development**: Run `cargo bench -p aimdb-bench` locally to generate JSON logs in `target/criterion/`. Review output to inspect allocations, latency, and throughput.
- **PRs with baseline changes**: Benchmark harness emits JSON files to `target/criterion/` and CI artifacts. Developer can checkout the PR and run `cargo bench` locally to inspect JSON output, or review action logs in the CI run.
- **Baseline updates**: When measurements improve or degrade intentionally (e.g., after W8), commit updated baseline JSON files to `data/baselines/` with clear rationale in the commit message.
- **Release prep**: Capture B3 on-target results; review against previous baseline; document in release notes if significant.
- **Maintenance**: Update baselines as reference architecture improves; document rationale in git commit message.

---

## References

- **037**: [Zero-Allocation Consume Path](037-zero-alloc-consume-path.md)
- **Criterion.rs**: https://docs.rs/criterion
- **DWT CYCCNT**: ARM Cortex-M debug spec, section D3.3
- **STM32H5 HAL**: https://github.com/stm32-rs/stm32h5xx-hal
- **`defmt`**: https://github.com/knurling-rs/defmt
