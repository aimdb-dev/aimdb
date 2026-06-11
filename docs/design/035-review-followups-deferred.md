# 035 — Review Follow-ups: Deferred Items & Embassy Hardware Validation

**Status:** Draft
**Predecessor:** [034 — Technical Debt & Architecture Review](034-technical-debt-review.md); code review of the #131/#135 refactor (commit `9152b82` + fix round)
**Scope:** The five findings the review fix round deliberately did *not* change, why, and what it takes to resolve each. Includes the hardware validation plan for the one item that was blocked on it (hardware is now available: Nucleo-H563ZI + KNX/IP gateway).

---

## 1. Context

The review of the de-genericization refactor fixed 15 findings (heartbeat-response
liveness, build-time error collection, ACK tracking, backoff pacing, doc rot, …).
Five findings were left as-is because each needed either a runtime environment we
couldn't exercise, or a maintainer decision. This doc closes the loop: per item, the
current state, the proposed resolution, and the validation it needs.

## 2. Items

### 2.1 Embassy select-loop connected/disconnected fork — **restructure applied; §3 bench validation pending**

**Current state.** `embassy_client.rs::drive_connection` forks the entire select on
connectivity (`if engine.is_connected() { select3(recv, cmd, deadline) } else { select(recv, deadline) }`,
[embassy_client.rs:432](../../aimdb-knx-connector/src/embassy_client.rs)), duplicating the
datagram Ok/Err handling across both branches. The tokio shim expresses the same
policy as an arm guard. Because both shims gate command intake on connectivity,
`TunnelEngine::handle_command`'s disconnected drop path is dead code in production —
yet both shims carry a "Not connected, dropping GroupWrite" warning for it.

