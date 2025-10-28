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

/// Trait that provides Embassy-specific error conversions for DbError
///
/// This trait provides essential embedded error handling without requiring
/// the core AimDB crate to depend on embassy or embedded-hal.
///
/// In production embedded code, hardware errors should be constructed directly
/// using `DbError::HardwareError` with appropriate component IDs and error codes.
pub trait EmbassyErrorSupport {
    /// Converts a non-blocking error to DbError
    ///
    /// This is the production-critical method for handling embedded-hal's non-blocking
    /// pattern, which is commonly used in embedded async code for hardware interfaces.
    ///
    /// # Returns
    /// - `nb::Error::WouldBlock` → `DbError::ResourceUnavailable { resource_type: 1, _resource_name: () }`
    /// - `nb::Error::Other(e)` → The underlying error `e` converted to DbError
    ///
    /// # Example
    /// ```
    /// use aimdb_core::DbError;
    /// use aimdb_embassy_adapter::EmbassyErrorSupport;
    /// use embedded_hal_nb::nb;
    /// let would_block: nb::Error<DbError> = nb::Error::WouldBlock;
    /// let db_error = DbError::from_nb_error(would_block); // Returns ResourceUnavailable
    /// ```
    fn from_nb_error<E>(error: nb::Error<E>) -> Self
    where
        E: Into<Self>,
        Self: Sized;
}

impl EmbassyErrorSupport for DbError {
    /// Converts a non-blocking error to DbError.
    ///
    /// This handles the embedded-hal non-blocking pattern which is commonly
    /// used in embedded async code for hardware interfaces.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nb_error_would_block_conversion() {
        // Test nb::Error::WouldBlock conversion
        let would_block: nb::Error<DbError> = nb::Error::WouldBlock;
        let db_error = DbError::from_nb_error(would_block);

        if let DbError::ResourceUnavailable { resource_type, .. } = db_error {
            assert_eq!(resource_type, DbError::RESOURCE_TYPE_WOULD_BLOCK);
        } else {
            panic!("Expected ResourceUnavailable variant");
        }
    }

    #[test]
    fn test_nb_error_other_conversion() {
        // Test nb::Error::Other conversion with a hardware error
        let hardware_error = DbError::HardwareError {
            component: 4,       // UART component
            error_code: 0x6242, // UART error code
            _description: (),
        };
        let nb_other_error: nb::Error<DbError> = nb::Error::Other(hardware_error);
        let converted_error = DbError::from_nb_error(nb_other_error);

        // The converted error should be a hardware error
        assert_eq!(converted_error.error_code(), 0x6001); // Unified hardware error code
        assert_eq!(converted_error.error_category(), 0x6000); // Hardware category
    }

    #[test]
    fn test_nb_error_practical_usage() {
        // Practical example: handling WouldBlock from a hardware interface
        let would_block: nb::Error<DbError> = nb::Error::WouldBlock;
        let resource_error = DbError::from_nb_error(would_block);

        match resource_error {
            DbError::ResourceUnavailable { resource_type, .. } => {
                assert_eq!(resource_type, DbError::RESOURCE_TYPE_WOULD_BLOCK);
                // In practice: retry operation, yield to scheduler, or queue for later
            }
            _ => panic!("Expected ResourceUnavailable for WouldBlock"),
        }

        // Practical example: unwrapping a hardware error from nb::Error::Other
        let underlying_error = DbError::HardwareError {
            component: 4,       // UART component
            error_code: 0x6220, // UART error code
            _description: (),
        };
        let nb_wrapped: nb::Error<DbError> = nb::Error::Other(underlying_error);
        let unwrapped_error = DbError::from_nb_error(nb_wrapped);

        // The unwrapped error should retain its hardware error properties
        assert!(unwrapped_error.is_hardware_error());
        assert_eq!(unwrapped_error.error_category(), 0x6000);
    }
}
