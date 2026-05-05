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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_executor::{ExecutorError, JoinQueue as _, JoinReceiver as _};
    use wasm_bindgen_test::wasm_bindgen_test;

    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    fn make_queue() -> (WasmJoinSender<u32>, WasmJoinReceiver<u32>) {
        let (tx, rx) = mpsc::channel::<u32>(CAPACITY);
        WasmJoinQueue { tx, rx }.split()
    }

    #[wasm_bindgen_test]
    async fn roundtrip_send_recv() {
        let (tx, mut rx) = make_queue();
        tx.send(1).await.unwrap();
        tx.send(2).await.unwrap();
        tx.send(3).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 1);
        assert_eq!(rx.recv().await.unwrap(), 2);
        assert_eq!(rx.recv().await.unwrap(), 3);
    }

    #[wasm_bindgen_test]
    async fn bounded_capacity() {
        let (tx, _rx) = make_queue();
        // Fill to capacity
        for i in 0..CAPACITY as u32 {
            tx.send(i).await.unwrap();
        }
        // The (CAPACITY+1)th send should return Pending or close — poll once
        let mut tx_clone = tx.tx.clone();
        use futures_util::sink::SinkExt as _;
        // With a full bounded channel the send future should not resolve immediately;
        // we verify by checking that a non-blocking try_send fails.
        let result = tx_clone.try_send(99u32);
        assert!(
            result.is_err(),
            "expected try_send to fail when queue is full"
        );
    }

    #[wasm_bindgen_test]
    async fn send_after_rx_drop_yields_queue_closed() {
        let (tx, rx) = make_queue();
        drop(rx);
        let err = tx.send(1).await.unwrap_err();
        assert!(matches!(err, ExecutorError::QueueClosed));
    }

    #[wasm_bindgen_test]
    async fn recv_after_all_senders_drop_yields_queue_closed() {
        let (tx, mut rx) = make_queue();
        let tx2 = tx.clone();
        drop(tx);
        drop(tx2);
        let err = rx.recv().await.unwrap_err();
        assert!(matches!(err, ExecutorError::QueueClosed));
    }

    #[wasm_bindgen_test]
    async fn clone_sender_routes_to_same_receiver() {
        let (tx, mut rx) = make_queue();
        let tx2 = tx.clone();
        tx.send(10).await.unwrap();
        tx2.send(20).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 10);
        assert_eq!(rx.recv().await.unwrap(), 20);
    }
}