**Risk.** The two branches can drift silently (e.g. a future edit drains commands in
the disconnected branch and starts dropping writes during a backoff window while
tokio still queues them) — exactly the divergence the sans-io extraction (#135) was
meant to end.

**Proposed fix — option A (minimal, recommended).** Collapse the fork into one
select with a conditional *arm future* instead of a conditional *select shape*:

```rust
let cmd_arm = match engine.is_connected() {
    true => Either::Left(command_channel.receive()),
    false => Either::Right(pending()),   // commands stay queued in the channel
};
match select3(socket.recv_from(&mut recv_buf), cmd_arm, deadline).await { … }
```

One select, one datagram handler, identical queue-while-disconnected semantics
(commands keep buffering in the static channel, flush on reconnect). The engine's
drop path then stays dead by construction on both runtimes: demote the shim
warnings, and document `handle_command`'s `false` return as a defensive contract
(covered by the engine unit test only).

*Implementation note:* the landed code expresses the conditional arm as a plain
`async` block over a pre-evaluated `connected` flag instead of a future `Either`
— same semantics, no extra import; the shims' "dropping GroupWrite" warnings are
removed and the contract is documented on `handle_command` itself.

**Option B (deeper, not recommended now).** Move the policy into the engine:
`handle_command` queues bounded commands during `Backoff`/`Connecting` and flushes
on connect. Rejected for now — it duplicates buffering the channel already provides
and adds per-connection RAM on the MCU for no behavioral gain.

**Why it was deferred.** The select shape is the hot heart of the MCU connection
task; `embassy_futures::select` polling semantics around a `pending()` arm and
cancellation of `Channel::receive` are exactly the kind of thing host tests don't
prove. §3 is the validation plan.

### 2.2 `Action::Send` 278-byte inline frame — **keep**

`Action::Send { frame: heapless::Vec<u8, 278>, … }` dominates the action enum's
size; every protocol event memcpys ~290 bytes through the `VecDeque<Action>`,
including 10-byte ACKs. This is an explicitly documented design choice
([tunnel.rs:72-75](../../aimdb-knx-connector/src/tunnel.rs)): frames stay
stack-allocated so the per-datagram path never touches the heap, and the queue
holds 0–2 entries in practice. The old tokio client heap-allocated every frame via
`.to_vec()`, so the net delta vs. pre-refactor is favorable at KNX telegram rates
(tens/sec, not thousands).

**Revisit trigger:** only if MCU profiling shows the memcpy or the queue's ~290-byte
slot footprint in a flame graph. The alternative is known and mechanical: semantic
actions (`SendAck { channel_id, seq }`, `SendHeartbeat { … }`) with byte-building
moved into `drain_actions` at send time.

### 2.3 Buffer-ext trait triplication (tokio/embassy/wasm) — **keep, with a tripwire**

`TokioRecordRegistrarExt` / `EmbassyRecordRegistrarExt` / `WasmRecordRegistrarExt`
each declare `.buffer(cfg)` on the *same* receiver `RecordRegistrar<'_, T>` with
near-identical bodies. This is a deliberate outcome of deleting `ext_macros.rs`
(#131): buffer construction is the one genuinely adapter-specific registration step
left, and a per-crate trait is how Rust method resolution scopes it.

**Known latent hazard:** if one binary ever links two adapters and imports both
traits, `reg.buffer(cfg)` is E0034-ambiguous — and since the registrar is now
runtime-erased, a fully-qualified call could install adapter A's buffer in a
database driven by adapter B with no type-level guard. No in-tree binary does this
today (verified by grep; embassy's impl is additionally cfg-gated off std).

**Resolution:** leave as-is. If a dual-adapter binary becomes real (e.g. wasm
native-fallback + tokio), introduce a `BufferFactory<T>` hook in `aimdb-executor`
at that point, so each adapter contributes only its constructor expression. Don't
build the abstraction ahead of the consumer.

### 2.4 defmt `#[global_logger]` stub duplication in host tests — **mitigate with an exported macro**

The 18-line no-op `#[defmt::global_logger]` + `#[defmt::panic_handler]` block is
copy-pasted into `aimdb-embassy-adapter/tests/session_smoke.rs`,
`aimdb-serial-connector/tests/embassy_smoke.rs`, and (pre-existing) the adapter's
`buffer.rs` test module; serial-connector carries a `defmt` dev-dependency solely
for it. Root cause (verified): coercing `EmbassyAdapter` to `Arc<dyn RuntimeOps>`
instantiates a vtable whose `log` entry calls `defmt::*` unconditionally, so the
`_defmt_*` extern symbols must resolve in every host test binary that touches the
adapter — and `#[global_logger]` must be defined **once per binary**, so a shared
`#[cfg(test)]` item in the lib cannot serve integration-test binaries.

**Resolution:** a `#[macro_export] macro_rules! host_test_stubs` in
`aimdb-embassy-adapter` (doc(hidden)) that expands to the no-op logger, panic
handler, and the pinned-at-0 time-driver stub. Each test binary invokes the macro
once — per-binary uniqueness is preserved because the *expansion* is per-binary,
while the definition lives in one place. Alternative (bigger): feature-gate the
adapter's `Logger` impl so host builds link a no-op without defmt at all; rejected
because it forks the adapter's log path per target, which is its own drift hazard.

### 2.5 `AimDb::runtime()` — **deleted**

`AimDb` exposes three runtime accessors; `runtime() -> &dyn RuntimeOps`
([builder.rs:1120](../../aimdb-core/src/builder.rs)) has **zero callers** across
aimdb and aimdb-pro (all uses go through `runtime_ops()` / `runtime_ctx()`).
Recommendation: delete it **in this release** — the window is already breaking
(#131), and once published the accessor is semver-frozen forever. If a borrowed
flavor is ever wanted back, `&*db.runtime_ops()` already provides it. One-line
removal + changelog entry; kept out of the fix round only because removing public
API is the maintainer's call.

## 3. Hardware validation plan (item 2.1 + this fix round's KNX changes)

Hardware: **Nucleo-H563ZI** (the in-tree embassy demo target,
`examples/embassy-knx-connector-demo`, Ethernet) + a KNX/IP gateway on the same
LAN. One session validates both the 2.1 restructure *and* the behavior changes
already in the working tree that host tests can't fully prove (heartbeat-response
timeout, backoff socket pacing, send-failure untracking on embassy-net).

Build/flash: `cargo build -p embassy-knx-connector-demo --target thumbv8m.main-none-eabihf`
(demo's own target config), defmt logs via RTT.

| # | Scenario | Steps | Pass criteria |
|---|----------|-------|---------------|
| 1 | Baseline soak | Boot, let the demo connect and exchange telegrams for 30 min | Stable channel; heartbeats every 55 s each answered; zero AckTimeout warnings |
| 2 | Queue-while-disconnected | Power-cycle the gateway; issue 3–5 GroupWrites while it's down (≤ channel depth 32); restore | No "dropping GroupWrite" log; all queued writes appear on the bus after re-handshake, in order |
| 3 | Heartbeat-response timeout | Block UDP 3671 gateway→device (or unplug gateway *after* handshake, keeping link up via a switch) | Within ≤ 65 s (heartbeat cadence + 10 s timeout): defmt shows reset + backoff; on unblock, reconnect with a fresh channel id and traffic resumes |
| 4 | Stale-channel NACK | Restart the gateway quickly so it forgets the channel but answers heartbeats with non-zero status | Engine reconnects on the NACKed CONNECTIONSTATE_RESPONSE instead of staying Connected |
| 5 | Backoff pacing | Point the demo at a non-existent gateway IP | Exactly one CONNECT_REQUEST per 5 s backoff window (sniff with Wireshark); no rebind storm, CPU idle between attempts |
| 6 | Send-failure path | While connected, briefly bring the interface down (`embassy-net` route loss) during writes | "KNX send failed" logged, **no** AckTimeout for those frames; recovery via scenario-3 mechanism |
| 7 | Inbound flood | Burst group writes from ETS/knxd toward the device | Telegrams routed or drop-logged (channel full); protocol loop never stalls; heartbeats stay on schedule |

Run the matrix twice: once on current working tree (regression baseline for this
fix round), once with the 2.1 restructure applied. Identical defmt traces (modulo
timestamps) for scenarios 1–7 is the acceptance bar for 2.1; tokio needs no re-run
(its arm-guard shape is unchanged and host-tested against the fake gateway).

## 4. Suggested order

1. **2.1** restructure + §3 matrix (hardware is available now; one bench session).
2. **2.5** delete `AimDb::runtime()` while the breaking window is open (one line, zero risk).
3. **2.4** `host_test_stubs!` macro next time an embassy host test is added (third copy is the trigger).
4. **2.2 / 2.3** no action; revisit triggers documented above.

> **Successor:** the still-open items (2.1's §3 validation, 2.4) and the dormant triggers (2.2, 2.3) are carried forward in [036 — Follow-up Refactoring](036-followup-refactoring.md), the single live follow-up list. 2.1's restructure and 2.5 landed in PR #140.
