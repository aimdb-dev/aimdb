//! Embassy Adapter for AimDB
//!
//! This crate provides Embassy-specific extensions for AimDB, enabling the database
//! to run on embedded systems using the Embassy async runtime and embedded-hal
//! peripheral abstractions.
//!
//! # Features
//!
//! - **Embassy Integration**: Seamless integration with Embassy async executor
//! - **Hardware Abstraction**: Support for UART, ADC, GPIO, and Timer peripherals
//! - **Error Handling**: Embassy-specific error conversions and handling
//! - **No-std Compatible**: Designed for resource-constrained embedded systems
//! - **Configurable Task Pool**: Adjustable pool size for dynamic task spawning
//!
//! ## Task Pool Configuration
//!
//! The Embassy adapter uses a static task pool for dynamic spawning. By default,
//! it supports up to 8 concurrent dynamic tasks. You can increase this limit
//! by enabling one of the task pool feature flags:
//!
//! - `embassy-task-pool-8` (default): 8 concurrent dynamic tasks
//! - `embassy-task-pool-16`: 16 concurrent dynamic tasks  
//! - `embassy-task-pool-32`: 32 concurrent dynamic tasks
//!
//! Only enable one task pool feature at a time. If you exceed the pool size,
//! you'll get a `ExecutorError::SpawnFailed` error.
//!
//! Example in `Cargo.toml`:
//! ```toml
//! [dependencies]
//! aimdb-embassy-adapter = { version = "0.1", features = ["embassy-runtime", "embassy-task-pool-16"] }
//! ```
//!
//! # Architecture
//!
//! Embassy is a no_std async runtime, so this adapter is designed for embedded
//! environments and works with the no_std version of aimdb-core by default.
//!
//! The adapter extends AimDB's core functionality without requiring embassy
//! dependencies in the core crate. It provides:
//!
//! - Hardware error constructors for common MCU peripherals
//! - Automatic conversions from embedded-hal error types
//! - Embassy-specific const functions for zero-overhead error handling
//!
//! # Usage
//!
//! ```rust,no_run
//! // This example only works when std feature is not enabled
//! // Embassy adapter is designed exclusively for no_std environments
//! # #[cfg(not(feature = "std"))]
//! # {
//! use aimdb_core::DbError;
//! use aimdb_embassy_adapter::EmbassyErrorSupport;
//!
//! // Convert from nb errors (production-critical for embedded-hal)
//! let nb_error: embedded_hal_nb::nb::Error<DbError> = embedded_hal_nb::nb::Error::WouldBlock;
//! let db_error = DbError::from_nb_error(nb_error);
//!
//! // Create hardware errors directly (recommended approach)
//! let uart_error = DbError::HardwareError {
//!     component: 4,  // UART component ID
//!     error_code: 0x6210,
//!     _description: (),
//! };
//! # }
//! ```
//!
//! # Error Code Ranges
//!
//! The adapter uses specific error code ranges for different peripherals:
//!
//! - **UART**: 0x6200-0x62FF  
//! - **ADC**: 0x6400-0x64FF
//! - **GPIO**: 0x6500-0x65FF
//! - **Timer**: 0x6600-0x66FF

#![no_std]

extern crate alloc;

// Only include the implementation when std feature is not enabled
// Embassy adapter is designed exclusively for no_std environments
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
pub mod buffer;

#[cfg(not(feature = "std"))]
mod error;

#[cfg(not(feature = "std"))]
mod runtime;

#[cfg(all(not(feature = "std"), feature = "embassy-time"))]
pub mod time;

// Error handling exports
#[cfg(not(feature = "std"))]
pub use error::EmbassyErrorSupport;

// Buffer implementation exports
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
pub use buffer::EmbassyBuffer;

// Runtime adapter exports
#[cfg(feature = "embassy-runtime")]
pub use runtime::EmbassyAdapter;

/// Type alias for Embassy database
///
/// This provides a convenient type for working with databases on the Embassy runtime.
/// Most users should use `AimDbBuilder` directly to create databases.
#[cfg(feature = "embassy-runtime")]
pub type EmbassyDatabase = aimdb_core::Database<EmbassyAdapter>;

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

// Generate extension trait for Embassy adapter using the macro
#[cfg(all(not(feature = "std"), feature = "embassy-sync"))]
aimdb_core::impl_record_registrar_ext! {
    EmbassyRecordRegistrarExt,
    EmbassyAdapter,
    EmbassyBuffer,
    ["embassy-runtime", "embassy-sync"],
    |cfg| EmbassyBuffer::<T, 16, 4, 4, 1>::new(cfg)
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
    /// **For SPMC Ring Buffer:**
    /// ```ignore
    /// // Small buffer: 16 items, 2 consumers
    /// reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
    ///
    /// // Large buffer: 64 items, 4 consumers  
    /// reg.buffer_sized::<64, 4>(EmbassyBufferType::SpmcRing)
    /// ```
    ///
    /// **For SingleLatest (only latest value stored):**
    /// ```ignore
    /// // 4 consumers watching the latest value
    /// reg.buffer_sized::<1, 4>(EmbassyBufferType::SingleLatest)
    /// ```
    ///
    /// **For Mailbox (single-slot overwrite):**
    /// ```ignore
    /// // Parameters are ignored, single slot
    /// reg.buffer_sized::<1, 4>(EmbassyBufferType::Mailbox)
    /// ```
    ///
    /// # Rule of Thumb
    /// - Set `CAP` to your desired ring buffer size for SPMC
    /// - Set `CONSUMERS` to match your number of `.tap()` consumers
    fn buffer_sized<const CAP: usize, const CONSUMERS: usize>(
        &'a mut self,
        buffer_type: EmbassyBufferType,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, EmbassyAdapter>;
}

#[cfg(all(feature = "embassy-runtime", feature = "embassy-sync"))]
impl<'a, T> EmbassyRecordRegistrarExtCustom<'a, T>
    for aimdb_core::RecordRegistrar<'a, T, EmbassyAdapter>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn buffer_sized<const CAP: usize, const CONSUMERS: usize>(
        &'a mut self,
        buffer_type: EmbassyBufferType,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, EmbassyAdapter> {
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
        self.buffer_raw(buffer)
    }
}
