# 040 — WASM SingleLatest Parity: Fresh Subscribers Must Observe the Current Value

**Status:** Proposed 2026-07-05. Companion to 039; both follow the claims assessment. Fixes the one observable buffer-semantics divergence between the three runtime adapters, and adds the shared contract test that prevents the class of bug from recurring.

---

## 1. Problem

The cross-runtime promise is "same buffer semantics on every runtime". For `SingleLatest`, Tokio and Embassy agree: **a fresh subscriber observes the buffer's current value once, as if it had been pushed right after subscription; if nothing has been pushed yet, it waits.** The WASM adapter diverges: a fresh subscriber sees *nothing* until the **next** push.

Concretely, in the browser: a dashboard that subscribes to `config.theme` after the producer has already published renders blank until the config changes again. The same code on Tokio or Embassy renders immediately.

This matters beyond the bug itself: it is the counterexample to the "deploy the same buffer semantics across runtimes" claim, and it exists because SingleLatest's fresh-subscriber semantics are adapter folklore — stated only in a Tokio adapter comment — rather than a documented, conformance-tested contract.

## 2. Current behavior

| Adapter | Fresh subscriber, value already published | Fresh subscriber, nothing published yet | Mechanism |
|---|---|---|---|
| Tokio | ✅ delivers current value once | ✅ waits (`Pending` / `BufferEmpty`) | `subscribe()` calls `rx.mark_changed()`; `watch_recv` loops past the empty (`None`) slot ([`buffer.rs:175-189, 322-338`](../../aimdb-tokio-adapter/src/buffer.rs)) |
| Embassy | ✅ delivers current value once | ✅ waits | native `embassy_sync::watch` behavior: receivers track a seen-message ID starting at 0, so the current value counts as unseen — the Tokio arm was written to match it (comment at [`buffer.rs:176-182`](../../aimdb-tokio-adapter/src/buffer.rs)) |
| WASM | ❌ **waits for the *next* push** | ✅ waits | `subscribe()` initializes the reader already caught up ([`buffer.rs:148-153`](../../aimdb-wasm-adapter/src/buffer.rs)) |

The Tokio behavior is pinned by regression tests (`test_watch_fresh_subscriber_sees_current_value`, `test_watch_fresh_subscriber_no_push_yet_does_not_error`, `..._poll_recv_pending` in [`aimdb-tokio-adapter/src/buffer.rs`](../../aimdb-tokio-adapter/src/buffer.rs)). The WASM adapter has **no buffer unit tests at all** — its only test exercises `transform_join` end to end — which is how the divergence survived.

## 3. Root cause

One line. `WasmBuffer::subscribe()` snapshots the buffer's current version as the reader's starting point:

```rust
// aimdb-wasm-adapter/src/buffer.rs:148-153 (today)
WasmBufferInner::SingleLatest { version, .. } => {
    // Will fire on next push (version change).
    ReaderState::SingleLatest { last_seen_version: *version }
}
```

`try_recv` then treats `version == last_seen_version` as `BufferEmpty` ([`buffer.rs:272-286`](../../aimdb-wasm-adapter/src/buffer.rs)), so the value published *before* subscribe is never delivered.

## 4. Change set

### W1 — The fix (one line)

Initialize the reader as never-having-seen-anything:

```rust
WasmBufferInner::SingleLatest { .. } => {
    // A fresh subscriber observes the current value once, as if it had
    // been pushed after subscription — matches tokio (`mark_changed()`)
    // and embassy's native watch::Receiver behavior.
    ReaderState::SingleLatest { last_seen_version: 0 }
}
```

Correctness against the buffer's invariants (`version` starts at 0 and increments on every push; `value` is `Some` iff `version >= 1`):

