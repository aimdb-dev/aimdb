/// Runtime-specific implementations of Waiter define how to
/// running the given future on the current thread until completion
use std::future::Future;

#[cfg(feature = "std")]
pub struct Waiter {
    handle: tokio::runtime::Handle,
}

#[cfg(feature = "std")]
impl Waiter {
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.handle.block_on(fut)
    }
}
