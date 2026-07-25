# 048 — Fan-out event encoding

**Status:** complete — encoding microbench (§3–5) and end-to-end socket harness
(§6) both run; H-A/H-B/H-C answered. Improvement A implemented in `AimxCodec`
and re-measured (§7): ~14× faster encode, but only ~3% end-to-end — kept as
hygiene, not a hot-path fix.

**Scope:** the fan-out event-encoding fast path on the converged AimX/WS
transport — design [047](047-retire-ws-protocol-converge-on-aimx.md). A
benchmark-driven analysis: it measures the per-broadcast encode cost and records
the decision to keep the fast path as hygiene rather than a hot-path fix.

**Related:** design [047](047-retire-ws-protocol-converge-on-aimx.md)
(the convergence this measures) and design 046 (CBOR self-describing remote
access — record representation; a separate in-flight doc on PR #195, not yet in
this branch; shares the `aimdb-bench` harness and the byte-heavy-as-JSON
observation).

**Measured on:** `feat/retire-aimdb-ws-protocol`. Encoding microbench:
`aimdb-bench/src/fanout_encode.rs`, `benches/b0_alloc_fanout_encode.rs`,
`benches/b1_b2_fanout_encode.rs`. Socket harness:
`aimdb-websocket-connector/benches/fanout_socket.rs`.

**Environment:** Docker Desktop VM — Linux 6.10.14-linuxkit x86_64, Intel Core
i5-7267U @ 3.10 GHz (4 cores visible), rustc 1.91.1, `cargo bench` (optimized).
This is a **virtualized, noisy** host: absolutes run high and Criterion
confidence intervals are wide. The numbers below are point-estimate medians and
are evidence for **relative direction**, not portable latency guarantees.

---

## 1. Purpose

The converged AimX-over-WS path builds the `event` envelope **per subscriber**:
each connection stamps `{"t":"event","seq":N,"topic":T,"sub":S,"data":V}` with
its own `sub` (id-routed demux) and `seq` (drop detection). The retired
`aimdb-ws-protocol` path pre-serialized **one** `Data{topic,payload,ts}` frame
and shared the finished bytes, because that frame carried no per-subscriber
fields. `sub`/`seq` are the capabilities the convergence exists to add (047
§2.2, §3.1), so "serialize once" was a property of a thinner frame, not a bar
to clear — the convergence is already decided **Go** (047 §5).

This benchmark therefore does **not** compare against the deleted crate. It
measures the converged encode path **against itself** — the current
per-subscriber codec versus an "encode once, patch the per-subscriber fields"
fast path — to answer two questions: does the encode cost scale cleanly, and is
it worth optimizing?

It is a pure CPU/allocation microbenchmark. It excludes the socket write, task
scheduling, the `ClientManager` fan-out, and backpressure — all of which both
strategies share. One timed unit is **one broadcast to N subscribers** (N
frames).

---

## 2. Hypotheses (pre-registered)

- **H-B — linear scaling.** Per-broadcast encode cost grows ~linearly in N with
  a small constant per-subscriber increment; no super-linear knee. *Falsified
  by* any O(N²)/contention curve.
- **H-C — optimization payoff.** The shared-suffix fast path reduces
  per-broadcast cost by a meaningful fraction, and the benchmark localizes
  *where* (expected: byte-heavy payloads and high fan-out) versus where the
  naive path is already cheap (expected: small payloads).

- **H-A — absolute acceptability & encode share.** Whether 500-client fan-out
  is fast enough in absolute terms, and whether the encode is a real fraction of
  the per-subscriber cost relative to the **socket write** both paths pay. The
  CPU microbench cannot settle this; it is answered by the socket-driven harness
  in §6.

---

## 3. Method

**Two payload shapes** (already-serialized JSON, as a record codec hands the
session layer):

| Shape | Record payload | Event frame |
|---|--:|--:|
| `small_structured` | 110 B object | 191 B |
| `byte_heavy` | 3657 B JSON integer array (a byte record — JSON has no byte type, 046 §6.1) | 3738 B |