- **Nothing published yet:** `version == 0 == last_seen_version` → `BufferEmpty` → `poll_recv` parks. Matches Tokio/Embassy.
- **Value already published:** `version >= 1 != 0` → delivers the current value once and advances `last_seen_version` to `version`. Matches Tokio/Embassy.
- **Subsequent reads:** versions equal → waits for the next push. Unchanged.
- The existing `None`-value guard in `try_recv` becomes unreachable (kept as a defensive branch, same shape as Tokio's `watch_recv` loop).

`u64` version wraparound is not a practical concern (a push per nanosecond for ~585 years).

### W2 — `peek()` parity (same theme, ~10 lines)

Found during the same analysis: WASM's `DynBuffer` impl ([`buffer.rs:164-177`](../../aimdb-wasm-adapter/src/buffer.rs)) does not override `peek()`, so the buffer-native non-destructive read used by AimX `record.get` returns `None` on WASM for SingleLatest **and** Mailbox, while Tokio ([`buffer.rs:218-227`](../../aimdb-tokio-adapter/src/buffer.rs)) and Embassy ([`buffer.rs:275-288`](../../aimdb-embassy-adapter/src/buffer.rs)) both implement it. Add the override:

```rust
fn peek(&self) -> Option<T> {
    match &*self.inner.borrow() {
        WasmBufferInner::SingleLatest { value, .. } => value.clone(),
        WasmBufferInner::Mailbox { slot, .. } => slot.clone(),
        WasmBufferInner::SpmcRing { .. } => None, // no canonical latest — same as the other adapters
    }
}
```

### W3 — Regression tests, on the host

`WasmBuffer` is pure `core` + `alloc` (`Rc<RefCell<…>>`, no `wasm_bindgen` in the module), so buffer unit tests do **not** need `wasm-pack` or a headless browser — a plain `#[cfg(test)] mod tests` in `buffer.rs` runs under the workspace's normal `cargo test` loop, which already includes `aimdb-wasm-adapter`. Port the three Tokio fresh-subscriber regression tests plus a `peek()` suite mirroring Tokio's `peek_tests`. The existing `wasm-pack` browser lane stays as-is for the bindings/bridge layer.

### W4 — Codify the contract, then enforce it

Two parts, in order:

1. **Write the semantics down in core.** The fresh-subscriber rule currently lives in a Tokio adapter comment. Add it to the normative docs: `BufferCfg::SingleLatest` ([`cfg.rs`](../../aimdb-core/src/buffer/cfg.rs)) and `Buffer::subscribe` ([`traits.rs`](../../aimdb-core/src/buffer/traits.rs)) — "a fresh subscriber observes the current value once; if nothing has been produced yet it waits", alongside the existing poll/try_recv interleaving contract (`traits.rs:179-197`).
2. **Shared conformance suite.** `RuntimeOps` already has exactly this pattern: [`executor.rs`](../../aimdb-core/src/executor.rs) `test_support::assert_runtime_ops_contract`, invoked from each adapter's own test suite under its own executor. Mirror it for buffers: a `#[doc(hidden)] buffer::test_support` module in `aimdb-core` exposing `assert_single_latest_contract(mk: impl Fn() -> B)`, `assert_mailbox_contract(..)`, `assert_spmc_ring_contract(..)` — generic over `Buffer<T>`, async, executor-agnostic. Each adapter calls all three from `#[tokio::test]` / `block_on` / host-`#[test]` respectively. Initial scope: fresh-subscriber behavior, overwrite semantics, lag surfacing (`BufferLagged`), empty-buffer behavior, `peek()` where applicable. This is the structural fix — parity moves from discipline to CI.

## 5. Compatibility

- **Behavior change, WASM only:** JS/WASM subscribers (including `subscribe_typed` through the bindings and `WsBridge`) now receive an immediate callback with the current value when one exists. This is the documented cross-runtime semantics and what UI code almost always wants (render current state on mount). Code that assumed "future updates only" would observe one extra initial delivery.
- **Versioning:** `aimdb-wasm-adapter` 0.2.0 → 0.3.0 with a changelog entry describing the initial-delivery change. No API signature changes. Tokio/Embassy adapters and `aimdb-core` are untouched by W1–W3; W4 adds a `#[doc(hidden)]` test-support module to core (additive).

## 6. Risks

| Risk | Assessment |
|---|---|
| A WASM consumer double-handles the initial value (e.g. treats every delivery as a *transition*) | Only affects apps relying on the divergent behavior; called out in changelog. The other two runtimes already behave this way, so portable code already tolerates it. |
| Contract suite forces semantics an adapter can't express | Embassy's slot-exhaustion (`SUBS`/`WATCH_N` const generics) surfaces as `BufferClosed`; the suite must size fixtures within default limits, which the existing per-adapter tests already do. |
| Host-run tests miss browser-specific behavior | The buffer module has no browser dependency; browser-specific layers (bindings, `WsBridge`) keep their `wasm-pack` lane. |

## 7. Non-goals

- Aligning Embassy's const-generic buffer sizing surface (`.buffer(cfg)` capacity-mismatch panic, `WATCH_N=1` default) with the dynamic runtimes — separate, larger design.
- Close/`BufferClosed` parity for WASM and Embassy Mailbox on producer drop — candidate for a later extension of the W4 contract suite.
- SpmcRing backfill semantics (all three adapters already agree: no replay for late subscribers).

## 8. Acceptance criteria

1. On WASM, a `SingleLatest` subscriber created after a push receives the current value on its first `recv().await` / `try_recv()`; created before any push, it stays pending and does not error — verified by new host-run unit tests in `aimdb-wasm-adapter`.
2. `peek()` returns the current value for SingleLatest and the pending slot for Mailbox on WASM, matching Tokio/Embassy.
3. All three adapters pass the same `aimdb-core` buffer contract suite in their own test lanes.
4. `BufferCfg::SingleLatest` / `Buffer::subscribe` rustdoc states the fresh-subscriber rule.
5. Existing Tokio/Embassy buffer tests pass unchanged.
