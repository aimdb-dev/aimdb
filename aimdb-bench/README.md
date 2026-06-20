# aimdb-bench

Benchmarking infrastructure for AimDB. **Not for production use.**

Measures three classes of performance across three canonical workload profiles:

| Class | Tool | Purpose | CI gate? |
|---|---|---|---|
| **B0** — allocations/msg | hand-rolled `CountingAllocator` | regression detection on the consume path | phase 5 (planned) |
| **B1** — push-to-recv latency | Criterion p50/p99 | trend tracking | no |
| **B2** — steady-state throughput | Criterion msgs/sec | trend tracking | no |

Plus two informational benches that exercise the full runner-driven pipeline.

**Adapters covered:** Tokio only. Embassy is a planned follow-up once it can be exercised through host-test stubs without pulling in `embassy-runtime`.

---

## Workload profiles

Every bench runs the same three profiles, matching the three AimDB buffer types:

| Profile | Buffer | Tokio primitive | Payload |
|---|---|---|---|
| **Telemetry** | `SpmcRing` | `broadcast` | small (32 B) |
| **State** | `SingleLatest` | `watch` | medium (48 B) |
| **Command** | `Mailbox` | `Mutex + Notify` | small (32 B) |

---

## Running

Always run from the workspace root (`/aimdb_ws/aimdb`).

```sh
# B0 — allocation gate (buffer layer)
cargo bench -p aimdb-bench --bench b0_alloc_tokio

# B1 — latency (Criterion)
cargo bench -p aimdb-bench --bench b1_latency

# B2 — throughput (Criterion)
cargo bench -p aimdb-bench --bench b2_throughput

# Informational: allocation count through the runner pipeline
cargo bench -p aimdb-bench --bench b_alloc_pipeline

# Informational: runner-pipeline throughput (Criterion)
cargo bench -p aimdb-bench --bench b_runner_pipeline

# All at once
cargo bench -p aimdb-bench
```

### Criterion baselines

B1 and B2 use Criterion's built-in baseline system:

```sh
# Save a named baseline before a change
cargo bench -p aimdb-bench --bench b1_latency -- --save-baseline pre-w8

# Compare against it after
cargo bench -p aimdb-bench --bench b1_latency -- --baseline pre-w8
```

Criterion writes HTML reports to `target/criterion/`.

---

## B0 — allocation gate

`b0_alloc_tokio` does not use Criterion. It runs a fixed warmup + batch cycle and writes JSON results to `aimdb-bench/target/bench-results/b0_alloc_tokio.json` (the path is anchored to the crate dir, so it is the same regardless of the directory you run from).

**Measurement model:**
1. Create buffer + reader.
2. Warmup ≥ 200 push → recv cycles (excluded from counters).
3. Reset allocation counters.
4. Run 512 push → recv cycles.
5. Snapshot counters; divide by 512 for per-message figures.

The committed baseline lives in `data/baselines/b0_alloc_tokio.json`. When a change intentionally improves or changes allocation behaviour, re-run the bench and commit the updated JSON with a clear rationale in the commit message.

> **W8 result (design 037).** Since the zero-allocation consume path landed, the baseline records **0 allocs/msg** across all three tokio profiles (down from 1 — the boxed `recv()` future is gone). The committed baseline is therefore the target value; any nonzero B0 on these profiles is a regression to investigate.

**Noise reduction:** a `new_current_thread()` Tokio executor is used so there are no work-stealing threads and Tokio's scheduler does not allocate per-poll in the hot path.

**Production isolation:** `#[global_allocator]` is a per-binary link-time declaration. `CountingAllocator` exists only in bench binaries. Nothing in the production dependency graph is affected.

---

## Informational pipeline benches

`b_alloc_pipeline` and `b_runner_pipeline` exercise the same three profiles through a real `AimDbRunner` pipeline (`.source()` → buffer → `.tap()`). These include runner/stage machinery overhead on top of the buffer consume path.

Use them as a comparison point, not a regression gate. If they regress, `b0_alloc_tokio` tells you whether the issue is in the consume path itself.

---

## Caveats

- All benches measure a single current-thread Tokio executor. Results do not predict multi-threaded or work-stealing scheduler behavior.
- B0 is a counter, not a memory profiler. It reports allocation count and byte total; not per-call precision or heap fragmentation.
- B0's `bytes_per_msg` measures AimDB-added per-message heap allocations, not the message payload. Pre-W8 this was the `Box::pin` boxed `recv()` future (a single ~144 B type shared across all three buffer arms, hence identical byte counts); since design 037 / W8 the consume path is poll-based and this is **0 B/msg** on the clean path. A nonzero value flags a regression — e.g. the broadcast error path still allocates its `buffer_name` string, so a B0 run that triggers `BufferLagged`/`BufferClosed` will report > 0.
- Criterion p99 can vary ±5–10% on noisy CI runners. Use p50 medians for trend comparisons.
- Always specify `--release` or debug build consistently when comparing runs; optimizations differ by 5–50×.
- `b_alloc_pipeline` uses a paced source: per-message pace tokens and notification channels. The coordination overhead is included in the measured window.


