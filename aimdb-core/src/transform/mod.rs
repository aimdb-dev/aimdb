//! Reactive transform primitives for derived records.
//!
//! # Transform Archetypes
//!
//! - **Map** (1:1, stateless): Transform each input value to zero-or-one output value
//! - **Accumulate** (N:1, stateful): Aggregate a stream of values with persistent state
//! - **Join** (M×N:1, stateful, multi-input): Combine values from multiple input records
//!
//! All three are handled by a unified API surface:
//! - Single-input: `.transform()` with `TransformBuilder`
//! - Multi-input: `.transform_join()` with `JoinBuilder`

use core::any::Any;
use core::fmt::Debug;

extern crate alloc;
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use crate::typed_record::BoxFuture;

pub mod join;
pub mod single;

// Public re-exports
pub use single::{StatefulTransformBuilder, TransformBuilder, TransformPipeline};

#[cfg(feature = "alloc")]
pub use join::{JoinBuilder, JoinPipeline, JoinStateBuilder, JoinTrigger};

// JoinTrigger is always available (no std dependency)
#[cfg(not(feature = "alloc"))]
pub use join::JoinTrigger;

// ============================================================================
// TransformDescriptor — stored per output record in TypedRecord
// ============================================================================

pub(crate) struct TransformDescriptor<T, R: aimdb_executor::Spawn + 'static>
where
    T: Send + 'static + Debug + Clone,
{
    pub input_keys: Vec<String>,

    #[allow(clippy::type_complexity)]
    pub spawn_fn: Box<
        dyn FnOnce(
                crate::Producer<T, R>,
                Arc<crate::AimDb<R>>,
                Arc<dyn Any + Send + Sync>,
            ) -> BoxFuture<'static, ()>
            + Send
            + Sync,
    >,
}
