//! tokio-specific implementation of running the given future
//! on the current thread until completion

use std::future::Future;

pub struct Waiter {
    handle: tokio::runtime::Handle,
}

impl Waiter {
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }

    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.handle.block_on(fut)
    }
}
