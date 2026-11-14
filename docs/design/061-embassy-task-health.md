# 061 — Embassy task health

**Status:** 📝 Proposed, spiked (§12)

**Scope:** A `health` feature of `aimdb-embassy-adapter` that measures every
Embassy task through the executor's trace callbacks and serves the result as
AimDB records: how long each task waits to be polled, how long it holds the
executor, and which task is blocking the executor right now. It covers the
callbacks, task naming, the records, the report after a watchdog reset, and
an opt-in mode that runs AimDB on an `InterruptExecutor` so that it still
answers while a task blocks. Additive: no change to AimX or to existing
applications. A throwaway spike ran the design on the NUCLEO-H563ZI; §12
lists what it showed, and its findings are written into the sections they
concern. Three things outside the feature have to be fixed first (§7.1).

---

## 1. The problem

Embassy tasks share one executor and run until their next `await`. A task
that does not yield makes every other task on that executor late. The late
task is the one that gets noticed and debugged, and it is not the one at
fault. Nothing in the firmware names the task that held the executor.

Today this is found with a debug probe and a trace tool, on the desk. A
board in the field has neither.

> The firmware reports, by name, which task holds the executor and which
> tasks wait for it, over the link it already has, without a debug probe.

```
$ aimdb --connect serial:///dev/ttyACM0?baud=115200 record get mcu.blocked
{
  "task": "flash_task",
  "executor": 0,
  "held_ms": 2140,
  "ongoing": true
}
```

`ongoing: true` needs the mode of §6. Without it the same record is written
when the poll ends, with `ongoing: false`. In the spike, with §6, five such
calls during a 4 s busy loop were each answered in about 0.3 s.

**Usable without adopting AimDB.** The feature must work in firmware that
has no AimDB records of its own: one dependency, a few lines at start-up
(§2). This is a design constraint, not a convenience. It decides whether an
Embassy developer tries it.

## 2. API

```rust
use aimdb_embassy_adapter::health::{self, Health};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // … heap, clocks, UART as in embassy-serial-connector-demo …

    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(SerialServer::new(uart));       // or any other link
    builder.health(Health::new()
        .block_limit(Duration::from_millis(50))         // one poll may take this long
        .report_every(Duration::from_secs(1)));
    let (db, runner) = builder.build().await.unwrap();

    spawner.spawn(health::named(sensor_task().unwrap(), "sensor_task"));
    spawner.spawn(health::named(flash_task().unwrap(), "flash_task"));
    spawner.spawn(health::named(db_task(runner).unwrap(), "aimdb"));
}

#[embassy_executor::task]
async fn db_task(runner: AimDbRunner) { runner.run().await }
```

- `builder.health(…)` configures the three records of §4 with remote read
  access and adds the source that publishes them.
- `health::named(token, name)` reads the task's ID from the `SpawnToken`,
  stores the name for it and returns the token. A task spawned without it is
  measured too and reported as `task-<n>`, never as an address.
- A task function returns `Result<SpawnToken<_>, SpawnError>` in
  `embassy-executor` 0.10; `named` takes the token.
- The size of the task table is not a builder setting. The table exists
  before the builder does (§3.2), so it is set at compile time.
- `AimDbRunner` is re-exported at the root of `aimdb-core`; today a task
  that takes it has to name `aimdb_core::builder::AimDbRunner`.
- Limits are per instance in the first release. Per-task limits are left
  out until someone needs them (§9).

## 3. Measuring

### 3.1 The callbacks

`embassy-executor` 0.10 calls seven `extern "Rust"` functions when its
`trace` feature is on. The adapter's `health` feature enables `trace` and
defines all seven:

| Callback | Used for |
|---|---|
| `_embassy_trace_task_new` | Take a slot in the task table |
| `_embassy_trace_task_end` | Free the slot |
| `_embassy_trace_task_ready_begin` | Store the time the task became ready |
| `_embassy_trace_task_exec_begin` | Wait time = now − ready time; mark the task as the one its executor is polling |
| `_embassy_trace_task_exec_end` | Hold time = now − poll start; clear the mark |
| `_embassy_trace_poll_start`, `_embassy_trace_executor_idle` | Busy time per executor; defined but not reported in the first release (§3.3) |

Three properties of the callbacks shape the design:

