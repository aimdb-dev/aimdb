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
use embedded_hal::i2c;
use embedded_hal::spi;
use embedded_hal_nb::nb;

/// Trait that provides Embassy-specific error constructors for DbError
///
/// This trait provides hardware-specific error creation methods without requiring
/// the core AimDB crate to depend on embassy or embedded-hal.
pub trait EmbassyErrorSupport {
    /// Creates an SPI error for Embassy environments (error codes 0x6100-0x61FF)
    fn from_spi_error(code: u8) -> Self;

    /// Creates a UART error for Embassy environments (error codes 0x6200-0x62FF)
    fn from_uart_error(code: u8) -> Self;

    /// Creates an I2C error for Embassy environments (error codes 0x6300-0x63FF)
    fn from_i2c_error(code: u8) -> Self;

    /// Creates an ADC error for Embassy environments (error codes 0x6400-0x64FF)
    fn from_adc_error(code: u8) -> Self;

    /// Creates a GPIO error for Embassy environments (error codes 0x6500-0x65FF)
    fn from_gpio_error(code: u8) -> Self;

    /// Creates a Timer error for Embassy environments (error codes 0x6600-0x66FF)
    fn from_timer_error(code: u8) -> Self;

    /// Converts an SPI error to DbError
    fn from_spi_hal_error(error: spi::ErrorKind) -> Self;

    /// Converts an I2C error to DbError
    fn from_i2c_hal_error(error: i2c::ErrorKind) -> Self;

    /// Converts a non-blocking error to DbError
    fn from_nb_error<E>(error: nb::Error<E>) -> Self
    where
        E: Into<Self>,
        Self: Sized;
}

impl EmbassyErrorSupport for DbError {
    /// Creates an SPI error for Embassy environments (error codes 0x6100-0x61FF)
    fn from_spi_error(code: u8) -> Self {
        DbError::HardwareError {
            component: 2, // SPI component ID
            error_code: 0x6100 | (code as u16),
            _description: (),
        }
    }

    /// Creates a UART error for Embassy environments (error codes 0x6200-0x62FF)
    fn from_uart_error(code: u8) -> Self {
        DbError::HardwareError {
            component: 4, // UART component ID
            error_code: 0x6200 | (code as u16),
            _description: (),
        }
    }

    /// Creates an I2C error for Embassy environments (error codes 0x6300-0x63FF)
    fn from_i2c_error(code: u8) -> Self {
        DbError::HardwareError {
            component: 3, // I2C component ID
            error_code: 0x6300 | (code as u16),
            _description: (),
        }
    }

    /// Creates an ADC error for Embassy environments (error codes 0x6400-0x64FF)
    fn from_adc_error(code: u8) -> Self {
        DbError::HardwareError {
            component: 5, // ADC component ID
            error_code: 0x6400 | (code as u16),
            _description: (),
        }
    }

    /// Creates a GPIO error for Embassy environments (error codes 0x6500-0x65FF)
    fn from_gpio_error(code: u8) -> Self {
        DbError::HardwareError {
            component: 1, // GPIO component ID
            error_code: 0x6500 | (code as u16),
            _description: (),
        }
    }

    /// Creates a Timer error for Embassy environments (error codes 0x6600-0x66FF)
    fn from_timer_error(code: u8) -> Self {
        DbError::HardwareError {
            component: 0, // Timer component ID
            error_code: 0x6600 | (code as u16),
            _description: (),
        }
    }

    /// Converts an SPI error to DbError
    fn from_spi_hal_error(error: spi::ErrorKind) -> Self {
        let error_code = match error {
            spi::ErrorKind::ChipSelectFault => 0x01,
            spi::ErrorKind::ModeFault => 0x02,
            spi::ErrorKind::Overrun => 0x04,
            spi::ErrorKind::FrameFormat => 0x05,
            _ => 0xFF, // Generic/unknown error
        };
        Self::from_spi_error(error_code)
    }

