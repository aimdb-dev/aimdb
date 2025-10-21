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
/// Allows records to emit data to other record types, creating reactive data flow pipelines.
/// Clone-able (Arc-based) for passing to async tasks.
///
/// # Example
///
/// ```rust,ignore
/// async fn process_sensor(emitter: Emitter, data: SensorData) {
///     if data.value > 100.0 {
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
    /// Returns `Some(&R)` if the runtime type matches, `None` otherwise.
    pub fn runtime<R: 'static>(&self) -> Option<&R> {
        self.runtime.downcast_ref::<R>()
    }

    /// Emits a value to another record type
    ///
    /// Invokes the producer and all consumers registered for the target record type.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// async fn process(emitter: Emitter, data: SensorData) {
    ///     if data.temp > 100.0 {
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

    extern crate alloc;
    use alloc::collections::BTreeMap;

    fn create_test_emitter() -> Emitter {
        let records = BTreeMap::new();

        let inner = Arc::new(AimDbInner { records });

        let runtime = Arc::new(());
        Emitter::new(runtime, inner)
    }

    #[test]
    fn test_emitter_creation() {
        let _emitter = create_test_emitter();
    }

    #[test]
    fn test_emitter_clone() {
        let emitter = create_test_emitter();
        let _emitter2 = emitter.clone();
    }
}