**Two strategies, byte-identical output:**

- **`naive`** — the production `AimxCodec::encode(Outbound::Event)`, re-run once
  per subscriber. Each call re-escapes `topic`/`sub`, re-validates the record
  value (`as_raw`), and allocates serde intermediates.
- **`shared_suffix`** — the fast path. Escapes `topic` once per broadcast into a
  reused middle segment; per subscriber only formats the two varying scalars
  (`sub`, `seq`) and splices the shared record bytes. The current AimX field
  order (`t, seq, topic, sub, data`) already lets this stay **byte-identical**
  to the codec, so no wire change is needed.

**Faithfulness gate.** A test (`fast_path_is_byte_identical_to_codec`) asserts
the fast path reproduces the codec's bytes for both shapes across 512
subscribers. This is what licenses treating the two strategies as measuring the
same output rather than comparing different frames.

**Config.** Criterion: 30 samples, 1 s warmup, 3 s measurement. Allocation
bench: 50 warmup + 500 measured broadcasts per row, `CountingAllocator` global,
positive control fired after all windows.

**Reproduce:**

```bash
cargo test  -p aimdb-bench fanout
cargo bench -p aimdb-bench --bench b0_alloc_fanout_encode
cargo bench -p aimdb-bench --bench b1_b2_fanout_encode
```

---

## 4. Results

### 4.1 Allocation (deterministic)

Allocation calls and bytes per broadcast; `allocs/frame` is per-subscriber.

All three strategies at N=500 (`allocs/frame` is per-subscriber):

| Shape | Strategy | allocs/frame | bytes/broadcast |
|---|---|--:|--:|
| small | naive | 3.00 | 288,390 |
| small | validate_once | 2.002 | 192,110 |
| small | shared_suffix | 1.002 | 99,438 |
| byte-heavy | naive | 4.00 | 7,542,060 |
| byte-heavy | validate_once | 3.002 | 5,675,827 |
| byte-heavy | shared_suffix | 1.002 | 1,872,938 |

The naive path allocates **3–4× per frame** (serde intermediates + the output
buffer); hoisting validation removes one alloc/frame; the shared-suffix path
converges on **~1 alloc/frame** (just the output buffer) plus one per-broadcast
segment. For byte-heavy at 500 subscribers, allocation bytes drop **7.5 MB →
1.9 MB per broadcast** (naive → shared_suffix), with the validation hoist
accounting for the first ~1.9 MB of that.

### 4.2 Latency (medians, µs per broadcast)

Three strategies from one internally-consistent run (`naive` → `validate_once`
= Improvement A alone; `validate_once` → `shared_suffix` = Improvement B on
top). Absolutes vary run-to-run on this virtualized host (see §7), so the
comparison uses a single run.

**`small_structured` (191 B frame):**

| N | naive | validate_once | shared_suffix |
|--:|--:|--:|--:|
| 1 | 1.44 | 1.65 | 0.30 |
| 10 | 9.46 | 4.78 | 2.10 |
| 100 | 99.6 | 70.7 | 19.7 |
| 500 | 530 | 293 | 107 |

**`byte_heavy` (3738 B frame):**

| N | naive | validate_once | shared_suffix |
|--:|--:|--:|--:|
| 1 | 15.4 | 24.2 | 0.47 |
| 10 | 148 | 23.7 | 3.47 |
| 100 | 1713 | 112 | 42.8 |
| 500 | 7587 | 347 | 143 |

At N=1 `validate_once` can trail `naive` (its single validation is unamortized,
plus host noise); the comparison only matters at fan-out (N ≫ 1).

---

## 5. Findings and encode-path improvements

### 5.1 H-B — scaling is clean (for the encode loop)

