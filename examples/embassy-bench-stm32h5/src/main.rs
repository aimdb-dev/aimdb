#![no_std]
#![no_main]

//! B3 — On-target cycle & allocation profiling of the AimDB Embassy buffer
//! consume path.
//!
//! This is the part of the Embassy benchmark suite that **cannot run on a
//! host**: it reads the Cortex-M **DWT cycle counter** (`CYCCNT`) to measure the
//! real per-message cost, in CPU cycles, of `push` → `recv` for each AimDB
//! buffer profile on an STM32H563ZI (Cortex-M33 @ 250 MHz). The host
//! `aimdb-bench` suite covers B0 (allocations), B1 (wall-clock latency) and B2
//! (throughput) for the same Embassy buffer backend; this binary adds the
//! cycle-accurate B3 numbers and re-validates the zero-allocation claim
//! against the real embedded allocator (`embedded-alloc`), wrapped here in a
//! counting allocator.
//!
//! ## What is measured
//!
//! For Telemetry (`SpmcRing`), State (`SingleLatest`) and Command (`Mailbox`),
//! plus a 1→4 Telemetry fan-out, the binary runs a tight lockstep
//! `push`→`recv` loop and reports, per message:
//!   * **cycles/msg** — `DWT::cycle_count()` delta over the measured batch,
//!   * **allocs/msg** — global-allocator call count over the measured batch.
//!
//! The measured window excludes a warmup phase; the one-time reader boxing
//! happens during warmup (SpmcRing subscriber registration is eager, at
//! `subscribe()` time). As with the host B1/B2 suites,
//! payload construction is inside the timed loop, so the figure is the
//! end-to-end per-message consume cost, not the buffer call in isolation.
//!
//! ## Running
//!
//! ```bash
//! # From this crate dir, with a Nucleo-H563ZI connected via ST-LINK:
//! cargo run --release
//! ```
//!
//! Results stream over RTT (SWD) as defmt logs. `--release` is strongly
//! recommended; debug vs release cycle counts differ by an order of magnitude
//!, so always record the build profile with a baseline.

extern crate alloc;

use alloc::boxed::Box;

use aimdb_bench::alloc::CountingAllocator;
use aimdb_bench::payloads::{
    CommandMsg, StateMsg, TelemetryMsg, command_msg, state_msg, telemetry_msg,
};
use aimdb_core::buffer::{Buffer, Reader};
use aimdb_embassy_adapter::EmbassyBuffer;
use cortex_m::peripheral::DWT;
use defmt::info;
use embassy_executor::Spawner;
use embassy_futures::block_on;
use {defmt_rtt as _, panic_probe as _};

// ── Allocation-counting heap ─────────────────────────────────────────────────
//
// The embedded analogue of the host `CountingAllocator<System>` in
// `aimdb-bench` — same type, wrapping `embedded-alloc`'s `LlffHeap` instead of
// `std::alloc::System` (previously a hand-forked copy here,
// since `alloc::CountingAllocator` used to come bundled with a crate-wide
// `#[global_allocator]` declaration for `System` that a `no_std` binary
// couldn't reuse; now each binary declares its own).

#[global_allocator]
static HEAP: CountingAllocator<embedded_alloc::LlffHeap> =
    CountingAllocator(embedded_alloc::LlffHeap::empty());

#[inline]
fn reset_allocs() {
    aimdb_bench::alloc::reset();
}

/// Allocation count since the last [`reset_allocs`] (the shared counter also
/// tracks bytes allocated; this report only needs the count).
#[inline]
fn allocs() -> u32 {
    aimdb_bench::alloc::snapshot().0 as u32
}

// ── Buffer type aliases ──────────────────────────────────────────────────────
//
// Same backends as `aimdb_bench::profiles_embassy`, with smaller `CAP` to keep
// the static `PubSubChannel` footprint modest on-target. The lockstep loops
// keep at most one message in flight, so a small `CAP` never lags. `SUBS = 4`
// on the Telemetry ring leaves room for the 1→4 fan-out.

