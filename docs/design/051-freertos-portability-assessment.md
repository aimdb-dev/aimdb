# 051 — FreeRTOS Portability Assessment

**Status:** Assessment, 2026-09-01. No code changes; findings and a recommended path.
**Scope:** What it takes to run AimDB on a FreeRTOS system, which parts of the
workspace carry over unchanged, and which assumptions have to be re-established
under a preemptive RTOS.

---

## 1. Summary

AimDB is portable to FreeRTOS with a thin glue layer and no changes to
`aimdb-core`. The engine is `no_std + alloc`, never spawns, and takes its runtime
as a five-method trait object. Everything that is Embassy-specific sits in
`aimdb-embassy-adapter`, and most of that crate depends on `embassy-sync` and
`embassy-time`, not on the Embassy executor.

Three paths exist, in order of recommendation:

| Path | Shape | Reuses | New work | Verified here |
|---|---|---|---|---|
| **A. Embassy executor inside one FreeRTOS task** | `embassy_executor::raw::Executor` with a `__pender` that notifies the task | core, embassy adapter, buffers, serial connector | `__pender`, `critical-section` impl, `embassy-time` driver, allocator, logger | Compiles on `thumbv7em`; `__pender` hook confirmed in `embassy-executor` 0.10 |
| **B. Native `aimdb-freertos-adapter`** | `RuntimeOps` over FreeRTOS ticks, a `block_on` loop on task notifications, `EmbassyBuffer` for storage | core, `EmbassyBuffer` (needs only `embassy-sync` + `critical-section`) | A new adapter crate (~1.5–2k lines mirroring the embassy adapter), timer-driven `sleep`, conformance runs | Buffer layer compiles without `embassy-executor` |
| **C. ESP-IDF (`std` on FreeRTOS)** | Rust `std` target, `aimdb-tokio-adapter` on tokio | the whole std stack incl. `aimdb-sync` | Toolchain and sdkconfig only | Not verified (no espidf toolchain in this environment) |

Path A is the least code and keeps the whole embedded feature set. Path B is
the right long-term answer if Embassy dependencies are unwanted in a FreeRTOS
product. Path C is for ESP32-class parts only.

## 2. What was verified

All checks were run on the workspace at `161e0bc` with the `_external/embassy`
submodule initialised, against `thumbv7em-none-eabihf`:

```sh
cargo check -p aimdb-core --no-default-features \
    --features alloc,connector-session,remote --target thumbv7em-none-eabihf
cargo check -p aimdb-embassy-adapter --no-default-features \
    --features alloc,embassy-sync,embassy-time,connector-io --target thumbv7em-none-eabihf
cargo check -p aimdb-embassy-adapter --no-default-features \
    --features alloc,embassy-runtime,connectors --target thumbv7em-none-eabihf
```

All three pass. The second is the important one for portability: the buffer
layer and the `connector-io` session spine build with **no executor crate** in
the graph.

The `no_std` dependency graph of core is: `async-channel`, `concurrent-queue`,
`crossbeam-utils`, `event-listener(-strategy)`, `futures-{channel,core,util,
task}`, `hashbrown` (foldhash, no RNG), `portable-atomic`, `serde`,
`serde_json` (alloc), `slab`, `spin`, `thiserror`, plus proc-macro crates. No
OS, thread, executor, or HAL dependency. `aimdb-core/src` contains zero
`unsafe`.

`embassy-executor` 0.10.0 `src/raw/mod.rs` line 513 documents the user-supplied
pender: with no `platform-*` feature enabled the crate calls an
`extern "Rust" fn __pender(context: *mut ())` the binary must export.

## 3. Why the engine ports cleanly

### 3.1 Runtime enters as a value

[`RuntimeOps`](../../aimdb-core/src/executor.rs) is the entire runtime contract:
`name`, `now_nanos`, `unix_time`, `sleep(Duration) -> BoxFuture`, `log`.
`AimDb` holds it as `Arc<dyn RuntimeOps>`. The Embassy implementation
([`runtime.rs`](../../aimdb-embassy-adapter/src/runtime.rs)) is about 130
lines. A FreeRTOS implementation is the same size.

### 3.2 The engine never spawns

Since design 028, `AimDbBuilder::build()` collects every service, tap,
transform, connector, and `on_start` future into `Vec<BoxFuture>` and
`AimDbRunner::run()` drives them in a single `FuturesUnordered`
([`builder.rs`](../../aimdb-core/src/builder.rs) around line 887). The host
executor has to poll **one** `Send + 'static` future. That is the weakest
possible executor requirement: a `block_on` loop with a waker that does
`xTaskNotifyGive` is sufficient.

### 3.3 Storage is critical-section based, not executor based

