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
        extern crate alloc;
        // Leak the channel to obtain a 'static reference.
        // Called once per join transform at database startup — the leak is intentional
        // and matches the DB lifetime on embedded targets.
        let channel: &'static EmbassyChan<T> =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(Channel::new()));
        Ok(EmbassyJoinQueue { channel })
    }
}
