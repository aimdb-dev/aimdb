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

/// Extension trait for convenient configuration with Tokio
///
/// This trait provides high-level convenience methods for configuring records
/// with the Tokio runtime, automatically handling buffer creation and runtime
/// context extraction.
pub trait TokioRecordRegistrarExt<'a, T>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Configures a buffer using inline configuration
    fn buffer(
        &'a mut self,
        cfg: aimdb_core::buffer::BufferCfg,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter>;

    /// Registers a producer with automatic runtime context injection
    fn source<F, Fut>(
        &'a mut self,
        f: F,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter>
    where
        F: FnOnce(
                aimdb_core::RuntimeContext<TokioAdapter>,
                aimdb_core::Producer<T, TokioAdapter>,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static;

    /// Registers a consumer with automatic runtime context injection
    fn tap<F, Fut>(&'a mut self, f: F) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter>
    where
        F: FnOnce(
                aimdb_core::RuntimeContext<TokioAdapter>,
                aimdb_core::Consumer<T, TokioAdapter>,
            ) -> Fut
            + Send
            + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static;
}

#[cfg(feature = "tokio-runtime")]
impl<'a, T> TokioRecordRegistrarExt<'a, T> for aimdb_core::RecordRegistrar<'a, T, TokioAdapter>
where
    T: Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn buffer(
        &'a mut self,
        cfg: aimdb_core::buffer::BufferCfg,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter> {
        use aimdb_core::buffer::Buffer;
        let buffer = Box::new(TokioBuffer::<T>::new(&cfg));
        self.buffer_raw(buffer)
    }

    fn source<F, Fut>(
        &'a mut self,
        f: F,
    ) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter>
    where
        F: FnOnce(
                aimdb_core::RuntimeContext<TokioAdapter>,
                aimdb_core::Producer<T, TokioAdapter>,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        self.source_raw(|producer, ctx_any| {
            let runtime = ctx_any
                .downcast::<TokioAdapter>()
                .expect("Expected TokioAdapter runtime");
            let ctx = aimdb_core::RuntimeContext::from_arc(runtime.clone());
            f(ctx, producer)
        })
    }

    fn tap<F, Fut>(&'a mut self, f: F) -> &'a mut aimdb_core::RecordRegistrar<'a, T, TokioAdapter>
    where
        F: FnOnce(
                aimdb_core::RuntimeContext<TokioAdapter>,
                aimdb_core::Consumer<T, TokioAdapter>,
            ) -> Fut
            + Send
            + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        self.tap_raw(|consumer, ctx_any| {
            let runtime = ctx_any
                .downcast::<TokioAdapter>()
                .expect("Expected TokioAdapter runtime");
            let ctx = aimdb_core::RuntimeContext::from_arc(runtime.clone());
            f(ctx, consumer)
        })
    }
}