[`EmbassyBuffer`](../../aimdb-embassy-adapter/src/buffer.rs) wraps
`embassy_sync::{pubsub::PubSubChannel, watch::Watch, channel::Channel}` over
`CriticalSectionRawMutex`. Their only platform hook is the `critical-section`
crate. A FreeRTOS binary provides one implementation (see §4.2) and every
buffer works, from any task, including a producer task that is not the executor
task. Wakeups registered by the executor task are fired by `Waker::wake`, which
routes to the pender, which is a task notification. That is task- and ISR-safe
by construction.

The measured steady state on STM32H563 is 1.6–2.0k cycles per message with zero
allocations ([bench README](../../examples/embassy-bench-stm32h5/README.md)).
Nothing in that path touches the executor.

### 3.4 Locks in core are build-time

Core's `spin::Mutex` sites are the `TypedRecord` registration fields
(producer, consumers, transform descriptor) and the `RecordId` intern table.
They are taken during `configure()`/`build()` and by `consumer_count()`. The
produce/consume hot path is lock-free in core; the only lock is the buffer's
critical section. See §5.3 for the one caveat.

## 4. Path A in detail: Embassy executor hosted by FreeRTOS

This is the recommended route for a mixed C/Rust FreeRTOS firmware. Five pieces
of glue, all outside the AimDB workspace:

### 4.1 Executor task and pender

```rust
// Rust side, compiled as a staticlib linked into the FreeRTOS image.
use embassy_executor::raw::Executor;

static mut EXECUTOR: Option<Executor> = None;
static mut EXECUTOR_TASK: *mut c_void = core::ptr::null_mut(); // TaskHandle_t

#[unsafe(export_name = "__pender")]
fn pender(_ctx: *mut ()) {
    // Called from any task or ISR that wakes an AimDB future.
    unsafe { aimdb_notify_executor(EXECUTOR_TASK) } // C: xTaskNotifyGive / vTaskNotifyGiveFromISR
}

#[unsafe(no_mangle)]
extern "C" fn aimdb_executor_task(_: *mut c_void) {
    let exec = unsafe { EXECUTOR.insert(Executor::new(core::ptr::null_mut())) };
    exec.spawner().spawn(aimdb_main()).unwrap(); // builds the db and awaits runner.run()
    loop {
        unsafe { exec.poll() };
        unsafe { ulTaskNotifyTake(1, portMAX_DELAY) };
    }
}
```

**Feature split matters here.** `embassy-executor`'s `platform-cortex-m`
feature exports its own `__pender` (`src/platform/cortex_m.rs` line 1), so a
binary that defines one must build the executor with **no** `platform-*`
feature. The workspace pins `embassy-executor` with `platform-cortex-m` +
`executor-thread`, and `aimdb-embassy-adapter`'s `embassy-runtime` feature
inherits that. Two variants follow:

- **A1, custom pender (above).** Depend on `aimdb-embassy-adapter` with
  `embassy-sync`, `embassy-time`, and `connectors`/`connector-io` but **not**
  `embassy-runtime`, and on `embassy-executor` directly without a platform
  feature. This is the configuration verified in §2. Until §5.1 is fixed the
  `.buffer()`/`.buffer_sized()` sugar is unavailable in this configuration;
  construct `EmbassyBuffer` and pass it to `buffer_with_cfg` by hand. Features
  are additive, so no other crate in the image may enable `platform-cortex-m`.
- **A2, zero glue.** Keep `embassy-runtime` as-is and run
  `embassy_executor::Executor::run` inside the **lowest-priority** FreeRTOS
  task. The cortex-m thread executor idles on `WFE` and pends with `SEV`, so
  the task never blocks in FreeRTOS terms: it behaves like a second idle task,
  starves the real idle task (no tickless idle, no idle hook), and must sit
  below every other task. Single-core only. Acceptable for a bring-up or a
  design where AimDB is the whole background workload.

The `#[embassy_executor::task]` macro works in both variants for side tasks;
the AimDB runner itself is one future.

### 4.2 `critical-section` provider

Single-core: `taskENTER_CRITICAL`/`taskEXIT_CRITICAL` (BASEPRI masking) is the
correct provider. Interrupts above `configMAX_SYSCALL_INTERRUPT_PRIORITY` stay
enabled, which is safe because such ISRs may not call FreeRTOS APIs and must not
touch AimDB buffers either. `cortex-m/critical-section-single-core` (global
`cpsid i`) also works but masks the tick; the buffer sections are short, so it is
acceptable but not preferred.

SMP FreeRTOS (RP2040 SMP, ESP32 dual-core): the provider must be
`taskENTER_CRITICAL` in its SMP form (spinlock plus interrupt mask). A bare
interrupt mask is not a critical section across cores.

`portable-atomic`'s `critical-section` fallback (used by `observability` for
64-bit counters on Cortex-M4/M7) rides the same provider.

### 4.3 `embassy-time` driver

