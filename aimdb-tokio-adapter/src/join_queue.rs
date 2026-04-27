use aimdb_executor::{
    ExecutorError, ExecutorResult, JoinFanInRuntime, JoinQueue, JoinReceiver, JoinSender,
};

use crate::runtime::TokioAdapter;

const CAPACITY: usize = 64;

// ============================================================================
// TokioJoinQueue
// ============================================================================

pub struct TokioJoinQueue<T: Send + 'static> {
    tx: tokio::sync::mpsc::Sender<T>,
    rx: tokio::sync::mpsc::Receiver<T>,
}

pub struct TokioJoinSender<T: Send + 'static> {
    tx: tokio::sync::mpsc::Sender<T>,
}

pub struct TokioJoinReceiver<T: Send + 'static> {
    rx: tokio::sync::mpsc::Receiver<T>,
}

// Clone is required by JoinQueue::Sender bound
impl<T: Send + 'static> Clone for TokioJoinSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<T: Send + 'static> JoinQueue<T> for TokioJoinQueue<T> {
    type Sender = TokioJoinSender<T>;
    type Receiver = TokioJoinReceiver<T>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        (
            TokioJoinSender { tx: self.tx },
            TokioJoinReceiver { rx: self.rx },
        )
    }
}

impl<T: Send + 'static> JoinSender<T> for TokioJoinSender<T> {
    async fn send(&self, item: T) -> ExecutorResult<()> {
        self.tx
            .send(item)
            .await
            .map_err(|_| ExecutorError::QueueClosed)
    }
}

impl<T: Send + 'static> JoinReceiver<T> for TokioJoinReceiver<T> {
    async fn recv(&mut self) -> ExecutorResult<T> {
        self.rx.recv().await.ok_or(ExecutorError::QueueClosed)
    }
}

// ============================================================================
// JoinFanInRuntime for TokioAdapter
// ============================================================================

impl JoinFanInRuntime for TokioAdapter {
    type JoinQueue<T: Send + 'static> = TokioJoinQueue<T>;

    fn create_join_queue<T: Send + 'static>(&self) -> ExecutorResult<Self::JoinQueue<T>> {
        let (tx, rx) = tokio::sync::mpsc::channel(CAPACITY);
        Ok(TokioJoinQueue { tx, rx })
    }
}
