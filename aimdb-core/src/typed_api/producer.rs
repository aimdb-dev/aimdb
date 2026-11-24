use alloc::sync::Arc;
use core::fmt::Debug;
use core::marker::PhantomData;

use crate::buffer::{TryProduceError, WriteHandle};

// ============================================================================
// Producer - Type-safe value production
// ============================================================================

/// Type-safe producer for a specific record type
///
/// `Producer<T>` provides scoped access to produce values of type `T` only.
/// This follows the principle of least privilege - services only get access
/// to what they need, not the entire database.
///
/// # Type Parameters
/// * `T` - The record type this producer can emit
///
/// Pre-binds the record's buffer, latest-snapshot slot, and metadata tracker
/// (via an internal `Arc<dyn WriteHandle<T>>`), so `produce()` is a single
/// virtual call rather than a `HashMap` lookup + downcast on each invocation
///.
///
/// # Benefits
///
/// - **Type Safety**: Compile-time guarantee of correct type
/// - **Testability**: Easy to mock for testing
/// - **Clear Intent**: Function signature shows what it produces
/// - **Decoupling**: No access to other record types
/// - **Security**: Cannot misuse database for unintended operations
pub struct Producer<T> {
    /// Pre-resolved write handle to the record's buffer/snapshot/metadata.
    write: Arc<dyn WriteHandle<T>>,
    /// Stage profiling state (set by the spawn machinery for `.source()` stages).
    #[cfg(feature = "observability")]
    profiling: Option<Arc<crate::profiling::ProducerProfilingState>>,
    // `fn() -> T` carries T without forcing Producer's Send/Sync to depend on T.
    // T is only a type-system marker here — it is never stored or referenced.
    _phantom: PhantomData<fn() -> T>,
}

impl<T> Producer<T>
where
    T: Send + 'static + Debug + Clone,
{
    /// Create a new producer bound to a pre-resolved write handle.
    pub(crate) fn new(write: Arc<dyn WriteHandle<T>>) -> Self {
        Self {
            write,
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
        self.profiling = Some(Arc::new(crate::profiling::ProducerProfilingState::new(
            metrics, clock,
        )));
    }

    /// Push a value. Infallible — overwrite-on-overflow buffers cannot reject.
    /// Use this for fire-and-forget telemetry.
    ///
    /// Forwards the value to the record's buffer, the latest-snapshot slot,
    /// and the metadata tracker in a single synchronous call.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aimdb_core::{Producer, RuntimeContext};
    /// # #[derive(Clone, Debug)] struct Telemetry { celsius: f32 }
    /// # async fn read_sensor() -> Telemetry { Telemetry { celsius: 21.0 } }
    /// async fn sensor_loop(ctx: RuntimeContext, producer: Producer<Telemetry>) {
    ///     loop {
    ///         producer.produce(read_sensor().await);
    ///         ctx.time().sleep_secs(1).await;
    ///     }
    /// }
    /// ```
    pub fn produce(&self, value: T) {
        #[cfg(feature = "observability")]
        if let Some(state) = &self.profiling {
            state.record_produce();
        }
        self.write.push(value);
    }

    /// Lets a write handle report that it rejected a value.
    ///
    /// All current buffers (`SpmcRing`, `SingleLatest`, `Mailbox`) overwrite
    /// existing values when full and return `Ok(())`. Use
    /// [`produce`](Self::produce) for them. A future buffer that refuses a
    /// write when full can return the value through [`TryProduceError::Full`].
    /// A buffer that detects closure can return it through
    /// [`TryProduceError::Closed`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aimdb_core::{Producer, TryProduceError};
    /// # #[derive(Clone, Debug)] struct Command { id: u32 }
    /// fn send_command(producer: &Producer<Command>, cmd: Command) {
    ///     match producer.try_produce(cmd) {
    ///         Ok(()) => {}
    ///         Err(TryProduceError::Full(_)) => { /* backpressure: retry later */ }
    ///         Err(TryProduceError::Closed(_)) => { /* record shut down */ }
    ///     }
    /// }
    /// ```
    pub fn try_produce(&self, value: T) -> Result<(), TryProduceError<T>> {
        self.write.try_push(value)
    }
}

impl<T> Clone for Producer<T> {
    fn clone(&self) -> Self {
        Self {
            write: self.write.clone(),
            #[cfg(feature = "observability")]
            profiling: self.profiling.clone(),
            _phantom: PhantomData,
        }
    }
}