`naive` µs/frame is roughly constant (2.4–3.3 small; 14–22 byte-heavy);
`shared_suffix` µs/frame *decreases* with N as the once-per-broadcast precompute
amortizes. No super-linear knee. **H-B holds for the encode loop.** The caveat:
this microbench does not include the `ClientManager` `DashMap` fan-out, so the
true O(N²)/lock-contention check for the *bus* still requires the socket-driven
bench.

### 5.2 H-C — the payoff, isolated

The win decomposes into two independent improvements, and the `validate_once`
strategy isolates them. The root cause of the large byte-heavy gap is in the
codec: `AimxCodec::encode(Outbound::Event)` calls `as_raw(&data)`
(`aimdb-core/src/session/aimx/codec.rs:174`), which runs `serde_json` over the
**entire** record value to validate it is one JSON value. The naive path
therefore **re-validates the whole 1024-element array once per subscriber** —
O(payload) × N.

The N=500 decomposition (µs per broadcast, this run):

| Shape | naive | → validate_once (A) | → shared_suffix (B) | A share | B share |
|---|--:|--:|--:|--:|--:|
| small | 530 | 293 | 107 | ~56% | ~44% |
| byte-heavy | 7587 | 347 | 143 | **~97%** | ~3% |

**Improvement A — hoist the payload validation out of the per-subscriber loop.**
The record value is identical for every subscriber of a broadcast; validating it
once instead of once per subscriber removes N−1 full `serde_json` passes over
the payload. For byte-heavy this **alone is a 21.8× reduction** (7587 → 347 µs)
— essentially the entire win. It is a **small, self-contained change** that
needs none of the shared-suffix machinery, and it is the highest
value-to-effort item this benchmark surfaced. For small payloads it is a ~1.8×
cut (validation is cheap when the payload is tiny).

**Improvement B — the shared-suffix fast path.** Escape `topic` once per
broadcast and stamp per subscriber only the two varying scalars, rather than
re-running the full serde frame serialization each time. On top of A it adds a
further ~2.4× (byte-heavy) / ~2.7× (small) and cuts allocations to ~1/frame — it
carries **most of the small-payload win** but little of the byte-heavy one. It
applies cleanly to **exact-topic** fan-out (many clients on one topic share
`topic`/`data`); wildcard subscriptions, where each client matched a different
record, have no shared suffix and keep the per-frame path (047 §3.1).

Both are on the same contiguous-frame model as today: the payload is still
copied into each subscriber's frame buffer (a WS text frame embeds it), so
neither improvement removes that copy — they remove *serialization and
validation*, not the memcpy. True zero-copy would need vectored writes, which
WebSocket framing via tokio-tungstenite does not cleanly expose.

---

## 6. End-to-end socket-driven fan-out (H-A)

A separate harness (`aimdb-websocket-connector/benches/fanout_socket.rs`) drives
the **real** converged path: a live `WebSocketConnector` server fanning one
record update out to N raw `tokio-tungstenite` clients (not aimdb client
connectors — 500 of those would put the cost on the client side). One round sets
the record to a fresh value and waits until every client observes it; the
elapsed time is the end-to-end fan-out latency of one broadcast, **including**
the socket write, Tokio scheduling, the `ClientManager` bus, and client
receive/parse. 100 rounds/config, same Docker VM.

### 6.1 Results (median latency per broadcast)

| Shape | N | median | per-delivery | msgs/s |
|---|--:|--:|--:|--:|
| small | 1 | 156 µs | 156 µs | 5.6k |
| small | 10 | 547 µs | 55 µs | 17k |
| small | 100 | 5.1 ms | 51 µs | 14k |
| small | 500 | 20.8 ms | 42 µs | 22k |
| byte-heavy | 1 | 287 µs | 287 µs | 3.1k |
| byte-heavy | 10 | 732 µs | 73 µs | 11k |
| byte-heavy | 100 | 4.7 ms | 47 µs | 19k |
| byte-heavy | 500 | 22.9 ms | 46 µs | 18k |

### 6.2 What it says