- **They are link-time symbols.** A binary can have one implementer. The
  `health` feature cannot be combined with another crate that defines the
  same callbacks. The executor's `rtos-trace` feature uses a separate path
  in 0.10 and is unaffected: in the spike a binary with both features and
  an `rtos-trace` backend linked and measured the same.
- **`task_ready_begin` can be called from an interrupt,** and with §6 the
  others are called from two executors, one inside a poll of the other.
  All state the callbacks touch is therefore atomics or behind a critical
  section, and the task being polled, its poll start and the reset cell
  (§5) are kept per executor. The spike used one critical section per
  callback.
- **The IDs are addresses** and are documented as not stable across
  executor versions. The adapter treats them as opaque keys and pins the
  executor version it is tested with.

**Cost.** Measured on the H563 at 250 MHz, with interrupts off: 126 to 148
cycles for `ready_begin`, 233 to 248 for `exec_begin`, 242 to 262 for
`exec_end`, 133 to 148 for `poll_start` and `executor_idle`. End to end, a
task that only yields costs about 450 cycles (1.8 µs) more per poll with
the callbacks on than with them returning at once.

### 3.2 The task table

A fixed, static array of slots. It cannot be allocated at `build()`: the
executor spawns the main task before `main`'s body runs, so the first
`task_new` arrives before any builder exists. Its size is set at compile
time, by a cargo feature or a const. A callback finds its slot by a linear
search over the IDs, which is cheap for the handful of tasks a firmware
has. When the table is full, further tasks are counted in `untracked` and
not measured; the record says so. In the spike one slot is 52 bytes, and a
table of 12 slots with the state of 3 executors is 828 bytes.

Per slot, for the current report window and since boot:

| Field | Meaning |
|---|---|
| `polls` | Number of polls |
| `wait_max_us`, `wait_sum_us` | Time from ready to poll start |
| `hold_max_us`, `hold_sum_us` | Time from poll start to poll end |

Maxima and sums, no histograms. They answer "who held, who waited" and cost
a few words per task. Histograms are an alternative for later (§8).

### 3.3 Time

Timestamps come from `embassy_time::Instant::now()`, so the feature needs no
core-specific counter and works on any chip Embassy supports. They are
stored as 32-bit tick counts and compared with wrapping subtraction. A
duration longer than the wrap is reported as saturated, not as a small
number.

**The resolution is one tick.** At the workspace's 32 768 Hz that is
30.5 µs. In the spike a 100 ms busy loop read 100 036 µs and a 2.5 s one
2 500 152 µs against the cycle counter, which is all that finding a
blocking task needs. But most ordinary polls read 0 µs, and `busy_percent`
read 0 throughout, because every busy period rounds down to 0. An
application cannot choose a finer tick today: the adapter's `embassy-time`
dependency selects `tick-hz-32_768` (§7.1). Until it can, `busy_percent`
is left out of `mcu.tasks`.

Hold time is wall time: interrupts that fire during a poll count towards the
task that was being polled. With §6 that includes AimDB's own work on the
interrupt executor. Separating interrupt time needs the core's cycle counter
and is not part of the first release (§9).

`Ticker` makes up for missed ticks inside one poll: after a 2.5 s block
the spike's 1 kHz task held the executor for 2.4 ms. The table shows this
as that task's hold time, correctly.

### 3.4 Names

Embassy does not let code outside the executor look up a task's name from
its ID: `TaskRef::metadata()` is crate-private, and the `task` macro names
only the main task. The public way in is the `SpawnToken`, which exposes the
ID before the task is spawned. `health::named` uses it (§2). If the
executor's `metadata-name` feature is on, it also sets the name there, so
other tools see the same one.

## 4. Records

All three are read-only for remote peers and are plain JSON over AimX, so
the CLI and the MCP server need no host-side types.

**`mcu.tasks`** (`SingleLatest`), written every `report_every`:

```json
{
  "window_ms": 1000,
  "untracked": 0,
  "executors": [ { "id": 0 } ],
  "tasks": [
    { "name": "sensor_task", "executor": 0, "polls": 100,
      "wait_max_us": 180, "hold_max_us": 95,
      "wait_max_us_boot": 37400, "hold_max_us_boot": 110 },
    { "name": "flash_task", "executor": 0, "polls": 1,
      "wait_max_us": 20, "hold_max_us": 38100,
      "wait_max_us_boot": 20, "hold_max_us_boot": 38100 }
  ]
}
```

**`mcu.blocked`** (`SingleLatest`), written when a poll exceeds
`block_limit`:

