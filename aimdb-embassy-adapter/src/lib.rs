//! Embassy Adapter for AimDB
//!
//! This crate provides Embassy-specific extensions for AimDB, enabling the database
//! to run on embedded systems using the Embassy async runtime and embedded-hal
//! peripheral abstractions.
//!
//! # Features
//!
//! - **Embassy Integration**: Seamless integration with Embassy async executor
//! - **Hardware Abstraction**: Support for SPI, I2C, UART, ADC, GPIO, and Timer peripherals
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
//! ```rust
//! use aimdb_core::DbError;
//! use aimdb_embassy_adapter::{EmbassyErrorSupport, EmbassyErrorConverter};
//!
//! // Create hardware-specific errors
//! let spi_error = DbError::from_spi_error(0x42);
//! let i2c_error = DbError::from_i2c_error(0x10);
//!
//! // Convert from embedded-hal errors using the converter
//! let hal_error = embedded_hal::spi::ErrorKind::Overrun;
//! let db_error = EmbassyErrorConverter::from_spi(hal_error);
//! ```
//!
//! # Error Code Ranges
//!
//! The adapter uses specific error code ranges for different peripherals:
//!
//! - **SPI**: 0x6100-0x61FF
//! - **UART**: 0x6200-0x62FF  
//! - **I2C**: 0x6300-0x63FF
//! - **ADC**: 0x6400-0x64FF
//! - **GPIO**: 0x6500-0x65FF
//! - **Timer**: 0x6600-0x66FF

#![no_std]

mod error;

pub use error::{EmbassyErrorConverter, EmbassyErrorSupport};