- **Absolute (H-A):** one broadcast reaches all 500 clients in ~21–23 ms
  (~42–46 µs/delivery, ~18–22k deliveries/s). For a dashboard-style workload
  (a topic a few hundred browsers watch, updating a few times per second) that
  is comfortable.
- **System-level scaling (H-B, with the real bus):** per-delivery falls then
  flattens (~42–55 µs) as fixed costs amortize; total latency is roughly linear
  (5.1 ms → 20.8 ms from N=100 → 500). No super-linear knee — the microbench
  could not see the `ClientManager`/`DashMap` fan-out, and end-to-end it shows no
  O(N²) pathology.
- **The encode share — the headline.** The microbench made the byte-heavy encode
  look dominant (7.6 ms/broadcast at N=500). End-to-end, byte-heavy fan-out
  (22.9 ms) is only **~2 ms slower** than small (20.8 ms) at N=500 — the ~7 ms
  isolated encode difference **compresses to ~2 ms** once embedded in the real
  system, because the per-subscriber encodes overlap with socket I/O and spread
  across the multi-threaded runtime instead of running back-to-back on one core.
  Per-delivery is dominated by the socket write, scheduling, and client-side
  receive (~42 µs), not the encode (~1–15 µs isolated).

So the isolated encode wins do **not** translate proportionally. Improvement A's
realistic end-to-end payoff is **~9% for byte-heavy at high fan-out** (removing
~2 ms of a ~23 ms broadcast) and negligible for small payloads — worthwhile for
byte-heavy / bandwidth-bound workloads, not a critical hotspot. This vindicates
the original intuition that the socket write dominates at the system level.

### 6.3 Harness caveats

- **4-core VM.** 500 client tasks contend with the server for cores, so absolute
  per-delivery is inflated by client-side contention — not pure server fan-out.
  The **relative** small-vs-byte-heavy gap is the robust signal.
- **Latency, not sustained throughput.** Rounds are serialized (set → await full
  delivery → next), so `msgs/s` is the effective rate under isolated broadcasts,
  not pipelined load; true sustained throughput would be higher.
- **Poll granularity.** The driver busy-polls with `yield_now` (small fixed
  per-round overhead); p99 tails (up to ~150 ms) are scheduling pauses on the
  loaded VM.

---

## 7. Improvement A implemented and re-measured

Improvement A was implemented in `AimxCodec`: the `Outbound::Event` arm now
serializes the frame scaffolding (`t`/`seq`/`topic`/`sub`) and **splices the
record payload verbatim**, instead of validating it into a `RawValue` via
`as_raw` on every encode. The payload is trusted-valid (record-serializer
output), so the per-encode O(payload) validation was redundant — and on a
fan-out subscription it ran once *per subscriber* over the shared payload. The
change is byte-identical to the previous wire (asserted by
`event_encode_splices_data_byte_for_byte`), benefits every transport, and also
removes one payload copy.

### 7.1 Microbench — confirms the hoist landed

`naive` calls the real codec, so it now reflects the change:

| Shape (N=500) | naive before | naive after | validate_once |
|---|--:|--:|--:|
| small | 530 µs | 449 µs | 275 µs |
| byte-heavy | 7587 µs | **540 µs** | 504 µs |

Byte-heavy `naive` dropped **~14×** and now tracks `validate_once` — the
per-encode validation is gone. Small barely moved (validation was cheap there).

### 7.2 End-to-end socket harness — the real payoff

| Shape (N=500) | before | after | Δ |
|---|--:|--:|--:|
| small | 20.8 ms | 20.6 ms | ~noise |
| byte-heavy | 22.9 ms | 22.2 ms | ~0.7 ms (~3%) |
| byte-heavy − small gap | 2.1 ms | 1.6 ms | ~0.5 ms closed |