```json
{ "task": "flash_task", "executor": 0, "held_ms": 38, "ongoing": false }
```

With §6 it is also written while the poll still runs, once when the limit
is passed and then once per report interval with the growing `held_ms`, and
a last time with `ongoing: false` when the poll ends.

The record holds the last event, not a ring of them. A ring cannot be read
after the fact over AimX: `record.get` on an `SpmcRing` answers
`unsupported`, and a drain or a subscription starts at the next message, so
a reader that connects after the event finds nothing. With `SingleLatest`
`aimdb record get mcu.blocked` returns the last block, and
`aimdb watch mcu.blocked` follows one as it grows. A history of blocks is
left for later (§9).

**`mcu.reset`** (`SingleLatest`), written once at start-up (§5):

```json
{ "after_reset": true, "task": "flash_task", "held_ms_at_least": 4000 }
```

The record keys are fixed, not one per task. Tasks are spawned at run time
and AimDB records are configured at build time, so per-task keys would need
every task declared twice.

## 5. A task that never yields

When a poll never ends, no report follows it. Two things cover this case.

**The poll in progress is kept in memory that survives a reset.**
`task_exec_begin` writes the task's ID and the start time into a cell in the
`.uninit` section, which `cortex-m-rt` does not clear at boot, and
`task_exec_end` clears it. The application's own watchdog resets the board.
At the next start the adapter finds the cell still set, resolves the ID to a
name once the tasks have registered again, and writes `mcu.reset`. The cell
is copied and cleared in the first callback of all, the main task's
`task_new`: the main task's first `exec_begin` would otherwise overwrite
it. There is one cell per executor (§3.1). Task
addresses are static, so the ID of the same firmware resolves to the same
task. The cell carries a magic value and the build identity (057); a cell
from another firmware is ignored. In the spike the cell survived an IWDG
reset on the H563 and resolved to the blocking task, and the bytes left in
it after flashing were rejected by the magic.

`held_ms_at_least` is the time between the poll's start and the last time
anything saw it still running. Only the watcher of §6 can see that: with it
the spike reported 3770 ms before a 4 s watchdog. Without §6 nothing runs
during the block and the value is 0, unless the application passes its
watchdog timeout in.

The adapter does not own the watchdog and does not read the reset cause,
both of which are chip-specific. The application can pass the cause in.

