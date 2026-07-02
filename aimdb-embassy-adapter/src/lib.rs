//! Embassy Adapter for AimDB
//!
//! This crate provides Embassy-specific extensions for AimDB, enabling the database
//! to run on embedded systems using the Embassy async runtime.
//!
//! # Features
//!
//! - **Embassy Integration**: Seamless integration with Embassy async executor
//! - **No-std Compatible**: Designed for resource-constrained embedded systems
//!
//! # Architecture
//!
//! Embassy is a no_std async runtime, so this adapter is designed for embedded
//! environments and works with the no_std version of aimdb-core by default.
//! It provides the runtime ([`EmbassyAdapter`] implementing
//! `aimdb_core::RuntimeOps`), the buffer implementations ([`EmbassyBuffer`]),
//! and the connector spines (`connectors` feature).

#![no_std]

extern crate alloc;

// Only include the implementation when std feature is not enabled
// Embassy adapter is designed exclusively for no_std environments
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
pub mod buffer;

#[cfg(not(feature = "std"))]
mod runtime;

// Force-`Send` helper for Embassy data-plane connectors (see module docs).
#[cfg(not(feature = "std"))]
pub mod send_wrapper;

// Centralized Embassy connector spines (session + data-plane) — the one audited
// home for the single-core `unsafe` + `SendFutureWrapper`.
#[cfg(all(not(feature = "std"), feature = "connectors"))]
pub mod connectors;

/// Link stubs for **host** test binaries that touch the Embassy adapter
/// (035 §2.4): a no-op `#[defmt::global_logger]` + `#[defmt::panic_handler]`
/// and a pinned-at-0 embassy-time driver.
///
/// Coercing `EmbassyAdapter` to `Arc<dyn RuntimeOps>` instantiates a vtable
/// whose `log` entry calls `defmt::*` unconditionally, so the `_defmt_*`
/// extern symbols must resolve in every host test binary that links the
/// adapter — and `#[global_logger]`/the time driver must be defined **once
/// per binary**, so a shared `#[cfg(test)]` item cannot serve integration
/// tests. This macro keeps the definition in one place; each test binary
/// expands it exactly once at top level (or once in the lib's test module).
///
/// The invoking crate needs `defmt` and `embassy-time-driver` resolvable
/// (dev-dependencies are enough).
///
/// The time driver pins the clock at 0 and `schedule_wake` wakes immediately,
/// so an already-expired timer (e.g. `sleep(Duration::ZERO)`) completes on its
/// next poll. Non-zero sleeps would spin forever — host tests must not use
/// them.
#[macro_export]
#[doc(hidden)]
macro_rules! host_test_stubs {
    () => {
        #[defmt::global_logger]
        struct HostTestLogger;

        unsafe impl defmt::Logger for HostTestLogger {
            fn acquire() {}
            unsafe fn flush() {}
            unsafe fn release() {}
            unsafe fn write(_bytes: &[u8]) {}
        }

        #[defmt::panic_handler]
        fn defmt_panic() -> ! {
            core::panic!("defmt panic in host test")
        }

        struct HostTestTimeDriver;
        impl embassy_time_driver::Driver for HostTestTimeDriver {
            fn now(&self) -> u64 {
                0
            }
            fn schedule_wake(&self, _at: u64, waker: &core::task::Waker) {
                waker.wake_by_ref();
            }
        }
        embassy_time_driver::time_driver_impl!(
            static HOST_TEST_TIME_DRIVER: HostTestTimeDriver = HostTestTimeDriver
        );
    };
}

#[cfg(not(feature = "std"))]
pub use send_wrapper::SendFutureWrapper;

// Runtime exports
#[cfg(not(feature = "std"))]
pub use runtime::EmbassyAdapter;

// Buffer implementation exports
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
pub use buffer::EmbassyBuffer;

// Re-export core types for convenience
#[cfg(all(not(feature = "std"), feature = "embassy-runtime"))]
pub use embassy_executor::Spawner;

/// Embassy-specific delay function
///
/// Delays execution for the specified number of milliseconds using Embassy's
/// timer system. This should be used in services running on Embassy runtime.
///
/// # Arguments
/// * `ms` - Number of milliseconds to delay
///
/// # Example
/// ```rust,no_run
/// # #[cfg(all(not(feature = "std"), feature = "embassy-time"))]
/// # {
/// use aimdb_embassy_adapter::delay_ms;
///
/// # async fn example() {
/// delay_ms(1000).await; // Wait 1 second using Embassy timer
/// # }
/// # }
/// ```
#[cfg(all(not(feature = "std"), feature = "embassy-time"))]
pub async fn delay_ms(ms: u64) {
    embassy_time::Timer::after(embassy_time::Duration::from_millis(ms)).await;
}