Implement `embassy_time_driver::Driver` (`now()` and `schedule_wake`). Two
workable backings:

- **Hardware timer** (TIMx, LPTIM): microsecond resolution, wake from the
  compare ISR. Matches the `tick-hz-*` feature the workspace already pins for
  the STM32 examples. Preferred for `now_nanos` fidelity (stage profiling).
- **FreeRTOS tick**: `now()` from `xTaskGetTickCount` (`configTICK_RATE_HZ`,
  typically 1 kHz), `schedule_wake` via one `xTimerCreate` software timer
  whose callback calls the waker. Millisecond `sleep` granularity; fine for
  connectors and services, coarse for profiling. The `tick-hz` feature must be
  set to match `configTICK_RATE_HZ`.

`RuntimeOps::unix_time` uses the adapter's `set_unix_time` anchor; a FreeRTOS
node that learns wall time from SNTP or a host handshake calls it once.

### 4.4 Allocator

`#[global_allocator]` over `pvPortMalloc`/`vPortFree` (heap_4 or heap_5) is the
natural choice when C code already owns a heap. `embedded-alloc` on a static
region (the examples use 32 KB) also works. All boxed futures are allocated
once at `build()`; there are zero per-message allocations (design 037), so
fragmentation pressure is low.

### 4.5 Logging

`EmbassyAdapter::log` forwards to `defmt` unconditionally, and `defmt` is a
non-optional dependency of the adapter. `defmt-rtt` coexists with FreeRTOS
without issues, so a project that accepts RTT-based logging needs only a
`#[defmt::global_logger]`. A project committed to `printf`/SEGGER RTT text
should either write a custom logger or use a native adapter (Path B), where
`log` goes through `aimdb-core`'s `log` facade feature (design 050).

### 4.6 Threading rule the application must keep

Every `unsafe impl Send` in the workspace lives in
[`connectors.rs`](../../aimdb-embassy-adapter/src/connectors.rs) and
[`send_wrapper.rs`](../../aimdb-embassy-adapter/src/send_wrapper.rs), all under
one invariant: the wrapped futures are polled by one cooperative executor and
never migrate. Under FreeRTOS that reads:

- `AimDbRunner::run()` and every connector future run **only** in the executor
  task.
- Other tasks interact through `AimDb`, `Producer<T>`, `Consumer<T>` handles,
  which are `Send + Sync` with no `unsafe` behind them. `Producer::produce` and
  `try_produce` are synchronous and safe from any task. A consumer in another
  task must poll with `try_recv` or wait on its own primitive; `recv().await`
  needs an executor.
- On SMP, pin the executor task to one core.

## 5. Concerns and gaps

### 5.1 `.buffer()` sugar is gated on the executor feature

`EmbassyRecordRegistrarExt` and `EmbassyRecordRegistrarExtCustom` are
implemented under `#[cfg(all(feature = "embassy-runtime", feature = "embassy-sync"))]`
([`lib.rs`](../../aimdb-embassy-adapter/src/lib.rs)). The `EmbassyBuffer` type
itself needs only `embassy-sync`. Path B, or any host that wants the buffers
without `embassy-executor` in its graph, has to either enable `embassy-runtime`
anyway or call `buffer_with_cfg(Box::new(EmbassyBuffer::...))` by hand. Regating
these impls on `embassy-sync` is a one-line change.

### 5.2 Network connectors are bound to `embassy-net`

The embedded halves of the MQTT, KNX, and TCP connectors take
`&'static embassy_net::Stack` (`embassy_client.rs`, `embassy_tls.rs`,
`NetStack`). FreeRTOS products almost always run lwIP or FreeRTOS+TCP, and
running a second IP stack for AimDB is not attractive. Two options:

- **Implement the session transport over lwIP.** `aimdb_core::session::{Connection,
  Dialer, Listener}` is three methods (`recv`, `send`, `peer`, plus `accept` /
  `connect`). A socket task doing blocking lwIP I/O, bridged through
  `async-channel` with pender wakeups, or a netconn-callback-to-waker shim,
  satisfies it. The TCP connector and AimX remote access then ride the
  runtime-neutral `run_client`/`serve` engines already in core with no
  connector code changes.
- **MQTT** has no runtime-neutral client in the workspace; the embedded half is
  written against `embassy_net::tcp::TcpSocket`. A FreeRTOS MQTT path is
  either an lwIP `Connection` plus a small MQTT client over it, or the C
  firmware's existing MQTT client bridged through a `Source`/sink pair via
  `pump_source`/`pump_sink`.

The serial connector is the exception: its embedded half is generic over
`embedded-io-async` halves (`connector-io`). A FreeRTOS UART driver exposing
`embedded_io_async::{Read, Write}` with ISR-to-waker signalling plugs in
unchanged.

### 5.3 Spin locks under preemption