**The other side judges silence.** A reader that hears nothing from the
board for a few seconds reports that by its own clock. That is the reader's
job. It needs a client that gives up: `aimdb-client` has no timeout on
requests after the handshake today (#474).

## 6. Answering while a task blocks

By default AimDB runs on the same executor as the application, so its
answer waits until the blocking task yields. The opt-in mode moves AimDB to
an `InterruptExecutor`, which runs inside an interrupt and preempts the
thread-mode executor.

```rust
static AIMDB_EXECUTOR: InterruptExecutor = InterruptExecutor::new();

#[interrupt]
unsafe fn UART5() { AIMDB_EXECUTOR.on_interrupt() }   // any spare interrupt

// in main:
interrupt::UART5.set_priority(Priority::P15);         // lowest interrupt priority
let high = AIMDB_EXECUTOR.start(interrupt::UART5);
high.spawn(health::named(db_task(builder), "aimdb"));
```

A watcher task on that executor wakes on a timer, compares the start of the
poll in progress with the current time, and writes `mcu.blocked` with
`ongoing: true` when the limit is passed. The serial session runs there too,
so `aimdb record get` is answered during the block.

**Priority.** The executor's interrupt takes the lowest interrupt priority:
above every task, below every peripheral interrupt. The UART interrupt that
feeds the session must be above it.

**What it costs.** AimDB's work now preempts the application's tasks, which
are late by at most AimDB's longest poll. In thread mode they wait for the
same poll to end, so the mode adds little: in the spike, with one
`record.get` after another, a 1 kHz task's interval was off by at most
1161 µs with AimDB on the interrupt executor and 1110 µs in thread mode.
Both are the 1.1 ms AimDB takes to serve one `record.get`, which is the
number to reduce in either mode. The mode stays opt-in because of the rules
below, not because of jitter.

**What makes it sound.** Three facts from the code today:

- AimDB drives `Send` futures (`BoxFuture` in `aimdb-core/src/executor.rs`),
  which `SendSpawner` requires. In the spike a task holding the
  `AimDbRunner` was accepted by `SendSpawner::spawn` and served AimX.
- The Embassy buffers use `CriticalSectionRawMutex`, so a record can be
  written by a task on one executor and read on the other.
- **But the `Send` of connector futures is asserted, not proven.**
  `SendFutureWrapper` forces it with the argument that Embassy is
  "single-core, cooperative, no preemption". With two executors that
  argument holds only if every AimDB future is polled by the AimDB executor
  alone and shares nothing that is not `Sync` with the application.

Rules that follow, to be enforced or at least documented:

1. The whole AimDB instance runs on one executor. It is not split.
2. A link's peripherals belong to AimDB's side. An `embassy-net` stack used
   by an AimDB connector, and its runner task, live on the AimDB executor
   and are not shared with application tasks.
3. Application tasks reach AimDB only through producers and consumers.
   Both end in a buffer behind a `CriticalSectionRawMutex` (§12, question
   6). Methods of `AimDb` itself are not covered: on `no_std`, core guards
   some state with `spin::Mutex`, and a task preempted while it holds one
   makes AimDB spin forever, because the holder never runs again. The case
   found is `AimDb::inbound_key_name` on a record with a wildcard inbound
   link (055), whose key table the router locks on AimDB's executor. That
   lock becomes a critical section before this mode ships (§7.1).
4. The allocator must be safe to call from an interrupt. `embedded-alloc`'s
   heaps take a critical section and qualify.

**What it cannot see.** A task that blocks inside a critical section, with
interrupts disabled, blocks the interrupt executor too. This includes
`embassy-stm32`'s blocking flash erase and write, which run each sector and
chunk inside `critical_section::with` (0.6.0, `flash/common.rs`). Such a
block is reported after it ends, or through §5. A demo fault for this mode
must therefore be a busy loop or a blocking wait, not a flash erase.

When such a block ends, the pending interrupt executor runs before the
task's `exec_end`. In the spike the watcher then wrote `ongoing: true` with
the full 2000 ms, 10 ms before the `ongoing: false`. A block that the
watcher first sees further past the limit than its own period is one it
could not watch; it reports that block only if it is still there one period
later.

## 7. What it does not do

- It does not say what a task was waiting for, or where inside a poll the
  time went.
- It does not separate interrupt time from task time (§3.3).
- It does not measure stack use. Embassy tasks have no stacks of their own;
  one high-water mark for the main stack could be added later.
- It does not judge. There is no verdict of `ok` or `degraded` in the first
  release; a reader, or a later join, decides from the records.
- It does not make the adapter installable from crates.io. The workspace
  patches `embassy-sync` from git (#386), and a patch is not published, so
  an Embassy user depends on AimDB by git revision until that is resolved.
  For the audience of this feature that is the larger obstacle.

### 7.1 What has to be fixed first

Found in the spike, outside this feature:

- **The serial AimX server runs out of heap on the second connection**
  (#473). Every CLI call over serial is a new handshake, and the unmodified
  serial demo panics on the second one. Blocks phase 1.
- **The adapter fixes the tick rate.** `aimdb-embassy-adapter` takes
  `embassy-time` from the workspace with `tick-hz-32_768`, so an application
  that wants another rate fails to build. The adapter stops selecting a
  tick. Needed for a finer resolution and for `busy_percent` (§3.3).
- **The key table's `spin::Mutex`** becomes a critical section (§6, rule 3).
  Blocks phase 3.

## 8. Alternatives considered

1. **Per-task records** (`task.<name>`). Reads well, but tasks appear at run
   time and records at build time. Rejected for the first release (§4).
2. **Histograms per task.** More precise, and several times the RAM per
   slot. Deferred until maxima and sums prove too coarse.
3. **The core's cycle counter as the time source.** Finer and cheaper per
   call, but missing on Cortex-M0+ and wrapping within seconds at high clock
   rates. Rejected as the default; a candidate for interrupt timing later.
4. **A hardware timer interrupt as the watcher,** without a second
   executor. It can detect the block but cannot answer a request, because
   the session still waits on the blocked executor. Rejected: the answer
   during the block is the point of §6.
5. **A second, small AimDB instance for health only,** on the interrupt
   executor, with the application's instance left in thread mode. It keeps
   AimDB's work off the application's back, but two instances cannot share
   one serial link. Rejected: in the spike §6 added no jitter worth the
   second instance.
6. **Building on `rtos-trace`.** It streams raw events to a host tool over a
   debug channel, which is the dependency this feature exists to remove.
7. **Using probes (059).** A probe is asked and answers once. A blocked
   executor cannot be asked, and the per-second table is state, not an
   inspection. The records can still be read by a probe later.
8. **`mcu.blocked` as an `SpmcRing`,** to keep a history. This was the
   first design. A ring has no value to `record.get` and no history for a
   reader that connects later (§4).

## 9. Later

- A verdict record with reasons, joined from `mcu.tasks` and `mcu.blocked`.
- Per-task limits.
- A history of blocks that a reader can fetch after the fact.
- `busy_percent` per executor, once the tick can be chosen or from the
  cycle counter (§3.3).
- Interrupt time per handler, from the cycle counter where the core has it.
- Main-stack and heap high-water marks.
- The Tokio counterpart: the same records fed from the runtime's task-poll
  hooks, with AimDB on a second runtime on its own thread.

## 10. What the spike had to settle

Run on the NUCLEO-H563ZI with `embassy-serial-connector-demo` as the base.
§12 has the answers.

| # | Question | Decides |
|---|---|---|
| 1 | Do the seven callbacks deliver wait and hold times that match a logic-analyser measurement of a known blocking poll? | Whether §3 works at all |
| 2 | What does one callback cost in cycles, and the table in RAM per task? | The overhead figures the README may state |
| 3 | Does `health::named` see the ID the callbacks later report? | §3.4 |
| 4 | Does the binary link with the executor's `rtos-trace` feature on as well? | The coexistence claim in §3.1 |
| 5 | Does the AimDB runner compile and run as a task on an `InterruptExecutor`? | §6 |
| 6 | Is anything that is not `Sync` reachable from a `Producer` or `Consumer` held by an application task? `RefCell` and `NoopRawMutex` occur in `connectors.rs`, `net.rs` and `event_capture.rs`. | Whether rules 1 to 3 of §6 are enough |
| 7 | With a busy loop in one task, is `aimdb record get mcu.blocked` answered over serial during the loop? | The feature's main claim |
| 8 | How much jitter does AimDB on the interrupt executor add to a 1 kHz application task? | Whether §6 can be recommended or stays a debugging mode |
| 9 | Does the `.uninit` cell survive a watchdog reset on this chip, and does the ID resolve after it? | §5 |
| 10 | Does `aimdb-core` build for `thumbv6m-none-eabi`? | Whether the Raspberry Pi Pico can be a target, or only the Pico 2 |

Question 10 matters for reach more than for the design: the Pico is the
board most Embassy developers own, and its core lacks the atomics that
`alloc::sync::Arc` needs. The answer is no (§12).

## 11. Phases

1. **Measure and report.** The `health` feature, the callbacks, the task
   table, `health::named`, `mcu.tasks`, and `mcu.blocked` written when a
   poll ends. No second executor.
2. **After a reset.** The `.uninit` cell and `mcu.reset`.
3. **During the block.** The interrupt-executor mode, the watcher and
   `ongoing: true`, with the rules of §6 documented and an example.
4. **An example an Embassy developer can copy,** on the Raspberry Pi
   Pico 2. The first Pico is out: `aimdb-core` does not build for its core
   (§12, question 10).

Phases 1 and 2 do not depend on §6 and can ship first. Phase 1 needs #473
fixed, phase 3 the key table's lock (§7.1).

## 12. Spike results

A throwaway spike (`spikes/061-task-health`, not committed) ran on the
NUCLEO-H563ZI at 250 MHz, on `embassy-serial-connector-demo`'s setup, with
`embassy-executor` 0.10.0 and the workspace's 32 768 Hz tick. It implements
the seven callbacks, the table, `health::named`, the three records, the reset
cell, and the watcher on an `InterruptExecutor` (`UART4`, priority 15). A
writable record makes the board block on command: a busy loop, a busy loop
inside a critical section, or a loop that never ends. An IWDG with a 4 s
timeout is fed by a task on the thread-mode executor. No logic analyser was
at hand. The reference is the DWT cycle counter, read by the blocking task
around its own loop and by a 1 kHz task between its wake-ups.

| # | Answer |
|---|---|
| 1 | **Yes.** Hold time matched the busy loop within 1 to 5 ticks: 100 036 µs against 100 000 µs (DWT), 1 000 091 against 1 000 000, 2 500 152 against 2 500 000. The 1 kHz task's wait time matched its own measured lateness: 99 426 µs against 99 420, 999 847 against 999 812. |
| 2 | Per call, interrupts off, over five boots: `ready_begin` 126 to 148, `exec_begin` 233 to 248, `exec_end` 242 to 262, `poll_start` 133 to 136, `executor_idle` 139 to 148 cycles; `Instant::now()` alone 18 to 27. End to end, a task that only yields costs 1000 to 1052 cycles per poll with the callbacks on and 546 to 590 with them returning at once: **about 450 cycles (1.8 µs) per poll**. One slot is 52 bytes; the table with 12 slots, 3 executors and 8 pending events is 828 bytes. |
| 3 | **Yes.** `SpawnToken::id()` is `as_ptr() as u32`, the same value the callbacks get. All five named tasks were found by `task_new` under the ID `named` stored, on both executors; no task appeared twice. |
| 4 | **Yes.** With `rtos-trace` and `trace` both on and a no-op `rtos-trace` backend, the binary links with both symbol sets and runs with the same measurements. |
| 5 | **Yes.** `AimDbRunner` in an `#[embassy_executor::task]` is accepted by `SendSpawner::spawn`, and the instance serves AimX from the interrupt executor. |
| 6 | **Not quite.** See below. |
| 7 | **Yes,** for `mcu.tasks`; `record get mcu.blocked` itself was refused, because the spike's record was a ring (§12.1). During a 4 s busy loop, five separate `aimdb record get mcu.tasks` calls were answered in 0.28 to 0.31 s each; one just before the loop took 0.28 s. A `watch mcu.blocked` across a 3 s loop received `held_ms` 54, 1055 and 2056 with `ongoing: true` while the loop ran, then 3000 with `ongoing: false`. Without §6 the record arrives only after the loop. |
| 8 | Under load (back-to-back `record get mcu.tasks`, about 7 per second), the 1 kHz task's largest interval error was **1161 µs on the interrupt executor and 1110 µs in thread mode**, with 272 and 252 of about 19 500 intervals off by more than 500 µs. With no requests: 13 µs on the interrupt executor; 523 µs in thread mode, on 2 intervals, probably the end of the CLI call that started the run. Both loaded figures equal AimDB's longest poll (`aimdb` hold time 1.1 to 1.2 ms, serving one `record.get`). The interrupt executor adds no jitter that thread mode does not already have. |
| 9 | **Yes.** After an IWDG reset (RCC_RSR `0x24000000`, IWDG and pin), the cell was intact and its ID resolved to `fault_task`. In thread mode `held_ms_at_least` was 0 (nothing can update the cell during the block). With the watcher it was 3770 ms against the 4 s timeout. After flashing, the cell held leftover bytes and was rejected by the magic. |
| 10 | **No.** See below. |

**Q6.** What an application task reaches through a `Producer` is safe across
the two executors: `produce` reads the clock and pushes into a buffer behind
a `CriticalSectionRawMutex`, and a `Consumer`'s reader is an Embassy
subscriber behind the same mutex. In the spike an application task read a
record that AimDB wrote from the interrupt executor. The `RefCell`s in
`net.rs` and `connectors.rs` are inside the embassy-net types that rule 2
already keeps on AimDB's side. `event_capture.rs`'s `RefCell` is a std
`thread_local!` and does not exist on Embassy. What rules 1 to 3 do not cover
is `aimdb-core`'s `spin::Mutex` on `no_std`. Most of them guard
configuration and are not touched after `build()`. One is locked at run
time from both sides: the `KeyTable` of a wildcard inbound record (055).
The router locks it on AimDB's executor, and `AimDb::inbound_key_name`,
which is public, locks it from wherever it is called. An application task
that holds it when the interrupt executor preempts it makes AimDB spin
forever, because the holder never runs again. The lock becomes a critical
section, the reason 060 gives for its counter (§6, rule 3). This was found
by reading the code; the spike had no wildcard link and did not provoke it.

`INTERNED_KEYS`, behind the public `StringKey::intern`, is a `spin::Mutex`
too. Core locks it only while records are configured. It is a hazard only
for an application that interns keys at run time on both executors.

**Q10.** `aimdb-core` does not build for `thumbv6m-none-eabi`. Two
dependencies fail first and have a `portable-atomic` feature that fixes them:
`event-listener` (through `async-channel`) and `spin`. Then core's own code
fails: 16 imports of `alloc::sync`, `alloc::task::Wake`, and `futures-util`'s
`FuturesUnordered` and `AtomicWaker`. Core has 58 `Arc<dyn …>` uses, and
`portable-atomic-util`'s `Arc` cannot do the unsized coercion on stable. The
Pico 2 (Cortex-M33, `thumbv8m.main`) is the board phase 4 can target.

### 12.1 Findings that changed this design

Each is written into the section named.

- **The table is static, not allocated at `build()` (§3.2).** The
  executor's `run` spawns the main task before `main`'s body runs, so the
  first `task_new` comes before any builder exists. `max_tasks` was a
  builder setting and is now set at compile time.
- **The reset cell is read at the first callback (§5).** The main task's
  first `exec_begin` would overwrite it. The spike copies the cell in the
  first `task_new` and clears it.
- **One "poll in progress" per executor (§3.1, §5).** The interrupt
  executor's callbacks fire inside a thread-mode poll. The current task, the
  poll start and the reset cell are kept per executor.
- **`mcu.blocked` cannot be read after the fact over AimX (§1, §4).**
  `record.get` on a `SpmcRing` returns `unsupported` ("a ring has no current
  value"), so §1's example fails. `record.drain` opens its cursor on the
  first call, and an Embassy subscriber starts at the next message. Every
  CLI call over serial is a new handshake, which resets the session. So no
  reader finds an event written before it connected, and the ring keeps no
  history for anyone. `mcu.blocked` is now `SingleLatest`. The spike's
  `watch` results were taken with the ring; the `SingleLatest` form was not
  run.
- **Times are in 30.5 µs steps (§3.3).** At the 32 768 Hz tick most polls
  read 0 µs, and `busy_percent` read 0 throughout, because each busy period
  rounds to 0. A finer tick does not help today: `aimdb-embassy-adapter`
  takes `embassy-time` with `workspace = true`, which brings
  `tick-hz-32_768`, so an application that uses the adapter cannot choose a
  tick rate. The spike's 1 MHz build failed on exactly that, so no finer
  tick was measured. `busy_percent` is out of the first release (§7.1, §9).
- **Late `ongoing: true` after a critical section (§6).** When a block
  inside `critical_section::with` ends, the pending interrupt executor runs
  before the task's `exec_end`. The watcher then writes `ongoing: true`
  with the full time, 10 ms before the `ongoing: false`. The watcher now
  waits one period before it reports a block it first sees that late.
- **Hold time includes the interrupt executor (§3.3).** In §6 mode, a
  thread-mode task's hold time includes AimDB's time on the interrupt
  executor. It is wall time, and §3.3 now names the case.
- **§6's cost is AimDB's longest poll in both modes (Q8).** §6 can be
  recommended on jitter grounds. The 1.1 ms poll for one `record.get` is
  the number to reduce, whichever mode is used.
- **The example in §2 did not compile.** In 0.10 a task function returns
  `Result<SpawnToken<_>, SpawnError>`; the builder method is
  `with_connector`; and the spike built the database in `main` and gave the
  task the runner, which is what §2 now shows. Passing the builder to the
  task was not tried.
- `AimDbRunner` is not re-exported at the crate root (§2).
- `Ticker` catches up missed ticks inside one poll (§3.3).

### 12.2 Found on the way, outside this design

- **The serial AimX server runs out of heap on the second connection
  (#473).** Each
  new handshake on a live connection (`session/server.rs`) creates a new
  `async_channel::bounded::<SubEvent>(256)`. `concurrent-queue` allocates
  all 256 slots up front, 56 bytes each: 14 336 bytes. The old channel is
  still alive at that moment, so the peak is two of them. The unmodified
  `embassy-serial-connector-demo` (32 KB heap) panics on the second
  `aimdb record get` and stays down. The spike's 48 KB heap failed on every
  third call. The spike ran with 96 KB. This blocks §1's workflow, where
  every CLI call is a new handshake, and has to be fixed before 061 ships.
- **The CLI has no request timeout (#474).** When the board blocked before
  replying, `aimdb record set` waited for more than 60 s. §5's "the other
  side judges silence" holds for a client with a timeout, not for the CLI
  today.
- A panic in the AimDB task, with panic-probe, ends in an IWDG reset. The
  reset cell then reported `aimdb` as the task that never yielded, which
  is correct.
