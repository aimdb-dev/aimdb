extern crate alloc;

use aimdb_executor::{ExecutorResult, JoinQueue, JoinReceiver, JoinSender};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

#[cfg(all(feature = "embassy-runtime", feature = "alloc"))]
use crate::runtime::EmbassyAdapter;
#[cfg(all(feature = "embassy-runtime", feature = "alloc"))]
use aimdb_executor::JoinFanInRuntime;

/// Internal queue capacity for Embassy join fan-in.
///
/// Uses a compile-time constant because `embassy_sync::channel::Channel` requires
/// `N` as a const generic. Cannot be overridden per-call — this is the fixed
/// default for all Embassy join queues.
const CAPACITY: usize = 8;

type EmbassyChan<T> = Channel<CriticalSectionRawMutex, T, CAPACITY>;

// ============================================================================
// EmbassyJoinQueue
// ============================================================================

pub struct EmbassyJoinQueue<T: Send + 'static> {
    channel: &'static EmbassyChan<T>,
}

/// Sender half — just a `&'static Channel` reference, cheap to clone.
pub struct EmbassyJoinSender<T: Send + 'static> {
    channel: &'static EmbassyChan<T>,
}

/// Receiver half — also a `&'static Channel` reference.
pub struct EmbassyJoinReceiver<T: Send + 'static> {
    channel: &'static EmbassyChan<T>,
}

impl<T: Send + 'static> Clone for EmbassyJoinSender<T> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel,
        }
    }
}

impl<T: Send + 'static> JoinQueue<T> for EmbassyJoinQueue<T> {
    type Sender = EmbassyJoinSender<T>;
    type Receiver = EmbassyJoinReceiver<T>;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        (
            EmbassyJoinSender {
                channel: self.channel,
            },
            EmbassyJoinReceiver {
                channel: self.channel,
            },
        )
    }
}

impl<T: Send + 'static> JoinSender<T> for EmbassyJoinSender<T> {
    async fn send(&self, item: T) -> ExecutorResult<()> {
        // Blocks when full (bounded backpressure). Embassy channels do not close,
        // so this never returns Err in normal operation.
        self.channel.send(item).await;
        Ok(())
    }
}

impl<T: Send + 'static> JoinReceiver<T> for EmbassyJoinReceiver<T> {
    async fn recv(&mut self) -> ExecutorResult<T> {
        // Embassy channels do not close — this blocks until a message arrives.
        // On embedded targets the join loop runs for the device lifetime.
        Ok(self.channel.receive().await)
    }
}

// ============================================================================
// JoinFanInRuntime for EmbassyAdapter
// ============================================================================

#[cfg(all(feature = "embassy-runtime", feature = "alloc"))]
impl JoinFanInRuntime for EmbassyAdapter {
    type JoinQueue<T: Send + 'static> = EmbassyJoinQueue<T>;

    fn create_join_queue<T: Send + 'static>(&self) -> ExecutorResult<Self::JoinQueue<T>> {
        // Leak the channel to obtain a 'static reference.
        // Called once per join transform at database startup — the leak is intentional
        // and matches the DB lifetime on embedded targets.
        let channel: &'static EmbassyChan<T> =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(Channel::new()));
        Ok(EmbassyJoinQueue { channel })
    }
}

// ============================================================================
// Tests
// ============================================================================

// These tests cover: roundtrip ordering, bounded backpressure, and sender cloning.
// Embassy channels do not close — there are no QueueClosed scenarios to test.
//
// They run on the host: the queue types depend only on `embassy_sync::Channel`
// and `futures::executor::block_on` (the runtime-specific `JoinFanInRuntime`
// impl above carries its own `embassy-runtime` gate), so this module is gated on
// `embassy-sync` rather than `embassy-runtime` and never pulls embassy-executor's
// cortex-m assembly into the host test build. `make test` exercises them via:
//   cargo test -p aimdb-embassy-adapter \
//       --no-default-features --features "alloc,embassy-sync,embassy-time"
// (the `critical-section/std` dev-dep satisfies the `CriticalSectionRawMutex`
// link target; defmt/time-driver stubs live in `buffer.rs`'s test module).
#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    fn make_channel() -> &'static EmbassyChan<u32> {
        extern crate alloc;
        alloc::boxed::Box::leak(alloc::boxed::Box::new(Channel::new()))
    }

    fn make_queue() -> (EmbassyJoinSender<u32>, EmbassyJoinReceiver<u32>) {
        EmbassyJoinQueue {
            channel: make_channel(),
        }
        .split()
    }

    #[test]
    fn roundtrip_send_recv() {
        let (tx, mut rx) = make_queue();
        block_on(async {
            tx.send(1).await.unwrap();
            tx.send(2).await.unwrap();
            tx.send(3).await.unwrap();
            assert_eq!(rx.recv().await.unwrap(), 1);
            assert_eq!(rx.recv().await.unwrap(), 2);
            assert_eq!(rx.recv().await.unwrap(), 3);
        });
    }

    #[test]
    fn bounded_capacity_8() {
        // Fill to CAPACITY without consuming, then assert an extra send is Pending.
        let channel: &'static EmbassyChan<u32> = make_channel();
        block_on(async {
            for i in 0..CAPACITY as u32 {
                channel.send(i).await;
            }
        });
        // One more send should not resolve immediately (channel is full)
        let mut polled = false;
        let send_fut = channel.send(99u32);
        futures::pin_mut!(send_fut);
        let waker = futures::task::noop_waker();
        let mut cx = core::task::Context::from_waker(&waker);
        if core::future::Future::poll(send_fut.as_mut(), &mut cx) == core::task::Poll::Pending {
            polled = true;
        }
        assert!(polled, "send should be Pending when channel is at capacity");
    }

    #[test]
    fn clone_sender_routes_to_same_receiver() {
        let (tx, mut rx) = make_queue();
        let tx2 = tx.clone();
        block_on(async {
            tx.send(10).await.unwrap();
            tx2.send(20).await.unwrap();
            assert_eq!(rx.recv().await.unwrap(), 10);
            assert_eq!(rx.recv().await.unwrap(), 20);
        });
    }
}
