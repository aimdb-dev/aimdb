//! Embassy-specific error handling support
//!
//! This module provides traits and implementations that add Embassy
//! and embedded-hal specific functionality to AimDB's core error types.
//!
//! Embassy is a no_std async runtime, so this adapter always works in no_std mode
//! and uses the no_std field names from DbError.
//!
//! This crate is excluded from the main workspace to prevent feature unification issues
//! that would enable std mode. Build it separately with: cargo build -p aimdb-embassy-adapter

use aimdb_core::DbError;
use embedded_hal_nb::nb;

// Embassy Error Code Base Values
const UART_ERROR_BASE: u16 = 0x6200;
const ADC_ERROR_BASE: u16 = 0x6400;
const GPIO_ERROR_BASE: u16 = 0x6500;
const TIMER_ERROR_BASE: u16 = 0x6600;

// Component IDs for Embassy hardware
const TIMER_COMPONENT_ID: u8 = 0;
const GPIO_COMPONENT_ID: u8 = 1;
const UART_COMPONENT_ID: u8 = 4;
const ADC_COMPONENT_ID: u8 = 5;

/// Trait that provides Embassy-specific error constructors for DbError
///
/// This trait provides hardware-specific error creation methods without requiring
/// the core AimDB crate to depend on embassy or embedded-hal.
pub trait EmbassyErrorSupport {
    /// Creates a UART error for Embassy environments (error codes 0x6200-0x62FF)
    fn from_uart_error(code: u8) -> Self;

    /// Creates an ADC error for Embassy environments (error codes 0x6400-0x64FF)
    fn from_adc_error(code: u8) -> Self;

    /// Creates a GPIO error for Embassy environments (error codes 0x6500-0x65FF)
    fn from_gpio_error(code: u8) -> Self;

    /// Creates a Timer error for Embassy environments (error codes 0x6600-0x66FF)
    fn from_timer_error(code: u8) -> Self;

    /// Converts a non-blocking error to DbError
    fn from_nb_error<E>(error: nb::Error<E>) -> Self
    where
        E: Into<Self>,
        Self: Sized;
}

impl EmbassyErrorSupport for DbError {
    /// Creates a UART error for Embassy environments (error codes 0x6200-0x62FF)
    fn from_uart_error(code: u8) -> Self {
        DbError::HardwareError {
            component: UART_COMPONENT_ID,
            error_code: UART_ERROR_BASE | (code as u16),
            _description: (),
        }
    }

    /// Creates an ADC error for Embassy environments (error codes 0x6400-0x64FF)
    fn from_adc_error(code: u8) -> Self {
        DbError::HardwareError {
            component: ADC_COMPONENT_ID,
            error_code: ADC_ERROR_BASE | (code as u16),
            _description: (),
        }
    }

    /// Creates a GPIO error for Embassy environments (error codes 0x6500-0x65FF)
    fn from_gpio_error(code: u8) -> Self {
        DbError::HardwareError {
            component: GPIO_COMPONENT_ID,
            error_code: GPIO_ERROR_BASE | (code as u16),
            _description: (),
        }
    }

    /// Creates a Timer error for Embassy environments (error codes 0x6600-0x66FF)
    fn from_timer_error(code: u8) -> Self {
        DbError::HardwareError {
            component: TIMER_COMPONENT_ID,
            error_code: TIMER_ERROR_BASE | (code as u16),
            _description: (),
        }
    }

    /// Converts a non-blocking error to DbError
    fn from_nb_error<E>(error: nb::Error<E>) -> Self
    where
        E: Into<Self>,
        Self: Sized,
    {
        match error {
            nb::Error::Other(e) => e.into(),
            nb::Error::WouldBlock => DbError::ResourceUnavailable {
                resource_type: DbError::RESOURCE_TYPE_WOULD_BLOCK,
                _resource_name: (),
            },
        }
    }
}

/// Converter functions for embedded-hal errors to DbError
pub struct EmbassyErrorConverter;

