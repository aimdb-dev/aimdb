use aimdb_executor::{
    ExecutorError, ExecutorResult, JoinFanInRuntime, JoinQueue, JoinReceiver, JoinSender,
};
use futures_channel::mpsc;
use futures_util::sink::SinkExt as _;
use futures_util::stream::StreamExt as _;

use crate::runtime::WasmAdapter;

const CAPACITY: usize = 64;

// ============================================================================
// WasmJoinQueue
// ============================================================================

pub struct WasmJoinQueue<T: Send + 'static> {
    tx: mpsc::Sender<T>,
    rx: mpsc::Receiver<T>,
}

pub struct WasmJoinSender<T: Send + 'static> {
    tx: mpsc::Sender<T>,
}

pub struct WasmJoinReceiver<T: Send + 'static> {
    rx: mpsc::Receiver<T>,
}

impl<T: Send + 'static> Clone for WasmJoinSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<T: Send + 'static> JoinQueue<T> for WasmJoinQueue<T> {
    type Sender = WasmJoinSender<T>;
    type Receiver = WasmJoinReceiver<T>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        (
            WasmJoinSender { tx: self.tx },
            WasmJoinReceiver { rx: self.rx },
        )
    }
}

impl<T: Send + 'static> JoinSender<T> for WasmJoinSender<T> {
    async fn send(&self, item: T) -> ExecutorResult<()> {
        // Clone sender to get a mutable handle (futures-channel requires mut for Sink).
        let mut tx = self.tx.clone();
        tx.send(item).await.map_err(|_| ExecutorError::QueueClosed)
    }
}

impl<T: Send + 'static> JoinReceiver<T> for WasmJoinReceiver<T> {
    async fn recv(&mut self) -> ExecutorResult<T> {
        self.rx.next().await.ok_or(ExecutorError::QueueClosed)
    }
}

// ============================================================================
// JoinFanInRuntime for WasmAdapter
// ============================================================================

impl JoinFanInRuntime for WasmAdapter {
    type JoinQueue<T: Send + 'static> = WasmJoinQueue<T>;

    fn create_join_queue<T: Send + 'static>(&self) -> ExecutorResult<Self::JoinQueue<T>> {
        let (tx, rx) = mpsc::channel(CAPACITY);
        Ok(WasmJoinQueue { tx, rx })
    }
}
