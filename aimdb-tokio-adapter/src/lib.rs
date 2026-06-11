//! Tokio Adapter for AimDB
//!
//! This crate provides Tokio-specific extensions for AimDB, enabling the database
//! to run on standard library environments using the Tokio async runtime.
//!
//! # Features
//!
//! - **Tokio Integration**: Seamless integration with Tokio async executor
//! - **Time Support**: Timestamp, sleep, and delayed task capabilities with `tokio::time`
//! - **Error Handling**: Tokio-specific error conversions and handling
//! - **Std Compatible**: Designed for environments with full standard library
//!
//! # Architecture
//!
//! Tokio is a std async runtime, so this adapter is designed for standard
//! environments and works with the std version of aimdb-core by default.
//!
//! The adapter extends AimDB's core functionality without requiring tokio
//! dependencies in the core crate. It provides:
//!
//!
//! - **Runtime Module**: Core async task spawning with `RuntimeAdapter`
//! - **Time Module**: Time-related capabilities like timestamps and sleep
//! - **Error Module**: Runtime error constructors and conversions
//! - Rich error descriptions leveraging std formatting capabilities
//!
//! See the repository examples for complete usage patterns.

// Tokio adapter always requires std
#[cfg(not(feature = "std"))]
compile_error!("tokio-adapter requires the std feature");

pub mod buffer;
pub mod error;
pub mod runtime;
pub mod time;

pub use buffer::TokioBuffer;
pub use error::TokioErrorSupport;

#[cfg(feature = "tokio-runtime")]
pub use runtime::TokioAdapter;

/// Buffer-construction extension for [`aimdb_core::RecordRegistrar`].
///
/// Buffer construction is the one genuinely adapter-specific registration
/// step left after issue #131 — `source()` / `tap()` / `transform()` are
/// inherent methods on the registrar. This trait adds `.buffer(cfg)` backed
/// by [`TokioBuffer`].
pub trait TokioRecordRegistrarExt<T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Configures a [`TokioBuffer`] from the given configuration.
    fn buffer(&mut self, cfg: aimdb_core::buffer::BufferCfg) -> &mut Self;
}

impl<T> TokioRecordRegistrarExt<T> for aimdb_core::RecordRegistrar<'_, T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn buffer(&mut self, cfg: aimdb_core::buffer::BufferCfg) -> &mut Self {
        use aimdb_core::buffer::Buffer;
        let buffer = Box::new(TokioBuffer::<T>::new(&cfg));
        // Record the cfg so buffer_info() reports the real buffer
        // type/capacity for the dependency graph.
        self.buffer_with_cfg(buffer, cfg)
    }
}
