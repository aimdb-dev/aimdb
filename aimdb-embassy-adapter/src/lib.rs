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
//! use aimdb_embassy_adapter::{EmbassyErrorSupport, EmbassyErrorConverter};
//!
//! // Create hardware-specific errors
//! let uart_error = DbError::from_uart_error(0x42);
//! let adc_error = DbError::from_adc_error(0x10);
//! let gpio_error = DbError::from_gpio_error(0x05);
//! let timer_error = DbError::from_timer_error(0x20);
//!
//! // Convert from nb errors using the converter
//! let nb_error: embedded_hal_nb::nb::Error<DbError> = embedded_hal_nb::nb::Error::WouldBlock;
//! let db_error = EmbassyErrorConverter::from_nb(nb_error);
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

// Only include the implementation when std feature is not enabled
// Embassy adapter is designed exclusively for no_std environments
#[cfg(not(feature = "std"))]
mod error;

#[cfg(not(feature = "std"))]
pub use error::{EmbassyErrorConverter, EmbassyErrorSupport};
