use core::future::Future;

use crate::{ExecutorResult, Spawn};

/// Runtime capability for creating join fan-in queues.
///
/// Implemented by each runtime adapter. Queue capacity is an internal
/// constant chosen per adapter (Tokio: 64, Embassy: 8, WASM: 64).
pub trait JoinFanInRuntime: Spawn {
    type JoinQueue<T: Send + 'static>: JoinQueue<T>;

    fn create_join_queue<T: Send + 'static>(&self) -> ExecutorResult<Self::JoinQueue<T>>;
}

/// A bounded fan-in queue that can be split into a sender/receiver pair.
pub trait JoinQueue<T: Send + 'static> {
    type Sender: JoinSender<T> + Clone + Send + 'static;
    type Receiver: JoinReceiver<T> + Send + 'static;

    fn split(self) -> (Self::Sender, Self::Receiver);
}

/// The sending half of a join fan-in queue.
///
/// `send` may await when the queue is full (bounded backpressure).
/// Returns `Err(QueueClosed)` if the receiver has been dropped.
pub trait JoinSender<T: Send + 'static> {
    fn send(&self, item: T) -> impl Future<Output = ExecutorResult<()>> + Send + '_;
}

/// The receiving half of a join fan-in queue.
///
/// Returns `Err(QueueClosed)` when all senders have been dropped.
pub trait JoinReceiver<T: Send + 'static> {
    fn recv(&mut self) -> impl Future<Output = ExecutorResult<T>> + Send + '_;
}
