//! `RecordWriter<T>` — the sole implementor of `WriteHandle<T>`.
//!
//! Pre-binds the buffer so `Producer<T>` can push values without holding an
//! `Arc<AimDb<R>>` or running a `HashMap` lookup per call.

use alloc::sync::Arc;

use super::traits::{DynBuffer, WriteHandle};
use crate::clock::InstanceClock;
use crate::stamped::{Stamped, WriteMetadata};

pub(crate) struct RecordWriter<T: Clone + Send + 'static> {
    /// `None` for records without a configured buffer.
    buffer: Option<Arc<dyn DynBuffer<Stamped<T>>>>,
    /// The instance clock writes are stamped from.
    clock: Arc<dyn InstanceClock>,
}

impl<T: Clone + Send + 'static> RecordWriter<T> {
    pub(crate) fn new(
        buffer: Option<Arc<dyn DynBuffer<Stamped<T>>>>,
        clock: Arc<dyn InstanceClock>,
    ) -> Self {
        Self { buffer, clock }
    }
}

impl<T: Clone + Send + 'static> WriteHandle<T> for RecordWriter<T> {
    /// The one place a value is stamped (design 058 §4.3): the clock is read
    /// immediately before the value enters the buffer.
    fn push(&self, value: T) {
        if let Some(buf) = &self.buffer {
            let stamp = self.clock.now();
            buf.push(Stamped::new(value, WriteMetadata::new(stamp)));
        }
    }
}
