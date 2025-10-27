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
pub mod connector;
pub mod error;
pub mod runtime;
pub mod time;

pub use buffer::TokioBuffer;
pub use error::TokioErrorSupport;

#[cfg(feature = "tokio-runtime")]
pub use runtime::TokioAdapter;

/// Type alias for Tokio database
///
/// This provides a convenient type for working with databases on the Tokio runtime.
/// Most users should use `AimDbBuilder` directly to create databases.
#[cfg(feature = "tokio-runtime")]
pub type TokioDatabase = aimdb_core::Database<TokioAdapter>;

/// Extension trait for convenient buffer configuration with Tokio
///
/// This trait provides convenience methods for configuring buffers
/// inline when using the Tokio runtime adapter.
pub trait TokioRecordRegistrarExt<'a, T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Configures a buffer for this record using inline configuration
    ///
    /// This is a convenience method that creates a `TokioBuffer` and configures
    /// it in one step, avoiding the need to manually instantiate the buffer.
    ///
    /// # Arguments
    /// * `cfg` - Buffer configuration specifying capacity and behavior
    ///
    /// # Returns
    /// `&mut Self` for method chaining
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aimdb_tokio_adapter::TokioRecordRegistrarExt;
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// builder.configure::<Temperature>(|reg| {
    ///     reg.with_buffer(BufferCfg::SpmcRing { capacity: 10 })
    ///        .source(|producer, ctx_any| { ... })
    ///        .tap(|consumer| { ... });
    /// });
    /// ```
    fn with_buffer(
        &'a mut self,
        cfg: aimdb_core::buffer::BufferCfg,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter>;
}

#[cfg(feature = "tokio-runtime")]
impl<'a, T> TokioRecordRegistrarExt<'a, T> for aimdb_core::RecordRegistrar<'a, T, TokioAdapter>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn with_buffer(
        &'a mut self,
        cfg: aimdb_core::buffer::BufferCfg,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter> {
        use aimdb_core::buffer::Buffer;
        let buffer = Box::new(TokioBuffer::<T>::new(&cfg));
        self.buffer(buffer)
    }
}
