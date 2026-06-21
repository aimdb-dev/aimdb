# 038 — Proof-Artifact & Story Roadmap

**Status:** Draft 2026-06-12. Companion to 037; sequences the claims-hardening artifacts from the June review against the content calendar. Publication anchor: **all stories release after the Bring-Your-Own-Connector (BYOC) and Bring-Your-Own-Adapter (BYOA) series.**

---

## 1. Operating principles

1. **Two clocks, decoupled.** Artifacts land on *engineering time* (breaking window, release gates, hardware availability). Stories publish on *content time* (after BYOC/BYOA, in queue order). Nothing about the publication order is allowed to delay an artifact whose engineering deadline is earlier.
2. **Artifact-gated, not calendar-gated.** A story ships when its §9-style numbers are merged — never before (037 §10). Calendar slots below are targets, not promises.
3. **The series must not rot on publish.** BYOC and BYOA are SPI documentation. They get written against the SPI that will exist *after* the open breaking window closes — see §2.

## 2. Hard constraints on the series itself

- **BYOA teaches the post-W8 SPI.** W8 (037) changes `BufferReader` from boxed-future `recv` to `poll_recv`, and it must land inside the currently-open breaking window regardless of content plans. Writing the adapter series against the old SPI means it documents a deleted trait within weeks. Upside: `poll_recv` is the *better tutorial* — "implement one poll function, register the waker before `Pending`" is a cleaner adapter contract to teach than "return a `Pin<Box<dyn Future>>`", and the series quietly sets up story S1.
- **BYOC is already stable.** The connector SPI post-W1 (fused `SerializedSource` + sync ingest closures, #141) is the shape to document. One recommendation to keep it stable: when W8 propagates (037 §3.3), keep `SerializedSource`'s **async wrapper** rather than exposing a poll method on the connector SPI — connector authors' world then doesn't change, and the BYOC series survives W8 untouched.
- **Conformance suite runs early even though its story ships late.** First-run divergences between the Tokio/Embassy/WASM buffer implementations may require *behavioral* fixes — and behavioral fixes may be breaking. Discovering that after the window closes is the expensive version. Build and run the suite now (it reuses the W6 `host_test_stubs!` infra); publish S3 whenever the queue reaches it.

## 3. Phase 0 — lands now, silently (before/during the series)

| Item | Why now | Story attached |
|---|---|---|
| W8 + B0/B1/B2 baselines (037 §8) | Breaking window; "before" columns captured pre-merge | S1, later |
| Conformance suite first run | Divergences may need the window (§2) | S3, later |
| `#![forbid(unsafe_code)]` in `aimdb-core` | Trivial; enables the S5 claim | S5, later |
| Size job in CI (cargo-size/bloat per feature set, thumbv7em) | Defends ~50 KB before anyone attacks it | S4, later |
| Wording fixes: executor "zero dependencies" → "no required dependencies"; drop "real-time" until S7; serial + UDS rows in the connector table; retire the low-count star badge | Cheap correctness; none of these are stories | — |

**One hardware day, two stories.** The next W3 rig session should cover *both* the remaining KNX matrix scenarios (soak, queue-flush, stale-channel NACK, backoff pacing, inbound flood, clean detection number) *and* the B3 before/after capture (DWT cycles + heap high-water, main vs W8 branch). That single session completes the artifact gates for S1 and S2.

## 4. Phase 1–2 — the series (Alex's existing plan)

BYOC, then BYOA, on their own schedule. Each installment may close with a one-line forward tease ("the adapter contract is one poll function — in a few weeks we'll show what that bought us, with cycle counts"), but the series carries no claims that the Phase-0 artifacts haven't already locked in.

## 5. Phase 3 — the story queue (post-series, in order)

| # | Story | Hook | Artifact gate | Channel |
|---|---|---|---|---|
| S1 | **One heap allocation per message — found, measured, removed** | `WasmRecvFuture` boxed solely to satisfy a trait signature; object safety vs `async fn`; declined the monomorphized lane *with data* | W8 merged; 037 §9 populated incl. B3 | r/rust (TWIR follow) |
| S2 | **Our hardware matrix caught silent data loss before release** | Ten KNX writes vanishing behind warn-only AckTimeouts; spec-3.8.4 retransmit; same-day hardware re-validation | W3 matrix complete (release gate anyway) | r/embedded + blog; repurposed for DACH pilot decks & LinkedIn |
| S3 | **Three runtimes, one semantics suite** | Proving "same buffer semantics on Tokio/Embassy/WASM" instead of asserting it; what diverged on first run | Suite in CI; divergences fixed or documented | r/rust |
| S4 | **What a typed dataplane costs on a Cortex-M** | Binary-size archaeology: what actually dominates flash (fmt? panic infra? per-record monomorphization?); the honest ~50 KB breakdown | Size job + bloat analysis per feature set | embedded Rust; feeds the `Reader<T,B>` dormant trigger and grant TRL credibility |
| S5 | **Proving the unsafe away** | `forbid(unsafe_code)` core; Miri on the WASM/Embassy `Send+Sync` impls; loom on the W8 Mailbox waker we just hand-rolled | Miri + loom jobs green in CI | r/rust |
| S6 | **Schema evolution without a registry — proven** | proptest round-trips (up∘down = identity), old-reader/new-writer matrix | Property suite merged | r/rust + distributed-systems |
| S7 | **Earning the word "real-time" back** | p99/p99.9 publish→wake jitter on Embassy, measured on the B3 rig; soft-RT claims with numbers or not at all | B3 infra + jitter harness; long soak | embedded; gated last deliberately |

Parallel, different channel, existing plan: the MCP live-demo **Show HN** rides its own track and is unaffected by this queue.

## 6. Cadence and sequencing notes

- Suggested rhythm: one story per 2–3 weeks post-series, S1 first as the BYOA segue. S2 is independent and may interleave anywhere once the matrix closes.
- If an artifact slips, the queue *reorders* rather than waits — every story is self-contained by design.
- Each published story closes the loop in-repo: README/website wording updated in the same PR as the story's numbers (037 §8 rule, generalized), and the claim moves from "asserted" to "CI-enforced" in whatever doc owns it.
- After S7, the original audit is fully discharged: every public claim is either CI-enforced, hardware-measured, or deleted.
