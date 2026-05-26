//! `RecordWriter<T>` — the sole implementor of `WriteHandle<T>` (design 029).
//!
//! Pre-binds the three Arcs a `TypedRecord<T, R>` already owns (buffer,
//! latest-snapshot, metadata tracker) so `Producer<T>` can push values without
//! holding a `Arc<AimDb<R>>` or running a `HashMap` lookup per call.

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::sync::Arc;

#[cfg(feature = "std")]
use std::sync::Arc;

use super::traits::{DynBuffer, WriteHandle};

pub(crate) struct RecordWriter<T: Clone + Send + 'static> {
    /// `None` for records that only support `latest()` (no buffer configured).
    buffer: Option<Arc<dyn DynBuffer<T>>>,

    /// Snapshot slot shared with `TypedRecord` and any `latest()` reader.
    #[cfg(feature = "std")]
    latest_snapshot: Arc<std::sync::Mutex<Option<T>>>,
    #[cfg(not(feature = "std"))]
    latest_snapshot: Arc<spin::Mutex<Option<T>>>,

    /// Metadata tracker (already `Clone` with shared inner `Arc<Mutex>` /
    /// `Arc<AtomicBool>`). std-only.
    #[cfg(feature = "std")]
    metadata: crate::typed_record::RecordMetadataTracker,
}

impl<T: Clone + Send + 'static> RecordWriter<T> {
    #[cfg(feature = "std")]
    pub(crate) fn new(
        buffer: Option<Arc<dyn DynBuffer<T>>>,
        latest_snapshot: Arc<std::sync::Mutex<Option<T>>>,
        metadata: crate::typed_record::RecordMetadataTracker,
    ) -> Self {
        Self {
            buffer,
            latest_snapshot,
            metadata,
        }
    }

    #[cfg(not(feature = "std"))]
    pub(crate) fn new(
        buffer: Option<Arc<dyn DynBuffer<T>>>,
        latest_snapshot: Arc<spin::Mutex<Option<T>>>,
    ) -> Self {
        Self {
            buffer,
            latest_snapshot,
        }
    }
}

impl<T: Clone + Send + 'static> WriteHandle<T> for RecordWriter<T> {
    fn push(&self, value: T) {
        #[cfg(feature = "std")]
        {
            *self.latest_snapshot.lock().unwrap() = Some(value.clone());
        }
        #[cfg(not(feature = "std"))]
        {
            *self.latest_snapshot.lock() = Some(value.clone());
        }

        if let Some(buf) = &self.buffer {
            buf.push(value);
            #[cfg(feature = "std")]
            self.metadata.mark_updated();
        }
    }
}
