# 049 — Real-time cadences: `aimdb-rt`

**Status:** proposed — **not scheduled**. Captured so the shape is settled when
the work is picked up; nothing here is implemented and no dates are attached.

**Scope:** a synchronous, deadline-bounded entry point into the existing data
plane, shipped as an opt-in leaf crate. Nothing in `aimdb-core` changes, so this
does not need a breaking window.

**Vocabulary:** a **cadence** is a named periodic scheduling domain. It emits
**frames**; every frame has a **deadline**. A cadence spans records — it reads
several and writes several — so it is declared on the builder, alongside
records, never inside one.

---

## 1. Where this sits

AimDB has no real-time story today. That is a genuine gap for motion control,
robotics and drive-level applications, where the correctness of a value is
inseparable from *when* it was produced.

It is a smaller gap than it looks. Design [037](037-zero-alloc-consume-path.md)
established zero AimDB-added heap allocations per message end to end, and design
029 made the write path a synchronous, pre-bound, one-vtable-call `push`. The
data plane is already real-time-clean:

| Property | Where | Status |
|---|---|---|
| Produce is synchronous, non-blocking | [`RecordWriter::push`](../../aimdb-core/src/buffer/writer.rs#L23) | already true |
| Produce allocates nothing | 037, B0 baselines | already true, CI-measured |
| Buffers overwrite rather than block | [`Buffer::push`](../../aimdb-core/src/buffer/traits.rs#L46) | already true |
| Consumers cannot delay a producer | SPMC Ring / SingleLatest / Mailbox contract | already true |
| A cadence can *drive* that path | — | **missing** |
| Timing is deadline-based, not delay-based | [`RuntimeOps::sleep`](../../aimdb-core/src/executor.rs#L50) | **missing** |
| Deadline adherence is measured | — | **missing** |

So the work is not "build a real-time system". It is: add a way to enter the
plane that already exists, and prove the timing.

## 2. Current state (verified)

Two concrete blockers.

**Timing is relative and allocates.** `RuntimeOps::sleep(Duration) -> BoxFuture`
([`executor.rs:50`](../../aimdb-core/src/executor.rs#L50)) returns a boxed
future — one allocation per call, as
[`context.rs:70`](../../aimdb-core/src/context.rs#L70) already documents. Two
problems for a cadence:

- *Relative* sleep accumulates drift. Each iteration adds wake latency plus
  execution time, so a nominal 1 kHz loop slides continuously. A periodic loop
  must wait until `start + n · period`, an absolute instant.
- Allocating per frame contradicts the zero-alloc guarantee at exactly the
  moment it matters most.

**No boundary is expressed in the API.** `source`
([`typed_api.rs:583`](../../aimdb-core/src/typed_api.rs#L583)) takes an
`async` closure. Nothing prevents a user from awaiting a network publish inside
what they believe is their control frame — which is the single most common way
real-time claims fail in practice.

## 3. Decision: `aimdb-rt`, a leaf crate

Real-time hosting is platform-specific (`SCHED_FIFO` and core pinning on Linux,
interrupt executors on Cortex-M, nothing at all on WASM). Putting the SPI in
core would push that concern onto every adapter and every user.

Instead `aimdb-rt` is a **leaf**: it depends on core and, behind features, on
the adapters. Nobody who does not opt in pays anything, and core is untouched.

```text
aimdb-core  <──  aimdb-tokio-adapter   <──┐
            <──  aimdb-embassy-adapter <──┤
            <─────────────────────────────┴──  aimdb-rt   (features: tokio, embassy)
```

`cadence` arrives as an extension trait on `AimDbBuilder`, the same pattern as
[`TokioRecordRegistrarExt::buffer`](../../aimdb-tokio-adapter/src/lib.rs#L33)
uses on the registrar.

### 3.1 Module layout

```text
aimdb-rt/src/
  lib.rs           # docs, tier gate, re-exports
  clock.rs         # RtClock SPI
  period.rs        # Period, deadline arithmetic - pure, no_std, unit-testable
  frame.rs         # Frame: deadline, seq, overrun accounting
  driver.rs        # tier-independent frame loop
  builder.rs       # RtBuilderExt::cadence + CadenceBuilder
  deploy.rs        # RtDeployment, RtPriority
  stats.rs         # fixed-bucket histogram -> SignalGaugeHandle
  host/{mod,posix,embassy}.rs
  test_support.rs  # assert_rt_clock_contract - asserted per host
```

Two rules carry the weight. **`driver.rs` is tier-independent**: the frame loop
exists once, and `host/*` decides only *where* it runs, never *how a frame
works* — without this the tiers drift inside a release and the per-tier table
stops meaning anything. **`test_support.rs` is not optional**: it is the
convention that keeps `SpmcRing` identical across three adapters today
([`buffer/test_support.rs`](../../aimdb-core/src/buffer/test_support.rs)), and
`RtClock` needs the same treatment — `sleep_until` never returns early, a
deadline in the past returns immediately, `now_nanos` is monotonic.

### 3.2 Features

```toml
[features]
default = []                            # deliberate: no tier, no accidental dependency
std     = ["aimdb-core/std"]
posix   = ["std", "dep:libc"]           # pinned thread + clock_nanosleep
embassy = ["dep:embassy-executor", "dep:embassy-time"]
```

`default = []` departs from the other crates on purpose: the tier selects a
whole hosting strategy and a platform, so `cargo add aimdb-rt` must not silently
pick one. `lib.rs` opens with a `compile_error!` when zero or both tiers are
enabled. There is no `wasm` feature — the honest absence of §6, enforced by the
toolchain rather than by documentation.

## 4. The `RtClock` SPI

A cadence frame is synchronous, so it never needs a future:

```rust
pub trait RtClock: Send + Sync {
    /// Monotonic nanoseconds. Same epoch as `RuntimeOps::now_nanos`.
    fn now_nanos(&self) -> u64;

    /// Block until `deadline_nanos`. Returns the actual wake time.
    fn sleep_until(&self, deadline_nanos: u64) -> u64;
}
```

Object-safe, `no_std`-clean, no allocation, no executor, no `unsafe` in the
trait. Sharing the epoch with `RuntimeOps::now_nanos` is what lets deadline
statistics line up with the rest of observability — so the host implementations
**delegate** `now_nanos` to the adapter's `RuntimeOps` rather than reading the
OS clock independently. That makes the shared epoch structural instead of a
contract stated in prose.

| Target | Implementation |
|---|---|
| Linux | `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, …)` — absolute, so wake latency does not accumulate |
| Other std | `thread::sleep` of the remaining delta; documented as degraded |
| Embassy | not a sleep at all — the adapter drives the callback from a timer interrupt (§6) |
| WASM | not provided |

## 5. The `cadence` verb

A cadence is declared on the builder, as a peer of records:

```rust
builder.configure::<Encoder>("joint.enc", |reg| {
    reg.buffer(BufferCfg::SingleLatest);
});
builder.configure::<Setpoint>("joint.sp", |reg| {
    reg.buffer(BufferCfg::SingleLatest);
    reg.linked_from("mqtt://cloud/setpoint");
});
builder.configure::<JointCmd>("joint.cmd", |reg| {
    reg.buffer(BufferCfg::SingleLatest);
    reg.linked_to("mqtt://cloud/joint");     // async plane, unchanged
});

builder.cadence("servo", Period::hz(1000), |c| {
    let enc = c.read::<Encoder>("joint.enc");    // resolved once, at build
    let sp  = c.read::<Setpoint>("joint.sp");
    let cmd = c.write::<JointCmd>("joint.cmd");

    move |frame| {                                // the frame — synchronous
        let (Some(e), Some(s)) = (enc.peek(), sp.peek()) else { return };
        cmd.produce(servo_step(s, e, frame.dt()));
    }
});
```

```rust
pub trait RtBuilderExt {
    fn cadence<B, F>(&mut self, name: &'static str, period: Period, build: B) -> &mut Self
    where
        B: FnOnce(&mut CadenceBuilder<'_>) -> F,
        F: FnMut(&mut Frame) + Send + 'static;
}
```

### 5.1 Why the builder, not the registrar

An earlier draft hung the verb off `RecordRegistrar`. That was wrong. A frame
reads encoders and a setpoint and writes a command and a status — it *spans*
records. Declaring it under one record forces every input to arrive from
outside AimDB through a free function, and misfiles a process-level scheduling
entity as a property of one buffer. Records describe data; a cadence describes
timing. Neither should pretend to be the other.

### 5.2 The two-stage closure

The outer closure runs once at build and resolves typed handles; the inner
closure is the frame. This is not a flourish — it is what keeps lookup out of
the frame. `TypedRecord`'s mutexes (`producer`, `consumers`, `transform`) are
taken only during configure/build/teardown, never on the data path, so
resolving handles in the outer stage puts every lock in the build phase and
leaves the frame touching nothing but pre-bound buffer pointers. Same principle
as [`RecordWriter`](../../aimdb-core/src/buffer/writer.rs), which pre-binds the
buffer to avoid a map lookup per `produce`.

`c.read::<T>(key)` compiles to [`DynBuffer::peek`](../../aimdb-core/src/buffer/traits.rs#L97):
non-destructive, non-blocking, `Option<T>`, and `None` by default on
`SpmcRing`. That constrains cadence inputs to `SingleLatest`, which is the
semantically correct choice anyway — a control frame wants the freshest sample,
never a queue it can fall behind on.

### 5.3 The frame signature

The load-bearing detail is that `F` returns `()`, not a future. No executor is
in scope, so **`.await` inside a frame is a compile error**. The two-plane
discipline becomes mechanical rather than documentary — the same move 037 made
when it fixed the `BufferReader` signature so implementations could not be
wrong.

`Frame` exposes `deadline_nanos()`, `now()`, `dt()`, `seq()` and
`overran_previous()`.

Deliberately absent: logging. `RuntimeOps::log` takes `&str`, and constructing a
message means `format!`. Frames report through counters; the async plane does
the talking.

### 5.4 Lifecycle, via `Extensions`

`AimDbRunner` drives `BoxFuture`s and a cadence is not a future, so cadences
need a way into the build without a future-shaped disguise. That seam already
exists: [`Extensions`](../../aimdb-core/src/extensions.rs) is present on both
`AimDbBuilder` and the live `AimDb`, and its module documentation names the use
case exactly — *"the hook mechanism used by `aimdb-persistence` — and any other
crate that needs to attach data to the builder or the live database without
modifying `aimdb-core`."*

So `cadence` stores its registry via `builder.extensions_mut()` at declaration
time, and `aimdb-rt` retrieves it from `db.extensions()` to start the hosts:

```rust
let db = builder.build()?;
let rt = RtCadences::take(&db)?.start(RtDeployment::from_env()?)?;
runner.run().await;
```

No core change, no disguised source, and the leaf property holds.

## 6. Hosting and deployment

Declaration and placement are separate concerns. The program says *there is a
cadence named `servo` at 1 kHz*; the deployment says *`servo` runs on core 2 at
`SCHED_FIFO` 80*. A core number in application source breaks "deploy unchanged"
as surely as a hard-coded broker address would, and makes cadences untestable
in CI.

```rust
pub struct RtDeployment { /* per-cadence placement */ }

impl RtDeployment {
    /// No pinning, no RT priority. Degrades loudly; the declared tier drops.
    pub fn best_effort() -> Self;
    /// AIMDB_RT_<CADENCE>_CORE / _PRIO.
    pub fn from_env() -> Result<Self, RtError>;
    pub fn place(self, cadence: &str, core: usize, prio: RtPriority) -> Self;
}
```

The same binary then runs on a laptop (`best_effort`) and on a tuned target
(`from_env`) with no source change.

**POSIX host.** A dedicated `std::thread` — *not* a task — pinned with
`sched_setaffinity` and set to `SCHED_FIFO`. Note that this host needs `libc`
and `std` only: no Tokio is involved, because no executor is in the path. That
is the point, and it is why the feature is named `posix` rather than `tokio`.

```text
deadline += period
f(&mut frame)
actual = clock.sleep_until(deadline)
```

Two rules, both learned from reading a competitor implementation that gets them
wrong:

- Default priority is **not** `sched_get_priority_max()`. A spinning thread at
  99 outranks kernel threads and can take the machine. Default around 80.
- Scheduling policy is set **on the cadence thread after spawn**, never on a
  thread whose children will inherit it. glibc defaults to
  `PTHREAD_INHERIT_SCHED`; setting policy early means every thread created
  afterwards — connector I/O threads included — silently inherits real-time
  priority.

Missing `CAP_SYS_NICE` degrades loudly at startup and drops the declared tier;
it does not fail silently.

**Embassy host.** Bind the cadence to an `InterruptExecutor` at a chosen
priority, or drive it from a hardware timer. The adapter uses neither today;
interrupt executors are the primitive that makes the MCU tier a genuinely hard
claim rather than a cooperative one. Because binding happens at init, outside
the runner, this host does not use the `Extensions` handoff.

**WASM.** The verb is not compiled. Honest absence beats a soft guarantee.

## 7. The deadline contract

The loop already knows its deadline, so self-measurement is nearly free:

```text
overrun_ns = actual_wake - deadline
exec_ns    = after_callback - before_callback
```

Percentiles are **never computed in the frame**. `stats.rs` owns a fixed-bucket
histogram — `[u32; 64]`, log-spaced — and the frame does one array increment,
no allocation, `no_std`-clean. A tap on the async side drains it and folds the
result into gauges.

Both fold into the existing
[`SignalGaugeHandle`](../../aimdb-core/src/signal.rs), which is already wired to
`record.list` / `record.get`, the CLI and the MCP server. No new introspection
plumbing:

This also works with `observability` off: `SignalGaugeHandle` is inert-but-
present by design ([`signal.rs`](../../aimdb-core/src/signal.rs)), so `stats.rs`
needs no `#[cfg]`.

```text
$ aimdb cadence get servo
  1000 Hz · frames 1_204_338 · overruns 3 · overrun_max 412µs · exec p99 87µs
```

The consequence is worth stating plainly: **real-time deadline telemetry
queryable in natural language against a running machine.** "Did the servo
cadence miss any deadlines in the last hour?" is a question no other real-time framework
can answer conversationally, and here it falls out of infrastructure that already
exists. This is the differentiator, not the loop itself.

## 8. Measurement program — B4

Extends the existing `aimdb-bench` taxonomy (B0 allocations, B1 latency, B2
throughput, B3 on-target cycles).

**B4 — deadline jitter distribution.** Record `wake - deadline` for every frame
over a long run. Report p50 / p99 / p99.9 / **max** and total overrun count.
Commit baselines per tier alongside
[`b0_alloc_tokio.json`](../../aimdb-bench/data/baselines/b0_alloc_tokio.json).

Max, not mean. The mean is precisely what hides the events that matter.

**B0 extension.** Zero allocations per *frame*, timing path included. B0 covers
push/recv today; `sleep_until` must join the gate.

**On-target.** Run B4 on the STM32H563ZI rig that already produces
[B3](../../aimdb-bench/data/baselines/b3_cycles_stm32h5.json). That baseline
shows ~2 013 cycles/msg at 250 MHz ≈ 8.05 µs for the consume path — about 0.8%
of a 1 kHz frame budget, which is what makes the MCU tier plausible before a
line is written.

The README tier table is then filled in *from baselines*, not asserted:

| Tier | Hosting | Expected class |
|---|---|---|
| Embassy / Cortex-M, `InterruptExecutor` | timer interrupt | hard, µs-scale |
| Tokio + PREEMPT_RT + `isolcpus`/`nohz_full` | pinned `SCHED_FIFO` thread | firm |
| Tokio, stock Linux | pinned `SCHED_FIFO` thread | soft, ms tail |
| WASM | not available | — |

Publishing the row that says WASM cannot do this is worth more than any
benchmark in the other three.

## 9. Acceptance criteria

1. `.await` inside a cadence frame does not compile.
2. B0 reports 0 allocations per frame on both hosts, timing path included.
3. B4 baselines committed for every supported tier, host and on-target.
4. Deadline and execution statistics visible through the CLI and the MCP server
   without additional configuration.
5. A cadence coexists with `linked_to` on a record it writes, and a stalled link
   provably does not extend any frame (test, not assertion).
6. The same binary runs under `RtDeployment::best_effort()` and
   `RtDeployment::from_env()` with no source change — CI runs the former.
7. All record-handle resolution happens in the outer closure; a frame performs
   no map lookup and takes no lock.
8. README tier table populated from committed baselines.

## 10. Non-goals

- **No priorities in the async executor.** Once control code is `async` it
  inherits the executor's scheduling policy and the fight never ends.
- **No real-time executor.** The split — cadence outside the general-purpose
  scheduler, lock-free queue between planes — is where ROS 2's realtime working
  group, AUTOSAR, Xenomai and LinuxCNC all independently landed.
- **No safety claims.** If a liveness monitor is added later it is named a
  liveness monitor, not a watchdog, and it uses `now_nanos` — never
  `unix_time`, where a backwards NTP step and a saturating subtraction make it
  silently stop tripping. Functional safety is a hardware STO chain
  (ISO 13849, IEC 61800-5-2) and no Rust API substitutes for it. Saying so in
  the documentation is itself a differentiator in this space.

## 11. Open questions

- **`T: Clone` is a hole.** `push` clones, so a `T` containing a `Vec` or
  `String` allocates inside the frame and voids the guarantee. Either bound
  `cadence` on a marker trait, or document the caveat and add a B0 case
  that fails on an allocating `T`. Marker trait is cleaner; it costs an
  ergonomic wart.
- **Does `RtClock` need to be object-safe?** Assumed yes for symmetry with
  `RuntimeOps`, but a monomorphised clock would remove the indirect call from
  the timing path. Decide with B4 numbers, not by argument.
- **`Period` representation.** `Hz` is the natural user unit; nanoseconds are
  the natural internal one. Non-integer periods need a stated rounding rule.
- **String keys resolve at build time, not compile time.** `c.read::<T>("joint.enc")`
  turns a typo into a startup error rather than a compile error. Consistent with
  `transform(input_key, …)`, so at least the house is uniform — but a cadence
  failing at startup is worse than a transform doing so, and it may justify a
  typed key later.
- **Multiple cadences sharing a record.** Two cadences at different rates both
  writing one record is expressible and probably wrong. Detect and reject at
  build, or allow it and document last-writer-wins?

## 12. Sequencing

0. **Measure first.** A naive `clock_nanosleep` loop on target hardware, p99.9
   recorded. If the floor is already tens of microseconds, that bounds how much
   of the machinery below is justified.
1. `RtClock` + Linux implementation. Additive, no break.
2. B4 harness + host baseline.
3. `cadence` + `CadenceBuilder` + POSIX hosting + `RtDeployment`.
4. Deadline statistics → `SignalGaugeHandle` → CLI / MCP.
5. Embassy interrupt-executor hosting + on-target B4.
6. README tier table.

Steps 1–4 land the complete story on the POSIX tier. Step 5 is what makes the
hard real-time claim real.
