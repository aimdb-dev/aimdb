//! `RecordWriter<T>` — the sole implementor of `WriteHandle<T>` (design 029).
//!
//! Pre-binds the buffer so `Producer<T>` can push values without holding an
//! `Arc<AimDb<R>>` or running a `HashMap` lookup per call.

use alloc::sync::Arc;

use super::traits::{DynBuffer, WriteHandle};

pub(crate) struct RecordWriter<T: Clone + Send + 'static> {
    /// `None` for records without a configured buffer.
    buffer: Option<Arc<dyn DynBuffer<T>>>,
}

impl<T: Clone + Send + 'static> RecordWriter<T> {
    pub(crate) fn new(buffer: Option<Arc<dyn DynBuffer<T>>>) -> Self {
        Self { buffer }
    }
}

impl<T: Clone + Send + 'static> WriteHandle<T> for RecordWriter<T> {
    fn push(&self, value: T) {
        if let Some(buf) = &self.buffer {
            buf.push(value);
        }
    }
}
