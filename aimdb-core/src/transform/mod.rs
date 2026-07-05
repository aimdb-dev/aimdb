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

use core::fmt::Debug;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use crate::typed_record::BoxFuture;

pub mod join;
pub mod single;

// Public re-exports
pub use single::{StatefulTransformBuilder, TransformBuilder, TransformPipeline};

#[cfg(feature = "observability")]
use crate::{profiling::Clock, StageMetrics};
pub use join::{JoinBuilder, JoinEventRx, JoinPipeline, JoinTrigger};

// ============================================================================
// TransformDescriptor — stored per output record in TypedRecord
// ============================================================================

/// Futures contributed by a transform at build-time collection.
///
/// `task_future` is the transform's main event loop; `fanin_futures` is any
/// per-input forwarder needed for multi-input joins (empty for single-input).
pub(crate) struct CollectedTransform {
    pub task_future: BoxFuture<'static, ()>,
    pub fanin_futures: Vec<BoxFuture<'static, ()>>,
}

#[cfg(feature = "observability")]
pub(crate) type TransformProfiling = Option<(Arc<StageMetrics>, Clock)>;

#[cfg(not(feature = "observability"))]
pub(crate) type TransformProfiling = ();

pub(crate) struct TransformDescriptor<T>
where
    T: Send + 'static + Debug + Clone,
{
    pub input_keys: Vec<String>,

    /// Build the transform's futures.
    ///
    /// Receives:
    /// - `Producer<T>` — the pre-resolved write handle for the output record.
    /// - `Arc<AimDb>` — used by input forwarders to resolve their input
    ///   consumers at startup time.
    /// - `&str` — the output record's key, threaded through so the transform
    ///   task can include it in tracing messages even though `Producer<T>` no
    ///   longer carries a `.key()` accessor. Important when
    ///   multiple records share type `T` under different keys.
    #[allow(clippy::type_complexity)]
    pub build_fn: Box<
        dyn FnOnce(
                crate::Producer<T>,
                Arc<crate::AimDb>,
                &str,
                TransformProfiling,
            ) -> CollectedTransform
            + Send,
    >,
}
