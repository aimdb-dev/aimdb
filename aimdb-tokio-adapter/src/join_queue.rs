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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_executor::ExecutorError;

    fn make_queue() -> (TokioJoinSender<u32>, TokioJoinReceiver<u32>) {
        let adapter = TokioAdapter::new().expect("TokioAdapter");
        adapter
            .create_join_queue::<u32>()
            .expect("create_join_queue")
            .split()
    }

    #[tokio::test]
    async fn roundtrip_send_recv() {
        let (tx, mut rx) = make_queue();
        tx.send(1).await.unwrap();
        tx.send(2).await.unwrap();
        tx.send(3).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 1);
        assert_eq!(rx.recv().await.unwrap(), 2);
        assert_eq!(rx.recv().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn bounded_capacity() {
        let adapter = TokioAdapter::new().expect("TokioAdapter");
        let (tx, _rx) = adapter.create_join_queue::<u32>().unwrap().split();
        // Fill to capacity without consuming
        for i in 0..CAPACITY as u32 {
            tx.send(i).await.unwrap();
        }
        // The (CAPACITY+1)th send should not complete immediately
        let send_fut = tx.send(99);
        let result = tokio::time::timeout(std::time::Duration::from_millis(10), send_fut).await;
        assert!(result.is_err(), "expected send to block when queue is full");
    }

    #[tokio::test]
    async fn send_after_rx_drop_yields_queue_closed() {
        let (tx, rx) = make_queue();
        drop(rx);
        let err = tx.send(1).await.unwrap_err();
        assert!(matches!(err, ExecutorError::QueueClosed));
    }

    #[tokio::test]
    async fn recv_after_all_senders_drop_yields_queue_closed() {
        let (tx, mut rx) = make_queue();
        let tx2 = tx.clone();
        drop(tx);
        drop(tx2);
        let err = rx.recv().await.unwrap_err();
        assert!(matches!(err, ExecutorError::QueueClosed));
    }

    #[tokio::test]
    async fn clone_sender_routes_to_same_receiver() {
        let (tx, mut rx) = make_queue();
        let tx2 = tx.clone();
        tx.send(10).await.unwrap();
        tx2.send(20).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 10);
        assert_eq!(rx.recv().await.unwrap(), 20);
    }
}
