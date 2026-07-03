//! Tokio Adapter for AimDB
//!
//! This crate provides Tokio-specific extensions for AimDB, enabling the database
//! to run on standard library environments using the Tokio async runtime.
//!
//! The adapter provides two things on top of aimdb-core:
//!
//! - **Runtime**: [`TokioAdapter`] implements `aimdb_core::RuntimeOps`
//!   (clock, sleep, logging) over `std::time` + `tokio::time`.
//! - **Buffers**: [`TokioBuffer`] implements the three buffer semantics over
//!   tokio primitives, registered via [`TokioRecordRegistrarExt::buffer`].
//!
//! See the repository examples for complete usage patterns.

// Tokio adapter always requires std
#[cfg(not(feature = "std"))]
compile_error!("tokio-adapter requires the std feature");

pub mod buffer;
pub mod runtime;

pub use buffer::TokioBuffer;

#[cfg(feature = "tokio-runtime")]
pub use runtime::TokioAdapter;

/// Buffer-construction extension for [`aimdb_core::RecordRegistrar`].
///
/// Buffer construction is the one genuinely adapter-specific registration
/// step that is genuinely adapter-specific — `source()` / `tap()` / `transform()` are
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