impl EmbassyErrorConverter {
    /// Converts an nb::Error to DbError
    pub fn from_nb<E>(error: nb::Error<E>) -> DbError
    where
        E: Into<DbError>,
    {
        DbError::from_nb_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embassy_hardware_error_constructors() {
        // Test Embassy-specific const constructors for hardware errors

        // Test UART error constructor (0x6200-0x62FF range)
        let uart_error = DbError::from_uart_error(0x10);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = uart_error
        {
            assert_eq!(component, UART_COMPONENT_ID);
            assert_eq!(error_code, UART_ERROR_BASE | 0x10);
        } else {
            panic!("Expected HardwareError variant");
        }

        // Test ADC error constructor (0x6400-0x64FF range)
        let adc_error = DbError::from_adc_error(0x20);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = adc_error
        {
            assert_eq!(component, ADC_COMPONENT_ID);
            assert_eq!(error_code, ADC_ERROR_BASE | 0x20);
        } else {
            panic!("Expected HardwareError variant");
        }

        // Test GPIO error constructor (0x6500-0x65FF range)
        let gpio_error = DbError::from_gpio_error(0x08);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = gpio_error
        {
            assert_eq!(component, GPIO_COMPONENT_ID);
            assert_eq!(error_code, GPIO_ERROR_BASE | 0x08);
        } else {
            panic!("Expected HardwareError variant");
        }

        // Test Timer error constructor (0x6600-0x66FF range)
        let timer_error = DbError::from_timer_error(0xFF);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = timer_error
        {
            assert_eq!(component, TIMER_COMPONENT_ID);
            assert_eq!(error_code, TIMER_ERROR_BASE | 0xFF);
        } else {
            panic!("Expected HardwareError variant");
        }
    }

    #[test]
    fn test_embedded_hal_error_conversions() {
        // This test is simplified since we no longer have SPI/I2C HAL error conversions
        // We only test nb error conversion functionality

        // Test nb error conversion with a known error
        let known_error = DbError::from_uart_error(0x42);
        let nb_other_error: nb::Error<DbError> = nb::Error::Other(known_error);
        let converted_error = DbError::from_nb_error(nb_other_error);

        // The converted error should have the same code as the original (0x6001 for all hardware errors)
        assert_eq!(converted_error.error_code(), 0x6001);
    }

    #[test]
    fn test_nb_error_conversion() {
        // Test nb::Error::WouldBlock conversion (use DbError directly since it can convert to itself)
        let would_block: nb::Error<DbError> = nb::Error::WouldBlock;
        let db_error = DbError::from_nb_error(would_block);

        if let DbError::ResourceUnavailable { resource_type, .. } = db_error {
            assert_eq!(resource_type, DbError::RESOURCE_TYPE_WOULD_BLOCK);
        } else {
            panic!("Expected ResourceUnavailable variant");
        }

        // Test nb::Error::Other conversion with a known error
        let known_error = DbError::from_uart_error(0x42);
        let nb_other_error: nb::Error<DbError> = nb::Error::Other(known_error);
        let converted_error = DbError::from_nb_error(nb_other_error);

        // The converted error should have the same code as the original (0x6001 for all hardware errors)
        assert_eq!(converted_error.error_code(), 0x6001);
    }

    #[test]
    fn test_embassy_error_code_ranges() {
        // Test that Embassy hardware errors are properly categorized
        // Note: All HardwareError variants return 0x6001 from error_code(),
        // but have different component-specific error_code fields

        let uart_error = DbError::from_uart_error(0x00);
        assert_eq!(uart_error.error_category(), 0x6000); // Hardware category
        assert_eq!(uart_error.error_code(), 0x6001); // All hardware errors use this unified code

        let adc_error = DbError::from_adc_error(0x00);
        assert_eq!(adc_error.error_category(), 0x6000); // Hardware category
        assert_eq!(adc_error.error_code(), 0x6001);

        let gpio_error = DbError::from_gpio_error(0x00);
        assert_eq!(gpio_error.error_category(), 0x6000); // Hardware category
        assert_eq!(gpio_error.error_code(), 0x6001);

        let timer_error = DbError::from_timer_error(0x00);
        assert_eq!(timer_error.error_category(), 0x6000); // Hardware category
        assert_eq!(timer_error.error_code(), 0x6001);
    }

    #[test]
    fn test_converter_functions() {
        // Test EmbassyErrorConverter functions

        // Test nb error conversion
        let would_block: nb::Error<DbError> = nb::Error::WouldBlock;
        let db_error = EmbassyErrorConverter::from_nb(would_block);
        if let DbError::ResourceUnavailable { resource_type, .. } = db_error {
            assert_eq!(resource_type, DbError::RESOURCE_TYPE_WOULD_BLOCK);
        } else {
            panic!("Expected ResourceUnavailable variant");
        }
    }

    #[test]
    fn test_error_construction() {
        // Test that constructors work correctly at runtime

        let uart_error = DbError::from_uart_error(0x02);
        let adc_error = DbError::from_adc_error(0x04);
        let gpio_error = DbError::from_gpio_error(0x05);
        let timer_error = DbError::from_timer_error(0x06);

        // Verify the construction worked correctly (all hardware errors use 0x6001)
        assert_eq!(uart_error.error_code(), 0x6001);
        assert_eq!(adc_error.error_code(), 0x6001);
        assert_eq!(gpio_error.error_code(), 0x6001);
        assert_eq!(timer_error.error_code(), 0x6001);

        // Verify they're all hardware errors
        assert!(uart_error.is_hardware_error());
        assert!(adc_error.is_hardware_error());
        assert!(gpio_error.is_hardware_error());
        assert!(timer_error.is_hardware_error());
    }
}