/// Embassy-specific yield function
///
/// Yields control to allow other Embassy tasks to run. This provides
/// cooperative scheduling within the Embassy async runtime.
///
/// # Example
/// ```rust,no_run
/// # #[cfg(not(feature = "std"))]
/// # {
/// use aimdb_embassy_adapter::yield_now;
///
/// # async fn example() {
/// yield_now().await;
/// # }
/// # }
/// ```
#[cfg(not(feature = "std"))]
pub async fn yield_now() {
    // Embassy doesn't have a built-in yield_now, but we can simulate it
    // by using a very short timer delay
    #[cfg(feature = "embassy-time")]
    embassy_time::Timer::after(embassy_time::Duration::from_millis(0)).await;

    // If embassy-time is not available, we can use core::future::pending
    // and immediately wake, but for simplicity, we'll use a no-op for now
    #[cfg(not(feature = "embassy-time"))]
    {
        // Simple yield implementation - just await a ready future
        core::future::ready(()).await;
    }
}

/// Buffer-construction extension for [`aimdb_core::RecordRegistrar`].
///
/// Buffer construction is the one genuinely adapter-specific registration
/// step left after issue #131 — `source()` / `tap()` / `transform()` are
/// inherent methods on the registrar. This trait adds `.buffer(cfg)` backed
/// by [`EmbassyBuffer`] with conservative default const generics
/// (`CAP=16`, `SUBS=4`, `PUBS=4`, `WATCH_N=1`); use
/// [`EmbassyRecordRegistrarExtCustom::buffer_sized`] for precise sizing.
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
pub trait EmbassyRecordRegistrarExt<T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Configures an [`EmbassyBuffer`] from the given configuration.
    fn buffer(&mut self, cfg: aimdb_core::buffer::BufferCfg) -> &mut Self;
}

#[cfg(all(feature = "embassy-runtime", feature = "embassy-sync"))]
impl<T> EmbassyRecordRegistrarExt<T> for aimdb_core::RecordRegistrar<'_, T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn buffer(&mut self, cfg: aimdb_core::buffer::BufferCfg) -> &mut Self {
        use aimdb_core::buffer::Buffer;
        use alloc::boxed::Box;
        let buffer = Box::new(EmbassyBuffer::<T, 16, 4, 4, 1>::new(&cfg));
        // Record the cfg so buffer_info() reports the real buffer
        // type/capacity for the dependency graph.
        self.buffer_with_cfg(buffer, cfg)
    }
}

/// Buffer type selection for Embassy adapters
///
/// This enum allows explicit selection of buffer semantics when configuring
/// Embassy buffers with custom const generics.
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbassyBufferType {
    /// SPMC ring buffer - multiple consumers can read independently
    /// Capacity specified via const generic CAP
    SpmcRing,

    /// Single-latest buffer - all consumers get the most recent value
    /// Only stores one value, CAP const generic is ignored (typically use 1)
    SingleLatest,

    /// Mailbox buffer - single slot with overwrite semantics
    /// CAP const generic is ignored (typically use 1)
    Mailbox,
}