Per-delivery, byte-heavy improved ~1 µs (of ~44 µs). So the **14× microbench win
translates to ~3% end-to-end** — even below the ~9% H-A estimate. Removing the
encode validation closed only ~0.5 ms of the ~2 ms byte-heavy-vs-small gap; the
**remaining gap is payload size** (3.7 KB more to write and parse per delivery),
not encode. The improvement is real but small enough to sit near this VM's
run-to-run noise.

**Verdict:** Improvement A is a clean, byte-identical, safe change worth keeping
— it removes redundant work on *every* transport and cuts an allocation — but
its end-to-end fan-out benefit is marginal, consistent with H-A. It is justified
as hygiene, not as a fix for a hot path that turned out not to be hot.

---

## 8. Conclusions

1. **H-B: confirmed** — linear in both the isolated encode loop and, per §6, the
   real `ClientManager` bus end-to-end. No O(N²) pathology.
2. **H-C: confirmed, and isolated.** The `validate_once` variant splits the win:
   for byte-heavy, **~97% is the validation hoist (Improvement A)** and only ~3%
   is the shared-suffix structure (Improvement B); for small payloads the two
   are comparable (~56% / ~44%).
3. **H-A: answered — the isolated wins do not translate proportionally.** The
   encode is a minor fraction of end-to-end fan-out (§6): per-delivery is
   socket/scheduling-dominated, and the byte-heavy encode's ~7 ms isolated cost
   compresses to ~2 ms end-to-end. 500-client fan-out at ~21 ms/broadcast is
   comfortable for dashboard-style workloads.
4. **Improvement A implemented (§7).** The per-subscriber validation was removed
   from `AimxCodec` — a clean, byte-identical change. Microbench confirms it
   (byte-heavy encode ~14× faster), but end-to-end it moved fan-out only ~3%
   (~0.7 ms of ~23 ms), *below* the ~9% estimate: the residual byte-heavy penalty
   is payload size (bytes on the wire), not encode. Keep it as hygiene, not as a
   performance fix.
5. **Improvement B (shared-suffix) is not worth its complexity** given H-A and
   §7: its extra gain over A is small in isolation and vanishes end-to-end.

---

## 9. Threats to validity

- **Virtualized host with high run-to-run variance.** Docker Desktop VM; wide
  Criterion CIs, and absolutes shift materially between runs — small `naive`/500
  measured 1.24 ms in one run and 530 µs in another. Cross-run absolutes are
  unreliable; the decomposition uses a single run where all strategies were
  measured back-to-back, and the **proportions** (A vs B share) are the robust
  result, not the precise multipliers.
- **Excludes transport.** No socket write, scheduling, `ClientManager`, or
  backpressure — the microbench isolates encoding only.
- **Fast path trusts the payload.** `shared_suffix` performs no re-validation;
  `validate_once` performs exactly one validation per broadcast, which is the
  production-safe form of Improvement A. At N=1 that single validation can make
  `validate_once` trail `naive`; at realistic fan-out (N ≫ 1) it amortizes away.

---

## 10. Next steps

1. ~~**Isolate A vs B.**~~ Done — the `validate_once` strategy measures the split
   (§5.2): byte-heavy is ~97% validation hoist.
2. ~~**Run H-A.**~~ Done — the socket harness (§6) shows the encode is a minor
   end-to-end fraction.
3. ~~**Implement Improvement A.**~~ Done (§7) — implemented in `AimxCodec` as a
   verbatim payload splice (simpler than the anticipated once-per-broadcast
   plumbing, since the payload is trusted-valid), byte-identical, ~14× faster
   encode but only ~3% end-to-end. Kept as hygiene.
4. **Do not implement Improvement B (shared-suffix) on this evidence** — its
   marginal gain over A is small in isolation and negligible end-to-end (§6.2,
   §7.2). Revisit only if a concrete high-frequency small-payload exact-topic
   workload appears.
5. **Optional guardrail follow-up.** The splice trusts the record serializer's
   output; if defensiveness against a misconfigured custom serializer is wanted,
   validate once at the record-serialize / broadcast boundary rather than per
   encode — but this benchmark gives no performance reason to.
