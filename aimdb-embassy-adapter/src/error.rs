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

// Embassy Error Code Base Values
const SPI_ERROR_BASE: u16 = 0x6100;
const UART_ERROR_BASE: u16 = 0x6200;
const I2C_ERROR_BASE: u16 = 0x6300;
const ADC_ERROR_BASE: u16 = 0x6400;
const GPIO_ERROR_BASE: u16 = 0x6500;
const TIMER_ERROR_BASE: u16 = 0x6600;

// Component IDs for Embassy hardware
const TIMER_COMPONENT_ID: u8 = 0;
const GPIO_COMPONENT_ID: u8 = 1;
const SPI_COMPONENT_ID: u8 = 2;
const I2C_COMPONENT_ID: u8 = 3;
const UART_COMPONENT_ID: u8 = 4;
const ADC_COMPONENT_ID: u8 = 5;

// SPI-specific error codes
const SPI_CHIP_SELECT_FAULT: u8 = 0x01;
const SPI_MODE_FAULT: u8 = 0x02;
const SPI_OVERRUN: u8 = 0x04;
const SPI_FRAME_FORMAT: u8 = 0x05;
const SPI_UNKNOWN_ERROR: u8 = 0xFF;

// I2C-specific error codes
const I2C_BUS_ERROR: u8 = 0x01;
const I2C_ARBITRATION_LOSS: u8 = 0x02;
const I2C_NACK_ADDRESS: u8 = 0x03;
const I2C_NACK_DATA: u8 = 0x04;
const I2C_NACK_UNKNOWN: u8 = 0x05;
const I2C_OVERRUN: u8 = 0x06;
const I2C_UNKNOWN_ERROR: u8 = 0xFF;

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
            component: SPI_COMPONENT_ID,
            error_code: SPI_ERROR_BASE | (code as u16),
            _description: (),
        }
    }

    /// Creates a UART error for Embassy environments (error codes 0x6200-0x62FF)
    fn from_uart_error(code: u8) -> Self {
        DbError::HardwareError {
            component: UART_COMPONENT_ID,
            error_code: UART_ERROR_BASE | (code as u16),
            _description: (),
        }
    }

    /// Creates an I2C error for Embassy environments (error codes 0x6300-0x63FF)
    fn from_i2c_error(code: u8) -> Self {
        DbError::HardwareError {
            component: I2C_COMPONENT_ID,
            error_code: I2C_ERROR_BASE | (code as u16),
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

    /// Converts an SPI error to DbError
    fn from_spi_hal_error(error: spi::ErrorKind) -> Self {
        let error_code = match error {
            spi::ErrorKind::ChipSelectFault => SPI_CHIP_SELECT_FAULT,
            spi::ErrorKind::ModeFault => SPI_MODE_FAULT,
            spi::ErrorKind::Overrun => SPI_OVERRUN,
            spi::ErrorKind::FrameFormat => SPI_FRAME_FORMAT,
            _ => SPI_UNKNOWN_ERROR, // Generic/unknown error
        };
        Self::from_spi_error(error_code)
    }

    /// Converts an I2C error to DbError
    fn from_i2c_hal_error(error: i2c::ErrorKind) -> Self {
        let error_code = match error {
            i2c::ErrorKind::Bus => I2C_BUS_ERROR,
            i2c::ErrorKind::ArbitrationLoss => I2C_ARBITRATION_LOSS,
            i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Address) => I2C_NACK_ADDRESS,
            i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Data) => I2C_NACK_DATA,
            i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Unknown) => I2C_NACK_UNKNOWN,
            i2c::ErrorKind::Overrun => I2C_OVERRUN,
            _ => I2C_UNKNOWN_ERROR, // Generic/unknown error
        };
        Self::from_i2c_error(error_code)
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
            assert_eq!(component, SPI_COMPONENT_ID);
            assert_eq!(error_code, SPI_ERROR_BASE | 0x42);
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
            assert_eq!(component, UART_COMPONENT_ID);
            assert_eq!(error_code, UART_ERROR_BASE | 0x10);
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
            assert_eq!(component, I2C_COMPONENT_ID);
            assert_eq!(error_code, I2C_ERROR_BASE | 0x05);
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
        use crate::EmbassyErrorConverter;

        // Test SPI error conversions
        let spi_overrun = EmbassyErrorConverter::from_spi(spi::ErrorKind::Overrun);
        if let DbError::HardwareError {
            component,
            error_code,
            ..
        } = spi_overrun
        {
            assert_eq!(component, SPI_COMPONENT_ID);
            assert_eq!(error_code, SPI_ERROR_BASE | SPI_OVERRUN as u16);
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
            assert_eq!(component, SPI_COMPONENT_ID);
            assert_eq!(error_code, SPI_ERROR_BASE | SPI_MODE_FAULT as u16);
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
            assert_eq!(component, I2C_COMPONENT_ID);
            assert_eq!(error_code, I2C_ERROR_BASE | I2C_BUS_ERROR as u16);
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
            assert_eq!(component, I2C_COMPONENT_ID);
            assert_eq!(error_code, I2C_ERROR_BASE | I2C_NACK_ADDRESS as u16);
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
            assert_eq!(component, SPI_COMPONENT_ID);
            assert_eq!(error_code, SPI_ERROR_BASE | SPI_OVERRUN as u16); // component-specific code
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
            assert_eq!(component, I2C_COMPONENT_ID);
            assert_eq!(error_code, I2C_ERROR_BASE | I2C_BUS_ERROR as u16); // component-specific code
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
