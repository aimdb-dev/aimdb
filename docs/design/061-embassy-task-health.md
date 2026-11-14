# 061 — Embassy task health

**Status:** 📝 Proposed, not yet spiked

**Scope:** A `health` feature of `aimdb-embassy-adapter` that measures every
Embassy task through the executor's trace callbacks and serves the result as
AimDB records: how long each task waits to be polled, how long it holds the
executor, and which task is blocking the executor right now. It covers the
callbacks, task naming, the records, the report after a watchdog reset, and
an opt-in mode that runs AimDB on an `InterruptExecutor` so that it still
answers while a task blocks. Additive: no change to `aimdb-core`, to AimX or
to existing applications. Nothing here has been built or measured; §10 lists
what a spike has to settle before the design is fixed.

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
when the poll ends, with `ongoing: false`.

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

    let mut builder = AimDbBuilder::new().runtime(runtime);
    builder.health(Health::new()
        .max_tasks(8)                                   // table size, fixed
        .block_limit(Duration::from_millis(50))         // one poll may take this long
        .report_every(Duration::from_secs(1)));
    builder.connect(SerialServer::new(uart));           // or any other link

    spawner.spawn(health::named(sensor_task(), "sensor_task"));
    spawner.spawn(health::named(flash_task(), "flash_task"));
    spawner.spawn(health::named(db_task(builder), "aimdb"));
}
```

- `builder.health(…)` configures the three records of §4 with remote read
  access and adds the source that publishes them.
- `health::named(token, name)` reads the task's ID from the `SpawnToken`,
  stores the name for it and returns the token. A task spawned without it is
  measured too and reported as `task-<n>`, never as an address.
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
| `_embassy_trace_task_exec_begin` | Wait time = now − ready time; mark the task as the one being polled |
| `_embassy_trace_task_exec_end` | Hold time = now − poll start; clear the mark |
| `_embassy_trace_poll_start`, `_embassy_trace_executor_idle` | Busy and idle time per executor |

Three properties of the callbacks shape the design:

- **They are link-time symbols.** A binary can have one implementer. The
  `health` feature cannot be combined with another crate that defines the
  same callbacks. The executor's `rtos-trace` feature uses a separate path
  in 0.10 and should be unaffected; the spike confirms it.
- **`task_ready_begin` can be called from an interrupt,** and with §6 the
  others are called from two executors. All state the callbacks touch is
  therefore atomics or a critical section, never a `RefCell`.
- **The IDs are addresses** and are documented as not stable across
  executor versions. The adapter treats them as opaque keys and pins the
  executor version it is tested with.

### 3.2 The task table

A fixed array of `max_tasks` slots, allocated once at `build()`. A callback
finds its slot by a linear search over the IDs, which is cheap for the
handful of tasks a firmware has. When the table is full, further tasks are
counted in `untracked` and not measured; the record says so.

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

Hold time is wall time: interrupts that fire during a poll count towards the
task that was being polled. Separating interrupt time needs the core's cycle
counter and is not part of the first release (§9).

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
  "executors": [ { "id": 0, "busy_percent": 7 } ],
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

**`mcu.blocked`** (`SpmcRing`, 8 entries), written when a poll exceeds
`block_limit`:

```json
{ "task": "flash_task", "executor": 0, "held_ms": 38, "ongoing": false }
```

With §6 it is also written while the poll still runs, once when the limit
is passed and then once per report interval with the growing `held_ms`, and
a last time with `ongoing: false` when the poll ends. A ring keeps the
history, so a reader that looks afterwards still finds the event.

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
name once the tasks have registered again, and writes `mcu.reset`. Task
addresses are static, so the ID of the same firmware resolves to the same
task. The cell carries a magic value and the build identity (057); a cell
from another firmware is ignored.

The adapter does not own the watchdog and does not read the reset cause,
both of which are chip-specific. The application can pass the cause in.

**The other side judges silence.** A reader that hears nothing from the
board for a few seconds reports that by its own clock. That is the reader's
job and already possible today.

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

**What it costs.** AimDB's work now preempts the application's tasks. Their
jitter grows by AimDB's longest poll. That is the reason the mode is opt-in.

**What makes it sound, and what the spike must check.** Three facts from the
code today:

- AimDB drives `Send` futures (`BoxFuture` in `aimdb-core/src/executor.rs`),
  which `SendSpawner` requires.
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
4. The allocator must be safe to call from an interrupt. `embedded-alloc`'s
   heaps take a critical section and qualify.

**What it cannot see.** A task that blocks inside a critical section, with
interrupts disabled, blocks the interrupt executor too. This includes
`embassy-stm32`'s blocking flash erase and write, which run each sector and
chunk inside `critical_section::with` (0.6.0, `flash/common.rs`). Such a
block is reported after it ends, or through §5. A demo fault for this mode
must therefore be a busy loop or a blocking wait, not a flash erase.

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
   one serial link. Rejected for now; worth revisiting if the jitter of §6
   turns out too high.
6. **Building on `rtos-trace`.** It streams raw events to a host tool over a
   debug channel, which is the dependency this feature exists to remove.
7. **Using probes (059).** A probe is asked and answers once. A blocked
   executor cannot be asked, and the per-second table is state, not an
   inspection. The records can still be read by a probe later.

## 9. Later

- A verdict record with reasons, joined from `mcu.tasks` and `mcu.blocked`.
- Per-task limits.
- Interrupt time per handler, from the cycle counter where the core has it.
- Main-stack and heap high-water marks.
- The Tokio counterpart: the same records fed from the runtime's task-poll
  hooks, with AimDB on a second runtime on its own thread.

## 10. What the spike has to settle

Run on the NUCLEO-H563ZI with `embassy-serial-connector-demo` as the base.

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
`alloc::sync::Arc` needs.

## 11. Phases

1. **Measure and report.** The `health` feature, the callbacks, the task
   table, `health::named`, `mcu.tasks`, and `mcu.blocked` written when a
   poll ends. No second executor.
2. **After a reset.** The `.uninit` cell and `mcu.reset`.
3. **During the block.** The interrupt-executor mode, the watcher and
   `ongoing: true`, with the rules of §6 documented and an example.
4. **An example an Embassy developer can copy,** on the board question 10
   allows.

Phases 1 and 2 need no answer to questions 5 to 8 and can ship first.