type TelemetryBuffer = EmbassyBuffer<TelemetryMsg, 8, 4, 1, 1>;
type StateBuffer = EmbassyBuffer<StateMsg, 1, 1, 1, 2>;
type CommandBuffer = EmbassyBuffer<CommandMsg, 1, 1, 1, 1>;

const WARMUP: usize = 200;
const BATCH: u32 = 512;

/// Read CYCCNT, run `BATCH` lockstep `push`→`recv` cycles, return the cycle and
/// allocation deltas over the measured window.
macro_rules! measure {
    ($reader:expr, $push:expr) => {{
        reset_allocs();
        let start = DWT::cycle_count();
        for i in 0..BATCH {
            let _ = $push(WARMUP as u64 + i as u64);
            let _ = block_on($reader.recv());
        }
        let cycles = DWT::cycle_count().wrapping_sub(start);
        (cycles, allocs())
    }};
}

/// Reports raw allocation totals (not divided by `BATCH`) — integer division
/// masked any regression of 1..511 allocs/window as a rounded-to-zero
/// "0 allocs/msg". The `assert!` is the actual pass/fail
/// gate: a regression panics the run (via the linked `panic-probe` handler)
/// instead of silently printing a misleading zero.
fn report(profile: &str, buffer: &str, cycles: u32, allocs: u32) {
    info!(
        "[B3] {=str} {=str}: {=u32} cycles/msg ({=u32} allocs TOTAL in {=u32} msgs) ({=u32} cycles total, batch={=u32})",
        profile,
        buffer,
        cycles / BATCH,
        allocs,
        BATCH,
        cycles,
        BATCH,
    );
    assert!(
        allocs == 0,
        "allocation regression: {} allocs in {} msgs",
        allocs,
        BATCH
    );
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Initialize the heap behind the counting allocator.
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 32 * 1024; // 32 KB
        static mut MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe {
            let mem_ptr = core::ptr::addr_of_mut!(MEM);
            HEAP.0.init((*mem_ptr).as_ptr() as usize, HEAP_SIZE);
        }
    }

    // DWT cycle counter. We only touch DCB/DWT, which Embassy does not use, so
    // stealing the core peripherals here is sound regardless of init ordering.
    // SAFETY: exclusive access to DCB/DWT for the lifetime of this benchmark;
    // no other code in this binary touches them.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    cp.DCB.enable_trace();
    cp.DWT.enable_cycle_counter();

    // Clock tree: HSE 8 MHz (from the ST-LINK MCO) → PLL1 → 250 MHz. Identical
    // to the other H563 demos in this repo.
    let mut config = embassy_stm32::Config::default();
    {
        use embassy_stm32::rcc::*;
        use embassy_stm32::time::Hertz;

        config.rcc.hsi = None;
        config.rcc.hsi48 = Some(Default::default());
        config.rcc.hse = Some(Hse {
            freq: Hertz(8_000_000),
            mode: HseMode::BypassDigital,
        });
        config.rcc.pll1 = Some(Pll {
            source: PllSource::Hse,
            prediv: PllPreDiv::Div2,
            mul: PllMul::Mul125,
            divp: Some(PllDiv::Div2),
            divq: Some(PllDiv::Div2),
            divr: None,
        });
        config.rcc.ahb_pre = AHBPrescaler::Div1;
        config.rcc.apb1_pre = APBPrescaler::Div1;
        config.rcc.apb2_pre = APBPrescaler::Div1;
        config.rcc.apb3_pre = APBPrescaler::Div1;
        config.rcc.sys = Sysclk::Pll1P;
        config.rcc.voltage_scale = VoltageScale::Scale0;
    }
    let _p = embassy_stm32::init(config);

    info!("=== AimDB B3 — Embassy buffer profiling on STM32H563ZI @ 250 MHz ===");
    info!(
        "cycle_counter={=bool}  warmup={=u32}  batch={=u32}",
        DWT::has_cycle_counter(),
        WARMUP as u32,
        BATCH
    );

    // ── Telemetry: SpmcRing / PubSubChannel ──────────────────────────────────
    //
    // `subscribe()` registers the SpmcRing Subscriber eagerly, so no
    // priming call is needed before the first push.
    {
        let buf: TelemetryBuffer = EmbassyBuffer::new_spmc();
        let mut reader = Reader::new(Box::new(buf.subscribe()));
        for i in 0..WARMUP {
            buf.push(telemetry_msg(i as u64));
            let _ = block_on(reader.recv());
        }
        let (cycles, n_allocs) = measure!(reader, |i| buf.push(telemetry_msg(i)));
        report("Telemetry", "SpmcRing    ", cycles, n_allocs);
    }

    // ── State: SingleLatest / Watch ──────────────────────────────────────────
    {
        let buf: StateBuffer = EmbassyBuffer::new_watch();
        let mut reader = Reader::new(Box::new(buf.subscribe()));
        for i in 0..WARMUP {
            buf.push(state_msg(i as u64));
            let _ = block_on(reader.recv());
        }
        let (cycles, n_allocs) = measure!(reader, |i| buf.push(state_msg(i)));
        report("State    ", "SingleLatest", cycles, n_allocs);
    }

    // ── Command: Mailbox / Channel(capacity=1) ───────────────────────────────
    {
        let buf: CommandBuffer = EmbassyBuffer::new_mailbox();
        let mut reader = Reader::new(Box::new(buf.subscribe()));
        for i in 0..WARMUP {
            buf.push(command_msg(i as u64));
            let _ = block_on(reader.recv());
        }
        let (cycles, n_allocs) = measure!(reader, |i| buf.push(command_msg(i)));
        report("Command  ", "Mailbox     ", cycles, n_allocs);
    }

    // ── Telemetry 1→4 fan-out ────────────────────────────────────────────────
    //
    // One publisher, four subscribers, lockstep. Reported per produced message
    // (each observed by all four readers — 4 deliveries/msg).
    {
        let buf: TelemetryBuffer = EmbassyBuffer::new_spmc();
        let mut r0 = Reader::new(Box::new(buf.subscribe()));
        let mut r1 = Reader::new(Box::new(buf.subscribe()));
        let mut r2 = Reader::new(Box::new(buf.subscribe()));
        let mut r3 = Reader::new(Box::new(buf.subscribe()));
        for i in 0..WARMUP {
            buf.push(telemetry_msg(i as u64));
            let _ = block_on(r0.recv());
            let _ = block_on(r1.recv());
            let _ = block_on(r2.recv());
            let _ = block_on(r3.recv());
        }
        reset_allocs();
        let start = DWT::cycle_count();
        for i in 0..BATCH {
            buf.push(telemetry_msg(WARMUP as u64 + i as u64));
            let _ = block_on(r0.recv());
            let _ = block_on(r1.recv());
            let _ = block_on(r2.recv());
            let _ = block_on(r3.recv());
        }
        let cycles = DWT::cycle_count().wrapping_sub(start);
        let n_allocs = allocs();
        info!(
            "[B3] Telemetry SpmcRing(1->4): {=u32} cycles/msg ({=u32} allocs TOTAL in {=u32} msgs) (4 deliveries/msg, {=u32} cycles total, batch={=u32})",
            cycles / BATCH,
            n_allocs,
            BATCH,
            cycles,
            BATCH,
        );
        assert!(
            n_allocs == 0,
            "allocation regression: {} allocs in {} msgs",
            n_allocs,
            BATCH
        );
    }

    info!("=== B3 complete — target=0 allocs/msg (W8 zero-alloc consume path) ===");

    loop {
        cortex_m::asm::wfe();
    }
}
