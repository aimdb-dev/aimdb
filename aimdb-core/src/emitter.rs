//! Cross-record communication via Emitter pattern
//!
//! The Emitter provides a way for records to emit data to other record types,
//! enabling reactive data flow pipelines across the database.

use core::any::TypeId;
use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc};

#[cfg(feature = "std")]
use std::{boxed::Box, sync::Arc};

use crate::DbResult;

// Forward declare AimDbInner (will be defined in builder.rs)
pub use crate::builder::AimDbInner;

/// Type alias for boxed future returning unit
#[allow(dead_code)]
type BoxFutureUnit = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Emitter for cross-record communication
///
/// The Emitter allows records to emit data to other record types,
/// creating reactive data flow pipelines. It is generic over the runtime type.
///
/// # Design Philosophy
///
/// - **Runtime Generic**: Works with any runtime implementing `Runtime`
/// - **Type Safety**: Emit operations are type-checked at compile time
/// - **Cross-Record Flow**: Records can emit to any other registered record type
/// - **Clone-able**: Can be cloned cheaply (Arc-based) for passing to async tasks
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::experimental::Emitter;
///
/// async fn process_sensor<R: Runtime>(emitter: Emitter<R>, data: SensorData) {
///     // Do processing
///     let processed = process(data);
///     
///     // Emit to another record type if threshold exceeded
///     if processed.value > 100.0 {
///         emitter.emit(Alert::new("High value detected")).await?;
///     }
/// }
/// ```
#[derive(Clone)]
pub struct Emitter {
    /// Runtime adapter (type-erased for storage)
    #[cfg(feature = "std")]
    pub(crate) runtime: Arc<dyn core::any::Any + Send + Sync>,

    #[cfg(not(feature = "std"))]
    pub(crate) runtime: Arc<dyn core::any::Any + Send + Sync>,

    /// Database internal state (record registry)
    pub(crate) inner: Arc<AimDbInner>,
}

impl Emitter {
    /// Creates a new emitter from a concrete runtime
    ///
    /// # Arguments
    /// * `runtime` - The runtime adapter
    /// * `inner` - The database internal state
    ///
    /// # Returns
    /// A new `Emitter` instance
    pub fn new<R>(runtime: Arc<R>, inner: Arc<AimDbInner>) -> Self
    where
        R: 'static + Send + Sync,
    {
        Self {
            runtime: runtime as Arc<dyn core::any::Any + Send + Sync>,
            inner,
        }
    }

    /// Gets a reference to the runtime (downcasted)
    ///
    /// # Type Parameters
    /// * `R` - The expected runtime type
    ///
    /// # Returns
    /// `Some(&R)` if the runtime type matches, `None` otherwise
    pub fn runtime<R: 'static>(&self) -> Option<&R> {
        self.runtime.downcast_ref::<R>()
    }

    /// Emits a value to another record type
    ///
    /// This is the core cross-record communication primitive. When called,
    /// it will invoke the producer and all consumers registered for the
    /// target record type.
    ///
    /// # Type Parameters
    /// * `U` - The target record type to emit to
    ///
    /// # Arguments
    /// * `value` - The value to emit
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err` if the record type is not registered
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // In a producer or consumer function
    /// async fn process(emitter: Emitter, data: SensorData) {
    ///     if data.temp > 100.0 {
    ///         // Emit to Alert record type
    ///         emitter.emit(Alert::new("Temperature too high")).await?;
    ///     }
    /// }
    /// ```
    pub async fn emit<U>(&self, value: U) -> DbResult<()>
    where
        U: Send + 'static + Debug + Clone,
    {
        use crate::typed_record::AnyRecordExt;
        use crate::DbError;

        // Look up the record by TypeId
        let rec = self.inner.records.get(&TypeId::of::<U>()).ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: format!(
                        "No record registered for type: {:?}",
                        core::any::type_name::<U>()
                    ),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RuntimeError { _message: () }
            }
        })?;

        // Downcast to typed record
        let rec_typed = rec.as_typed::<U>().ok_or({
            #[cfg(feature = "std")]
            {
                DbError::RuntimeError {
                    message: "Type mismatch in record registry".into(),
                }
            }
            #[cfg(not(feature = "std"))]
            {
                DbError::RuntimeError { _message: () }
            }
        })?;

        // Produce the value (calls producer and consumers)
        rec_typed.produce(self.clone(), value).await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "std"))]
    use alloc::collections::BTreeMap;
    #[cfg(feature = "std")]
    use std::collections::HashMap;

    #[allow(dead_code)]
    #[derive(Debug, Clone, PartialEq)]
    struct TestData {
        value: i32,
    }

    #[test]
    fn test_emitter_creation() {
        #[cfg(feature = "std")]
        let records = HashMap::new();
        #[cfg(not(feature = "std"))]
        let records = BTreeMap::new();

        let inner = Arc::new(AimDbInner { records });

        // Create with a dummy runtime
        let runtime = Arc::new(());
        let _emitter = Emitter::new(runtime, inner);
    }

    #[test]
    fn test_emitter_clone() {
        #[cfg(feature = "std")]
        let records = HashMap::new();
        #[cfg(not(feature = "std"))]
        let records = BTreeMap::new();

        let inner = Arc::new(AimDbInner { records });
        let runtime = Arc::new(());
        let emitter = Emitter::new(runtime, inner);

        let _emitter2 = emitter.clone();
    }
}