/// Additional Embassy-specific extension methods for buffer configuration
///
/// These methods allow fine-grained control over Embassy buffer const generics,
/// which is necessary for embedded systems with limited resources.
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
pub trait EmbassyRecordRegistrarExtCustom<'a, T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Configure buffer with custom size parameters for Embassy
    ///
    /// This allows precise control over buffer sizing for embedded systems.
    /// The default `.buffer()` method uses conservative defaults (CAP=16, CONSUMERS=4),
    /// but you can optimize for your specific use case with this method.
    ///
    /// # Type Parameters
    /// - `CAP`: Buffer capacity
    ///   - For SPMC Ring: Ring buffer size (e.g., 16, 32, 64)
    ///   - For SingleLatest/Mailbox: Ignored (set to 1)
    /// - `CONSUMERS`: Maximum concurrent consumers
    ///   - For SPMC Ring: Used as SUBS (independent read positions in ring)
    ///   - For SingleLatest: Used as WATCH_N (watchers of latest value)
    ///   - For Mailbox: Ignored
    ///
    /// # Arguments
    /// - `buffer_type`: Explicit buffer type selection
    ///
    /// # Recommended Values
    ///
    /// - SPMC ring: `buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)` for a
    ///   small buffer (16 items, 2 consumers); `::<64, 4>` for a large one.
    /// - SingleLatest: `buffer_sized::<1, 4>(EmbassyBufferType::SingleLatest)`
    ///   — only the latest value is stored, 4 consumers watch it.
    /// - Mailbox: `buffer_sized::<1, 4>(EmbassyBufferType::Mailbox)` — single
    ///   slot with overwrite; the parameters are ignored.
    ///
    /// # Counting CONSUMERS
    ///
    /// `CONSUMERS` must cover **every** entity that calls `.subscribe()` on this record:
    /// - Each `.tap()` consumer: +1
    /// - Each `.link_to()` outbound connector: +1
    /// - Each `transform_join` that lists this record as an input: +1
    ///
    /// Exhausting the slot count is a silent failure at runtime (the subscriber exits
    /// immediately, producing no output). A `defmt::error!` is emitted, but set
    /// `CONSUMERS` high enough to avoid it altogether.
    fn buffer_sized<const CAP: usize, const CONSUMERS: usize>(
        &mut self,
        buffer_type: EmbassyBufferType,
    ) -> &mut aimdb_core::RecordRegistrar<'a, T>;

    /// Registers a producer with additional context (e.g., hardware peripherals)
    ///
    /// This is an extension of `.source()` that allows passing extra context
    /// to the producer function. This is particularly useful for hardware-dependent
    /// producers that need access to GPIO pins, UART handles, sensors, etc.
    ///
    /// # Type Parameters
    /// - `Ctx`: Additional context type (e.g., `ExtiInput`, UART handle, sensor interface)
    /// - `F`: Closure that takes (RuntimeContext, Producer, Context) and returns a Future
    ///
    /// # Example
    ///
    /// Illustrative (not compiled: uses device peripherals on a thumb target):
    ///
    /// ```ignore
    /// use embassy_stm32::exti::ExtiInput;
    ///
    /// builder.configure::<LightControl>(|reg| {
    ///     reg.buffer_sized::<8, 2>(EmbassyBufferType::SingleLatest)
    ///         .link_to("knx://1/0/6")
    ///         .source_with_context(button, button_handler)
    ///         .finish()
    /// });
    ///
    /// async fn button_handler(
    ///     ctx: RuntimeContext,
    ///     producer: Producer<LightControl>,
    ///     button: ExtiInput<'static>,
    /// ) {
    ///     // Use the button peripheral
    ///     button.wait_for_falling_edge().await;
    ///     // Produce data...
    /// }
    /// ```
    fn source_with_context<Ctx, F, Fut>(
        &mut self,
        context: Ctx,
        f: F,
    ) -> &mut aimdb_core::RecordRegistrar<'a, T>
    where
        Ctx: Send + 'static,
        F: FnOnce(aimdb_core::RuntimeContext, aimdb_core::Producer<T>, Ctx) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static;
}

#[cfg(all(feature = "embassy-runtime", feature = "embassy-sync"))]
impl<'a, T> EmbassyRecordRegistrarExtCustom<'a, T> for aimdb_core::RecordRegistrar<'a, T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn buffer_sized<const CAP: usize, const CONSUMERS: usize>(
        &mut self,
        buffer_type: EmbassyBufferType,
    ) -> &mut aimdb_core::RecordRegistrar<'a, T> {
        use aimdb_core::buffer::{Buffer, BufferCfg};
        use alloc::boxed::Box;

        // PUBS is hardcoded to 1 for typical SPMC patterns (internal implementation detail)
        const PUBS: usize = 1;

        // CONSUMERS parameter is used differently based on buffer type:
        // - SPMC Ring: CONSUMERS -> SUBS (independent ring positions)
        // - SingleLatest: CONSUMERS -> WATCH_N (latest value watchers)
        // - Mailbox: CONSUMERS is ignored

        // Create BufferCfg based on explicit buffer type selection
        let cfg = match buffer_type {
            EmbassyBufferType::SpmcRing => BufferCfg::SpmcRing { capacity: CAP },
            EmbassyBufferType::SingleLatest => BufferCfg::SingleLatest,
            EmbassyBufferType::Mailbox => BufferCfg::Mailbox,
        };

        // Map CONSUMERS to appropriate Embassy generic parameter
        let buffer = Box::new(EmbassyBuffer::<T, CAP, CONSUMERS, PUBS, CONSUMERS>::new(
            &cfg,
        ));
        // Record the cfg so buffer_info() reports the real buffer type/capacity
        // for the dependency graph.
        self.buffer_with_cfg(buffer, cfg)
    }

    fn source_with_context<Ctx, F, Fut>(
        &mut self,
        context: Ctx,
        f: F,
    ) -> &mut aimdb_core::RecordRegistrar<'a, T>
    where
        Ctx: Send + 'static,
        F: FnOnce(aimdb_core::RuntimeContext, aimdb_core::Producer<T>, Ctx) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        self.source(|ctx, producer| f(ctx, producer, context))
    }
}
