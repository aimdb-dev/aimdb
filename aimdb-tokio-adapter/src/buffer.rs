//! Tokio buffer implementations for AimDB
//!
//! This module provides Tokio-specific implementations of the buffer traits
//! defined in `aimdb-core`. It uses Tokio's async synchronization primitives:
//!
//! - **SPMC Ring**: `tokio::sync::broadcast` for bounded multi-consumer queues
//! - **SingleLatest**: `tokio::sync::watch` for latest-value semantics
//! - **Mailbox**: `tokio::sync::Mutex` + `tokio::sync::Notify` for single-slot overwrite

use std::sync::{Arc, Mutex as StdMutex};

use aimdb_core::buffer::{BufferBackend, BufferCfg, BufferReader};
use aimdb_core::DbError;
use tokio::sync::{broadcast, watch, Notify};

/// Tokio buffer implementation
pub struct TokioBuffer<T: Clone + Send + Sync + 'static> {
    inner: Arc<TokioBufferInner<T>>,
}

/// Internal buffer variants using Tokio primitives
enum TokioBufferInner<T: Clone + Send + Sync + 'static> {
    Broadcast {
        tx: broadcast::Sender<T>,
    },
    Watch {
        tx: watch::Sender<Option<T>>,
    },
    Notify {
        slot: Arc<StdMutex<Option<T>>>,
        notify: Arc<Notify>,
    },
}

impl<T: Clone + Send + Sync + 'static> BufferBackend<T> for TokioBuffer<T> {
    type Reader = TokioBufferReader<T>;

    fn new(cfg: &BufferCfg) -> Self {
        let inner = match &cfg {
            BufferCfg::SpmcRing { capacity } => {
                let (tx, _) = broadcast::channel(*capacity);
                TokioBufferInner::Broadcast { tx }
            }
            BufferCfg::SingleLatest => {
                let (tx, _rx) = watch::channel(None);
                TokioBufferInner::Watch { tx }
            }
            BufferCfg::Mailbox => TokioBufferInner::Notify {
                slot: Arc::new(StdMutex::new(None)),
                notify: Arc::new(Notify::new()),
            },
        };

        Self {
            inner: Arc::new(inner),
        }
    }

    fn push(&self, value: T) {
        match &*self.inner {
            TokioBufferInner::Broadcast { tx } => {
                let _ = tx.send(value);
            }
            TokioBufferInner::Watch { tx } => {
                let _ = tx.send(Some(value));
            }
            TokioBufferInner::Notify { slot, notify } => {
                *slot.lock().unwrap() = Some(value);
                notify.notify_waiters();
            }
        }
    }

    fn subscribe(&self) -> Self::Reader {
        match &*self.inner {
            TokioBufferInner::Broadcast { tx } => {
                TokioBufferReader::Broadcast { rx: tx.subscribe() }
            }
            TokioBufferInner::Watch { tx } => TokioBufferReader::Watch { rx: tx.subscribe() },
            TokioBufferInner::Notify { slot, notify } => TokioBufferReader::Notify {
                slot: Arc::clone(slot),
                notify: Arc::clone(notify),
            },
        }
    }
}

/// Tokio-based buffer reader
pub enum TokioBufferReader<T: Clone + Send + Sync + 'static> {
    Broadcast {
        rx: broadcast::Receiver<T>,
    },
    Watch {
        rx: watch::Receiver<Option<T>>,
    },
    Notify {
        slot: Arc<StdMutex<Option<T>>>,
        notify: Arc<Notify>,
    },
}

impl<T: Clone + Send + Sync + 'static> BufferReader<T> for TokioBufferReader<T> {
    async fn recv(&mut self) -> Result<T, DbError> {
        match self {
            TokioBufferReader::Broadcast { rx } => match rx.recv().await {
                Ok(value) => Ok(value),
                Err(broadcast::error::RecvError::Lagged(n)) => Err(DbError::BufferLagged {
                    lag_count: n,
                    buffer_name: "broadcast".to_string(),
                }),
                Err(broadcast::error::RecvError::Closed) => Err(DbError::BufferClosed {
                    buffer_name: "broadcast".to_string(),
                }),
            },
            TokioBufferReader::Watch { rx } => {
                rx.changed().await.map_err(|_| DbError::BufferClosed {
                    buffer_name: "watch".to_string(),
                })?;

                let value = rx.borrow().clone();
                match value {
                    Some(v) => Ok(v),
                    None => Err(DbError::BufferClosed {
                        buffer_name: "watch".to_string(),
                    }),
                }
            }
            TokioBufferReader::Notify { slot, notify } => {
                loop {
                    // Check if there's already a value
                    {
                        let mut guard = slot.lock().unwrap();
                        if let Some(value) = guard.take() {
                            return Ok(value);
                        }
                    }
                    // No value, wait for notification
                    notify.notified().await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spmc_ring_basic() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_spmc_ring_multiple_consumers() {
        let cfg = BufferCfg::SpmcRing { capacity: 10 };
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader1 = buffer.subscribe();
        let mut reader2 = buffer.subscribe();
        buffer.push(1);
        buffer.push(2);
        assert_eq!(reader1.recv().await.unwrap(), 1);
        assert_eq!(reader2.recv().await.unwrap(), 1);
        assert_eq!(reader1.recv().await.unwrap(), 2);
        assert_eq!(reader2.recv().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_single_latest_basic() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_single_latest_skip_intermediate() {
        let cfg = BufferCfg::SingleLatest;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);
        assert_eq!(reader.recv().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_mailbox_basic() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(42);
        assert_eq!(reader.recv().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_mailbox_overwrite() {
        let cfg = BufferCfg::Mailbox;
        let buffer = TokioBuffer::<i32>::new(&cfg);
        let mut reader = buffer.subscribe();
        buffer.push(1);
        buffer.push(2);
        assert_eq!(reader.recv().await.unwrap(), 2);
    }
}
