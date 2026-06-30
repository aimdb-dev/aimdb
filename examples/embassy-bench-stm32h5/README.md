# embassy-bench-stm32h5 — AimDB B3 on-target profiling

The **B3** tier of the AimDB benchmark suite (design [038](../../docs/design/038-aimdb-bench-crate-design.md)
§B3 / §Phase 4): the measurements that **cannot run on a host**. It reads the
Cortex-M **DWT cycle counter** (`CYCCNT`) to report the real per-message cost, in
CPU cycles, of the AimDB Embassy buffer `push` → `recv` consume path on an
**STM32H563ZI** (Cortex-M33 @ 250 MHz), and re-validates the W8 zero-allocation
claim against the real embedded allocator (`embedded-alloc`).

The host-runnable tiers — **B0** (allocations), **B1** (latency), **B2**
(throughput) — live in the [`aimdb-bench`](../../aimdb-bench) crate and exercise
the same Embassy buffer backend on the host via `futures::executor::block_on`.
This crate is the on-hardware complement, not a replacement.

## What it measures

For each AimDB buffer profile it runs a tight, lockstep `push`→`recv` loop after
a warmup and reports per message:

| Profile     | Backend        | embassy-sync primitive |
|-------------|----------------|------------------------|
| Telemetry   | `SpmcRing`     | `PubSubChannel`        |
| State       | `SingleLatest` | `Watch`                |
| Command     | `Mailbox`      | `Channel<_, _, 1>`     |
| Telemetry ×4| `SpmcRing` 1→4 | `PubSubChannel`        |

Each line reports **cycles/msg** (CYCCNT delta ÷ batch) and **allocs/msg**
(global-allocator calls ÷ batch). The target is **0 allocs/msg** — the same W8
goal the host B0 suite gates on.

## Hardware

- **Board:** ST Nucleo-H563ZI (STM32H563ZI, Cortex-M33).
- **Probe:** the onboard ST-LINK (SWD + RTT). No extra wiring — defmt logs stream
  over RTT on the same USB cable you flash with.

## Running

From **this directory** (the local `.cargo/config.toml` selects the
`thumbv8m.main-none-eabihf` target and the `probe-rs` runner):

```bash
cargo run --release
```

`--release` matters: debug vs release cycle counts differ by an order of
magnitude (design 038 §15.8). Always record the build profile with a baseline.

### Flashing from outside the dev container

If `probe-rs` and the ST-LINK live on your host (not in the container), build in
the container and flash from the host with [`flash.sh`](flash.sh):

```bash
# In the dev container:
cd examples/embassy-bench-stm32h5 && cargo build --release

# On the host (where the ST-LINK is attached):
cd examples/embassy-bench-stm32h5 && ./flash.sh
```

`flash.sh` prefers the `--release` binary and falls back to debug with a warning;
it runs `probe-rs run --chip STM32H563ZITx`, so B3 results stream over RTT to
your terminal.

First capture on a Nucleo-H563ZI @ 250 MHz (release), recorded in
[`aimdb-bench/data/baselines/b3_cycles_stm32h5.json`](../../aimdb-bench/data/baselines/b3_cycles_stm32h5.json):

```
=== AimDB B3 — Embassy buffer profiling on STM32H563ZI @ 250 MHz ===
cycle_counter=true  warmup=200  batch=512
[B3] Telemetry SpmcRing    : 2013 cycles/msg, 0 allocs/msg  (1030839 cycles total, batch=512)
[B3] State     SingleLatest: 2009 cycles/msg, 0 allocs/msg  (1029028 cycles total, batch=512)
[B3] Command   Mailbox     : 1661 cycles/msg, 0 allocs/msg  (850440 cycles total, batch=512)
[B3] Telemetry SpmcRing(1->4): 6239 cycles/msg, 0 allocs/msg  (4 deliveries/msg, 3194799 cycles total, batch=512)
=== B3 complete — target=0 allocs/msg (W8 zero-alloc consume path) ===
```

Command (single-slot `Channel`) is cheapest; the 1→4 fan-out is sub-linear
(~1560 cycles/delivery) since the single `push` is amortized across four reads.
Treat these as the regression reference; re-run 2–3× and update the baseline JSON
if a stable average drifts.

## Notes & caveats

- **Measurement window** excludes warmup. The one-time reader `Box`/lazy
  `SpmcRing` subscriber registration happens during warmup, so the measured
  window reflects steady-state per-message cost only.
- **Payload construction is inside the timed loop**, identical to the host B1/B2
  suites, so the figure is the end-to-end per-message consume cost (not the
  buffer call alone).
- **Clock governor / frequency:** DWT cycle counts assume the 250 MHz PLL1
  config in `main.rs`. Record the baseline at a fixed clock (design 038 §15.6).
- **CI compile check** uses `thumbv7em-none-eabihf` (the workspace's installed
  embedded triple, see the `examples` Make target), matching the other H563
  demos; the flashable artifact is the `thumbv8m.main-none-eabihf` build above.

## Troubleshooting

**`└─ <mod path> @ <invalid location: defmt frame-index: N>`** after each log line.
The `[B3] …` messages themselves decode fine; only the file:line annotations are
missing. The firmware is correct — it emits the same defmt 1.0 metadata
(`_defmt_version_ = 4`, `.defmt` + `.symtab` present) as every other embassy
example here, in both debug and release. The cause is host-side: the
`defmt-decoder` bundled in your `probe-rs` decodes defmt-1.0 message payloads but
not its location table. It affects any defmt-1.0 binary in this repo, not just
this one — flash another example (e.g. `embassy-serial-connector-demo`) to
confirm the same annotations appear.

Fix: update `probe-rs` to a current release (its decoder is updated in lockstep
with defmt) — `probe-rs --version`, then reinstall via your usual method (e.g.
`cargo install probe-rs-tools --locked`). It is purely cosmetic for B3: the
cycle/alloc numbers are unaffected.
