#[cfg(feature = "observability")]
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::fmt::Debug;
use core::marker::PhantomData;

use crate::buffer::DynBuffer;

// ============================================================================
// Consumer - Type-safe value consumption
// ============================================================================

/// Type-safe consumer for a specific record type
///
/// `Consumer<T>` provides scoped access to subscribe to values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Type Parameters
/// * `T` - The record type this consumer can subscribe to
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it consumes
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
pub struct Consumer<T> {
    /// Pre-resolved buffer handle to the record's buffer.
    buffer: Arc<dyn DynBuffer<crate::Stamped<T>>>,
    /// Stage profiling state (set by the spawn machinery for `.tap()` / `.link_to()`).
    #[cfg(feature = "observability")]
    profiling: Option<(Arc<crate::profiling::StageMetrics>, crate::profiling::Clock)>,
    // See Producer<T>: `fn() -> T` keeps Send/Sync independent of T.
    _phantom: PhantomData<fn() -> T>,
}

impl<T> Consumer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Create a new consumer bound to a pre-resolved buffer handle.
    pub(crate) fn new(buffer: Arc<dyn DynBuffer<crate::Stamped<T>>>) -> Self {
        Self {
            buffer,
            #[cfg(feature = "observability")]
            profiling: None,
            _phantom: PhantomData,
        }
    }

    /// Attaches stage profiling state. Internal — called by the spawn machinery.
    #[cfg(feature = "observability")]
    pub(crate) fn set_profiling(
        &mut self,
        metrics: Arc<crate::profiling::StageMetrics>,
        clock: crate::profiling::Clock,
    ) {
        self.profiling = Some((metrics, clock));
    }

    /// Subscribe to updates for this record type.
    ///
    /// Returns a [`Reader<T>`](crate::buffer::Reader) that yields values as they
    /// are produced. Its `recv().await` is allocation-free.
    /// Infallible — the buffer is pre-resolved at `Consumer` construction.
    pub fn subscribe(&self) -> crate::buffer::Reader<T> {
        let reader = self.buffer.subscribe_boxed();
        #[cfg(feature = "observability")]
        if let Some((metrics, clock)) = &self.profiling {
            return crate::buffer::Reader::new(Box::new(
                crate::profiling::ProfilingBufferReader::new(
                    reader,
                    metrics.clone(),
                    clock.clone(),
                ),
            ));
        }
        crate::buffer::Reader::new(reader)
    }
}

impl<T> Clone for Consumer<T> {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            #[cfg(feature = "observability")]
            profiling: self.profiling.clone(),
            _phantom: PhantomData,
        }
    }
}