    /// Converts an I2C error to DbError
    fn from_i2c_hal_error(error: i2c::ErrorKind) -> Self {
        let error_code = match error {
            i2c::ErrorKind::Bus => 0x01,
            i2c::ErrorKind::ArbitrationLoss => 0x02,
            i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Address) => 0x03,
            i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Data) => 0x04,
            i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Unknown) => 0x05,
            i2c::ErrorKind::Overrun => 0x06,
            _ => 0xFF, // Generic/unknown error
        };
        Self::from_i2c_error(error_code)
    }

    /// Converts a non-blocking error to DbError
    fn from_nb_error<E>(error: nb::Error<E>) -> Self
    where
        E: Into<Self>,
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
    /// Converts an SPI ErrorKind to DbError
    pub fn from_spi(error: spi::ErrorKind) -> DbError {
        DbError::from_spi_hal_error(error)
    }

    /// Converts an I2C ErrorKind to DbError
    pub fn from_i2c(error: i2c::ErrorKind) -> DbError {
        DbError::from_i2c_hal_error(error)
    }

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

        // Test SPI error constructor (0x6100-0x61FF range)
        let spi_error = DbError::from_spi_error(0x42);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = spi_error
        {
            assert_eq!(component, 2); // SPI component ID
            assert_eq!(error_code, 0x6142); // 0x6100 | 0x42
        } else {
            panic!("Expected HardwareError variant");
        }

        // Test UART error constructor (0x6200-0x62FF range)
        let uart_error = DbError::from_uart_error(0x10);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = uart_error
        {
            assert_eq!(component, 4); // UART component ID
            assert_eq!(error_code, 0x6210); // 0x6200 | 0x10
        } else {
            panic!("Expected HardwareError variant");
        }

        // Test I2C error constructor (0x6300-0x63FF range)
        let i2c_error = DbError::from_i2c_error(0x05);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = i2c_error
        {
            assert_eq!(component, 3); // I2C component ID
            assert_eq!(error_code, 0x6305); // 0x6300 | 0x05
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
            assert_eq!(component, 5); // ADC component ID
            assert_eq!(error_code, 0x6420); // 0x6400 | 0x20
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
            assert_eq!(component, 1); // GPIO component ID
            assert_eq!(error_code, 0x6508); // 0x6500 | 0x08
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
            assert_eq!(component, 0); // Timer component ID
            assert_eq!(error_code, 0x66FF); // 0x6600 | 0xFF
        } else {
            panic!("Expected HardwareError variant");
        }
    }

    #[test]
    fn test_embedded_hal_error_conversions() {
        use crate::EmbassyErrorConverter;

        // Test SPI error conversions
        let spi_overrun = EmbassyErrorConverter::from_spi(spi::ErrorKind::Overrun);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = spi_overrun
        {
            assert_eq!(component, 2); // SPI component
            assert_eq!(error_code, 0x6104); // 0x6100 | 0x04
        } else {
            panic!("Expected HardwareError variant");
        }

        let spi_mode_fault = EmbassyErrorConverter::from_spi(spi::ErrorKind::ModeFault);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = spi_mode_fault
        {
            assert_eq!(component, 2); // SPI component
            assert_eq!(error_code, 0x6102); // 0x6100 | 0x02
        } else {
            panic!("Expected HardwareError variant");
        }

        // Test I2C error conversions
        let i2c_bus_error = EmbassyErrorConverter::from_i2c(i2c::ErrorKind::Bus);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = i2c_bus_error
        {
            assert_eq!(component, 3); // I2C component
            assert_eq!(error_code, 0x6301); // 0x6300 | 0x01
        } else {
            panic!("Expected HardwareError variant");
        }

        let i2c_nack = EmbassyErrorConverter::from_i2c(i2c::ErrorKind::NoAcknowledge(
            i2c::NoAcknowledgeSource::Address,
        ));
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = i2c_nack
        {
            assert_eq!(component, 3); // I2C component
            assert_eq!(error_code, 0x6303); // 0x6300 | 0x03
        } else {
            panic!("Expected HardwareError variant");
        }
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
        let known_error = DbError::from_spi_error(0x42);
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

        let spi_error = DbError::from_spi_error(0x00);
        assert_eq!(spi_error.error_category(), 0x6000); // Hardware category
        assert_eq!(spi_error.error_code(), 0x6001); // All hardware errors use this unified code

        let uart_error = DbError::from_uart_error(0x00);
        assert_eq!(uart_error.error_category(), 0x6000); // Hardware category
        assert_eq!(uart_error.error_code(), 0x6001);

        let i2c_error = DbError::from_i2c_error(0x00);
        assert_eq!(i2c_error.error_category(), 0x6000); // Hardware category
        assert_eq!(i2c_error.error_code(), 0x6001);

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
        let spi_error = EmbassyErrorConverter::from_spi(spi::ErrorKind::Overrun);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = spi_error
        {
            assert_eq!(component, 2); // SPI component
            assert_eq!(error_code, 0x6104); // 0x6100 | 0x04 (Overrun) - component-specific code
        } else {
            panic!("Expected HardwareError variant");
        }
        // But the unified error code is always 0x6001
        assert_eq!(spi_error.error_code(), 0x6001);

        let i2c_error = EmbassyErrorConverter::from_i2c(i2c::ErrorKind::Bus);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = i2c_error
        {
            assert_eq!(component, 3); // I2C component
            assert_eq!(error_code, 0x6301); // 0x6300 | 0x01 (Bus) - component-specific code
        } else {
            panic!("Expected HardwareError variant");
        }
        // But the unified error code is always 0x6001
        assert_eq!(i2c_error.error_code(), 0x6001);

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

        let spi_error = DbError::from_spi_error(0x01);
        let uart_error = DbError::from_uart_error(0x02);
        let i2c_error = DbError::from_i2c_error(0x03);
        let adc_error = DbError::from_adc_error(0x04);
        let gpio_error = DbError::from_gpio_error(0x05);
        let timer_error = DbError::from_timer_error(0x06);

        // Verify the construction worked correctly (all hardware errors use 0x6001)
        assert_eq!(spi_error.error_code(), 0x6001);
        assert_eq!(uart_error.error_code(), 0x6001);
        assert_eq!(i2c_error.error_code(), 0x6001);
        assert_eq!(adc_error.error_code(), 0x6001);
        assert_eq!(gpio_error.error_code(), 0x6001);
        assert_eq!(timer_error.error_code(), 0x6001);

        // Verify they're all hardware errors
        assert!(spi_error.is_hardware_error());
        assert!(uart_error.is_hardware_error());
        assert!(i2c_error.is_hardware_error());
        assert!(adc_error.is_hardware_error());
        assert!(gpio_error.is_hardware_error());
        assert!(timer_error.is_hardware_error());
    }
}
