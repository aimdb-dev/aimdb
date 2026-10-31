//! `RecordWriter<T>` — the sole implementor of `WriteHandle<T>`.
//!
//! Pre-binds the buffer so `Producer<T>` can push values without holding an
//! `Arc<AimDb<R>>` or running a `HashMap` lookup per call.

use alloc::sync::Arc;

use super::traits::{DynBuffer, WriteHandle};
use crate::clock::InstanceClock;

pub(crate) struct RecordWriter<T: Clone + Send + 'static> {
    /// `None` for records without a configured buffer.
    buffer: Option<Arc<dyn DynBuffer<T>>>,
    /// The instance clock writes are stamped from.
    #[allow(dead_code)] // read once writes are stamped (design 058 §4.3)
    clock: Arc<dyn InstanceClock>,
}

impl<T: Clone + Send + 'static> RecordWriter<T> {
    pub(crate) fn new(
        buffer: Option<Arc<dyn DynBuffer<T>>>,
        clock: Arc<dyn InstanceClock>,
    ) -> Self {
        Self { buffer, clock }
    }
}

impl<T: Clone + Send + 'static> WriteHandle<T> for RecordWriter<T> {
    fn push(&self, value: T) {
        if let Some(buf) = &self.buffer {
            buf.push(value);
        }
    }
}
