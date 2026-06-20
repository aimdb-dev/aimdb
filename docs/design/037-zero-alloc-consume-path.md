# 037 — Zero-Allocation Consume Path: Poll-Based `BufferReader` SPI (W8)

**Status:** Implemented 2026-06-20 (pending review), stacked on the `aimdb-bench` crate (design 038). Builds on the 034/035/036 review cycle and the #141–#147 series. **Must land inside the currently-open breaking window** (same release as the W1/W2 SPI breaks) or it waits for the next major. Host B0–B2 measured (§9); B3 on-target and embassy-host B0 are follow-ups.

---

## 1. Where this sits

036 W1 (PR #141) removed the per-message `dyn Any` erasure from the connector, session-pump, and AimX paths. Its acceptance criterion was a `dyn Any` grep — which structurally cannot see boxed *futures*. Post-W1, exactly one AimDB-added per-message heap allocation remains: the `Pin<Box<dyn Future>>` constructed on every `recv()`.

The asymmetry is now stark:

| Direction | State after #141–#147 |
|---|---|
| Write path | Solved by 029: sync `push`, pre-bound handle, one vtable call, zero alloc |
| Inbound (connector → record) | Solved by W1: fused **sync** typed ingest closure, zero AimDB-added alloc |
| Consume path (record → consumer / connector / remote) | **One heap allocation per message, per reader** — this doc |

W8 closes the loop. End state: **zero AimDB-added heap allocations per message, end to end; no locks held across `await`; abstraction cost = one indirect call — enforced in CI, measured in `benches/`.**

## 2. Current state (verified, pr147 HEAD)

| Path | Mechanism | Per-message cost |
|---|---|---|
| In-process consumer | [`BufferReader::recv` → `Pin<Box<dyn Future>>`](../../aimdb-core/src/buffer/traits.rs#L142); `Box::pin` at [tokio `buffer.rs:283`](../../aimdb-tokio-adapter/src/buffer.rs#L283), [embassy `buffer.rs:416`](../../aimdb-embassy-adapter/src/buffer.rs#L416), [wasm `buffer.rs:210`](../../aimdb-wasm-adapter/src/buffer.rs#L210) | 1 heap box + 1 indirect call |
| Outbound connector | [`SerializedSource::recv` → `RecvSerializedFuture`](../../aimdb-core/src/connector.rs#L101) (W1 removed the value-level `Box<dyn Any>`; the future box stayed per the 036 W1 risk note "manual boxed-future pattern — keep it") | 1 heap box |
| Remote access / JSON | [`recv_json`](../../aimdb-core/src/buffer/traits.rs#L177) boxes around [`JsonReaderAdapter`](../../aimdb-core/src/typed_record.rs#L137)'s inner `recv()`, which boxes again | 2 heap boxes |
| Inbound | fused sync ingest (W1) | 0 |
| Produce | [`WriteHandle::push`](../../aimdb-core/src/buffer/traits.rs) sync | 0 (1 indirect call) |

Notable: the WASM adapter already owns a hand-rolled poll struct — `Box::pin(WasmRecvFuture { reader: self })`. The allocation exists **only to satisfy the trait signature**. The signature is the problem, not the implementations.

Out of scope, recorded in §7: the latest-snapshot [`spin::Mutex`](../../aimdb-core/src/typed_record.rs#L36) on the produce path.

## 3. Approach

### 3.1 The SPI change

Object safety and `async fn` conflict; object safety and `poll` do not. Replace the async method on the erased trait with its poll form:

```rust
pub trait BufferReader<T: Clone + Send>: Send {
    /// Poll for the next value. Registers `cx.waker()` when `Pending`.
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>>;

    fn try_recv(&mut self) -> Result<T, DbError>; // unchanged
}
```

The **consumer-facing API does not move**. `Reader<T>::recv()` remains `async`, implemented once:

```rust
pub async fn recv(&mut self) -> Result<T, DbError> {
    core::future::poll_fn(|cx| self.inner.poll_recv(cx)).await
}
```

`core::future::poll_fn` is stable (1.64) and lives in `core` — `no_std`-clean, zero allocation, no `unsafe`. Call sites in examples and aimdb-pro compile unmodified. Only `BufferReader` *implementors* break: an SPI break, in the window that #131/#135/#141 already opened.

### 3.2 Per adapter (in implementation order)

1. **WASM — unbox what already exists.** `WasmRecvFuture`'s poll body *becomes* `poll_recv`; delete the `Box::pin`. Smallest diff; do it first to validate the trait shape.
2. **Tokio Mailbox.** Currently `Mutex<slot>` + `Notify` ([`buffer.rs:8`](../../aimdb-tokio-adapter/src/buffer.rs#L8)). Replace `Notify` with waker storage beside the slot — single-slot take semantics is the textbook poll pattern. Drops the `Notify` permit subtleties entirely. Waker contract: see §6.
3. **Embassy.** Verified against the locked embassy-sync **0.8.0**: `Channel` exposes [`poll_receive(&self, cx)`](https://docs.rs/embassy-sync/0.8.0/embassy_sync/channel/struct.Channel.html) natively (channel.rs:332 in the crate source) and pubsub `Subscriber` implements `Stream` (`poll_next`). Direct mapping, zero stored state. If the Watch-backed path lacks a public poll fn, mirror its `changed()` poll body — embassy futures are hand-rolled poll structs; mechanical.
4. **Tokio Broadcast — the one residue.** `broadcast::Receiver` exposes no public poll API. Use the `BroadcastStream` technique: a `tokio_util::sync::ReusableBoxFuture` owned by the reader — **one allocation per subscriber lifetime, reused for every message**. This is a Tokio API limitation, not an AimDB design cost; documented as such.

### 3.3 Fused and remote paths inherit it

`SerializedSource` composes over `poll_recv` (subscribe → poll → serialize stays fused inside the registration closure); `RecvSerializedFuture` either becomes a poll method or keeps its async wrapper over the inner poll — either way the inner box is gone. `JsonBufferReader` collapses the same way; the remote-access **double** box disappears with no separate work item.

## 4. Measurement program — the centerpiece (lands *with* the change, not after)

W8 exists to make a claim provable; the proof ships in the same series. These benches instantiate the L0/L1 layers of the benchmark-pyramid plan for the consume path; the three canonical workload profiles — **Telemetry/`SpmcRing`**, **State/`SingleLatest`**, **Command/`Mailbox`** — are the unit of reporting throughout. New workspace member `benches/aimdb-bench` (criterion; host-only; dev-deps fenced from the `no_std` graph).

**B0 — Allocation count (hard gate + headline number).** Counting `#[global_allocator]` wrapper in dedicated host test binaries — the W6 `host_test_stubs!` infrastructure is the natural home for the embassy-host build. Protocol: set up producer + subscribers; warmup absorbs one-time setup (including the tokio-broadcast `ReusableBoxFuture`); snapshot the counter; push/consume N = 10 000; assert **and report** allocations/message. Expected: **1 → 0**, per buffer profile × {tokio, embassy-host}. Doubles as the CI gate (§5) and the first table row of any publication. WASM: covered by the unboxed `WasmRecvFuture` unit tests; alloc-counting under wasm32 in CI is not worth the harness (justified per the #146 compile-delete-or-justify rule).

**B1 — Leaf latency (L0).** Single-message publish→`recv`-return, ping-pong, **three-way per profile**: raw primitive (`broadcast` / `watch` / embassy `Channel` on host) vs AimDB-before vs AimDB-after. Tokio current-thread runtime, criterion `async_executor`, pinned core; report p50/p99 (criterion medians, never means). Two defined deltas, used consistently everywhere downstream: (after − raw) = **abstraction cost** — the README number; (before − after) = **what W8 bought** — the publication number.

**B2 — Steady-state throughput (L1).** msgs/sec at saturation, SPSC and 1→4 fan-out per profile. Fan-out deliberately exposes the `T: Clone` per-consumer copy cost — measured and reported, not hidden. Same three-way comparison as B1.

**B3 — On-target (Cortex-M).** The W3 KNX hardware rig already exists; reuse it. DWT `CYCCNT` around `recv` on the STM32H5, defmt-reported, N = 10 000: **cycles/message** before/after, plus the embedded-alloc **heap high-water mark** over the run — the fragmentation story no host bench can tell, and the number that makes the claim credible to the embedded audience. Ships as a flashable example with documented rig setup.

**Methodology constraints (so the numbers survive review).** Current-thread executors and pinned cores isolate leaf cost from scheduler noise; warmup excluded from samples; rustc version, criterion configuration, host CPU, and B3 rig recorded in §9 alongside the results; results are explicitly microbenchmark scope — **no system-throughput claims are derived from them**. Reproduction is one command (`cargo bench -p aimdb-bench`) against the committed lockfile.

**CI.** B0 is a required gate on PRs touching `aimdb-core/src/buffer/`, the adapter `buffer.rs` files, or `connector.rs`. B1/B2 run on the same trigger but report as trend only — CI runners are too noisy to gate on nanoseconds. B3 is a release-checklist item, not CI.

## 5. Acceptance criteria

- [x] `grep -rn "Box::pin" aimdb-{tokio,embassy,wasm}-adapter/src/buffer.rs aimdb-core/src/buffer/ aimdb-core/src/connector.rs` → zero hits on per-message construction sites. (Remaining hits are doc comments only: the wasm reader's note about the *removed* box, and the connector.rs BYOC doc example. The reused futures live in `ReusableBoxFuture::new` at subscribe time — tokio broadcast/watch — and the embassy `no_std` equivalent, all per-subscriber, not per-message.)
- [x] `BufferReader::recv`/`JsonBufferReader::recv_json` (boxed-future form) deleted; `poll_recv`/`poll_recv_json` object-safe; `thumbv7em-none-eabihf` and `wasm32-unknown-unknown` builds + clippy green.
- [x] **B0: 0 allocations/message** across the 3 tokio buffer profiles (was 1). Committed to `aimdb-bench/data/baselines/b0_alloc_tokio.json`. CI wiring is **advisory (report-only) per design 038 §6**; a hard gate and the embassy-host adapter B0 are the documented follow-up.
- [x] Examples and aimdb-pro compile with **no call-site changes** (`subscribe().recv().await` unchanged; `Consumer`/`AimDb`/`TypedRecord::subscribe` now return `Reader<T>`). Concrete-reader holders wrap once via `Reader::new(Box::new(..))`.
- [x] SPI break recorded in `aimdb-core` CHANGELOG; ships in the same release as the W1/W2 breaks.
- [x] §9 populated for B0–B2 (host), rustc + criterion recorded. **B3 (on-target, STM32H5) deferred** to a hardware session per §8/§4.

## 6. Risk notes

- **Cancellation semantics on tokio broadcast.** The in-flight inner `Recv` future now persists across caller polls (owned `ReusableBoxFuture`) instead of being dropped per call. Strictly fewer lost-wakeup hazards; behavioral note: a value claimed by the inner future just before caller cancellation is delivered on the *next* poll rather than dropped — consistent with broadcast cursor semantics. Document on the reader.
- **Lagged mapping** unchanged: `Lagged(n)` → `DbError::BufferLagged(n)`; pinned by existing adapter tests.
- **Mailbox waker contract.** Single-slot take semantics with potentially multiple readers: store wakers, wake-**all** on push (spurious wakeups are benign; losers re-poll to `Pending`). Wake-one is an optimization with a starvation analysis attached — not now.
- **Auto-traits.** The `poll_fn` future is `Send` iff the reader is; verify at the session-pump and connector spawn sites (compiler enforces; listed so the error is expected, not surprising).
- **Embassy Watch poll surface** unverified on 0.8.0 — confirm during step 3; fallback is the mechanical mirror noted in §3.2.

## 7. Dormant items (trigger-only)

| Item | Decision | Re-open trigger |
|---|---|---|
| Generic fast lane `Reader<T, B>` / `Producer<T, B>` (default type param keeps `Reader<T>` = boxed lane; adapter seam already exists: [`subscribe() -> Self::Reader`](../../aimdb-tokio-adapter/src/buffer.rs#L97) vs [`subscribe_boxed()`](../../aimdb-tokio-adapter/src/buffer.rs#L128)) | Not shipped. Post-W8 the dyn lane costs one indirect call and zero allocs; monomorphization stamps `T×B` copies against the ~50 KB flash budget | §9 numbers (B1/B3) or an MCU flame graph show the per-message indirect call as a measurable fraction of a real workload's budget |
| Latest-snapshot `spin::Mutex` → atomic ptr swap (portable-atomic) | Keep — bounded, tiny critical section on produce only | Producer-side profiling shows it, or a hard "no spinlocks" claim is wanted for marketing |

## 8. Sequencing and size

1. **B0–B2 scaffolding first**; baseline numbers on current HEAD committed (the "before" columns). B3 baseline captured on the W3 rig — fold into the next scheduled hardware session.
2. WASM unbox → Tokio Mailbox waker rewrite → Embassy `poll_receive` mapping → Tokio broadcast `ReusableBoxFuture`.
3. Flip the trait, delete `recv()`, migrate `SerializedSource` + JSON paths, fix fallout.
4. Gates into CI; populate §9 after-columns; status row in 036 §5 pointing here; README/website wording PR ("zero allocations per message, end to end; overhead: one indirect call, X ns") in the same series as the after-numbers — never before.

**Size:** M — wide but mechanical (three adapters + two fused paths). The only design-sensitive piece is the Mailbox waker contract (§6).

## 9. Results (populated by §4)

**Environment:** rustc `1.91.1` · criterion `0.5` · host CPU `dev container (shared, noisy — host B1/B2 are indicative trend only, not gated)` · B3: STM32H5 @ `—` MHz, embassy-executor `—`, embedded-alloc `—` (deferred follow-up).

**Host (B0–B2), measured 2026-06-20 via `aimdb-bench`.** B0 is the gate and headline; B1/B2 are after-W8 medians on a shared container (the three-way raw/before split and embassy-host B0 are deferred to the embassy follow-up per scope).

| Profile | allocs/msg (B0) before → **after** | p50 (B1) after | msgs/s (B2) after |
|---|---|---|---|
| Telemetry — Tokio SpmcRing (`broadcast`) | 1 → **0** | ~195 ns | ~5.2 M/s |
| State — Tokio SingleLatest (`watch`) | 1 → **0** | ~446 ns | ~2.5 M/s |
| Command — Tokio Mailbox | 1 → **0** | ~70 ns | ~13.6 M/s |
| Telemetry — Embassy Channel (host) | deferred (embassy-host B0 follow-up) | — | — |

**Headline:** the last AimDB-added per-message heap allocation on the in-process consume path is removed — **1 → 0 allocs/msg** on every tokio buffer profile, byte total 0 in the measured window. Abstraction cost is one indirect call per `recv` (the boxed `Reader<T>` lane). Bytes/msg before-W8 was 144 B (the `Box::pin(async move …)` future) → 0 B after.

**On-target (B3, STM32H5):**

| Metric | Before | After |
|---|---|---|
| cycles/message (DWT `CYCCNT`) | — | — |
| heap high-water mark, 10 k msgs | — | — |
| allocations/message | — | — |

## 10. Publication note (r/rust)

The §9 tables are the post's spine; **the post follows the merged numbers, never precedes them**. Framing is purely technical per the established channel split: the find (`WasmRecvFuture` boxed solely to satisfy a trait signature), the mechanism (object safety vs `async fn`; poll as the escape that keeps `dyn`), the trade analysis (the box was deadweight, not part of the dyn trade — and the monomorphized `Reader<T, B>` lane declined *with data*, §7), the B0–B3 numbers, and the one-command reproduction path with links to the PR and this doc. No claims beyond measured scope. Declining an optimization on the basis of measurements is itself the credibility move for that audience — it belongs in the post, not just the doc.