`spin::Mutex` in core (§3.4) and the internal lock of `event-listener` (used by
`async-channel` in the session engines) spin without yielding. On a single-core
preemptive kernel, a high-priority task spinning on a lock held by a preempted
low-priority task deadlocks. This is a non-issue when:

- `configure()`/`build()` run in the executor task before other tasks receive
  handles (the normal flow), and
- session-engine channels are touched only from the executor task (they are;
  only futures hold them).

The one path worth guarding is dynamic `RecordId` interning from multiple tasks
at runtime (dynamic MQTT topics, design 018). Options: intern all keys during
build, or swap core's `no_std` `Mutex` alias for `critical_section::Mutex`,
which is the RTOS-correct primitive and already a transitive dependency.

### 5.4 No blocking API and no C API for `no_std`

`aimdb-sync` is the FFI door, and it is `std`-only: it spawns an OS thread that
owns a tokio runtime (`handle.rs`). Its `no_std` build exports only the error
types. Design 007 already deferred "RTOS with threads" pending a use case. A C
firmware that wants `get()`/`set()` from arbitrary tasks needs either Path C, or
a small `no_std` blocking layer: `Producer::try_produce` already works from any
task; blocking consume is `try_recv` plus a task notification the consumer
registers with the buffer's waker. There is no `cbindgen`/`extern "C"` surface
anywhere in the workspace today (the only `extern` is the `pthread_atfork`
hook in `aimdb-sync/src/fork.rs`), so a C API is new work regardless of path.

### 5.5 Stack and heap budgeting

Futures are heap-boxed, so the executor task's stack carries the poll depth of
the deepest future, not the futures themselves. `serde_json` and the session
engine are the deep ones. Start at 8–16 KB, measure with
`uxTaskGetStackHighWaterMark`, and keep `configCHECK_FOR_STACK_OVERFLOW` on
during bring-up. Heap: 32 KB matches the examples; the record count and
buffer capacities (const generics, so mostly static) set the rest.

### 5.6 Clock resolution

With a tick-based time driver, `now_nanos` is millisecond-granular. Stage
profiling (design 014) and `observability` counters still work but lose
resolution; use DWT `CYCCNT` or a hardware timer where profiling matters.

## 6. Path B: a native adapter

If Embassy crates are unwelcome in the product, a `aimdb-freertos-adapter` needs:

1. `RuntimeOps` (≈130 lines): `now_nanos` from a hardware timer or ticks,
   `sleep` from a timer wheel or FreeRTOS software timers, `log` via the `log`
   facade or a callback.
2. An executor: a `block_on` over `runner.run()` whose `Waker` sends a task
   notification, and a timer path that wakes the same task. Roughly 50 lines;
   no `embassy-executor` needed because the engine hands over one future.
3. Buffers: reuse `EmbassyBuffer` (`embassy-sync` + `critical-section` only,
   verified) after §5.1 is fixed, or port the three primitives to
   `critical_section::Mutex` and drop `embassy-sync` as well.
4. Conformance: `assert_runtime_ops_contract` and the shared buffer contract
   suite (`aimdb-core/src/buffer/test_support.rs`) run on the host, as the
   three existing adapters do.

Estimated at the size of the existing embassy adapter without its connector
spines (~1.5k lines) plus an lwIP `Connection` if networking is needed.

## 7. Path C: ESP-IDF

Rust on ESP-IDF is `std` over FreeRTOS. tokio builds for the `espidf` targets
(with `esp_vfs_eventfd` registered for `net`), so `aimdb-core/std` +
`aimdb-tokio-adapter` and even `aimdb-sync` (pthreads over FreeRTOS tasks)
should run as on Linux. Points to verify before relying on it: the
`rt-multi-thread` feature the workspace pins on tokio, default pthread stack
sizes (`CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT`), `pthread_atfork` availability
for `aimdb-sync`'s fork detector, and flash size (`std` + `serde_json/std` +
tokio is several hundred KB). Not exercised in this assessment.

## 8. Recommended next steps

1. Regate the registrar extension impls on `embassy-sync` (§5.1). Trivial, unblocks Path B and slimmer Path A builds.
2. Replace core's `no_std` `spin::Mutex` alias with `critical_section::Mutex` (§5.3), or document the build-before-share rule in `RuntimeOps` docs.
3. Build a Path A demo: STM32 with FreeRTOS + Rust staticlib, `__pender` on task notifications, hardware-timer time driver, serial connector over a FreeRTOS UART driver. This is the port; everything else is hardening.
4. Decide whether an lwIP `Connection` belongs in the workspace (as a `aimdb-lwip-connector` or as a `Connection` impl inside a FreeRTOS adapter) so TCP/AimX remote access reach FreeRTOS nodes.
5. Revisit the deferred `no_std` blocking API from design 007 if C-side producers/consumers are required.
