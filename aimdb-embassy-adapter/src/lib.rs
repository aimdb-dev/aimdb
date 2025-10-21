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

#[cfg(not(feature = "std"))]
pub mod database;

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

// Database implementation exports
#[cfg(feature = "embassy-runtime")]
pub use database::{
    EmbassyDatabase, EmbassyDatabaseBuilder, EmbassyDatabaseSpec, EmbassyDatabaseSpecBuilder,
};

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
