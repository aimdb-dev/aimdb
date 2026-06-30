//! Consumer-facing reader handles (design 037 / W8).
//!
//! The [`BufferReader`] / [`JsonBufferReader`] SPIs are object-safe and
//! poll-based so adapters can implement them without a per-message
//! `Pin<Box<dyn Future>>` allocation. These handles restore the ergonomic
//! `async fn recv().await` surface for consumers by wrapping the erased
//! reader's `poll_*` method in [`core::future::poll_fn`] — which is `core`-only
//! (no_std-clean), allocation-free, and `unsafe`-free.
//!
//! The wrapped future is `Send` because the boxed reader is `Send`.

use alloc::boxed::Box;
use core::future::poll_fn;

use crate::buffer::BufferReader;
use crate::DbError;

#[cfg(feature = "remote-access")]
use crate::buffer::JsonBufferReader;

/// Owned, ergonomic handle over an erased [`BufferReader`].
///
/// Returned by `Consumer::subscribe`. This is the "boxed lane": one indirect
/// call per `recv`, zero AimDB-added heap allocations per message. (The generic
/// monomorphized `Reader<T, B>` fast lane remains dormant — see design 037 §7.)
pub struct Reader<T: Clone + Send> {
    inner: Box<dyn BufferReader<T> + Send>,
}

impl<T: Clone + Send> Reader<T> {
    /// Wrap an erased reader in an ergonomic handle.
    pub fn new(inner: Box<dyn BufferReader<T> + Send>) -> Self {
        Self { inner }
    }

    /// Receive the next value, awaiting until one is available.
    ///
    /// Allocation-free: wraps the erased reader's
    /// [`poll_recv`](BufferReader::poll_recv) via `core::future::poll_fn`.
    ///
    /// # Behavior by Buffer Type
    /// - **SPMC Ring**: Returns next value, or `Lagged(n)` if fell behind
    /// - **SingleLatest**: Waits for value change, returns most recent
    /// - **Mailbox**: Waits for slot value, takes and clears it
    pub async fn recv(&mut self) -> Result<T, DbError> {
        poll_fn(|cx| self.inner.poll_recv(cx)).await
    }

    /// Non-blocking receive — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    pub fn try_recv(&mut self) -> Result<T, DbError> {
        self.inner.try_recv()
    }
}

/// Owned, ergonomic handle over an erased [`JsonBufferReader`].
///
/// Returned by `subscribe_json`. Awaiting `recv_json` is allocation-free: it
/// wraps [`poll_recv_json`](JsonBufferReader::poll_recv_json) via
/// `core::future::poll_fn`, so the pre-W8 remote-access double box is gone.
#[cfg(feature = "remote-access")]
pub struct JsonReader {
    inner: Box<dyn JsonBufferReader + Send>,
}

#[cfg(feature = "remote-access")]
impl JsonReader {
    /// Wrap an erased JSON reader in an ergonomic handle.
    pub fn new(inner: Box<dyn JsonBufferReader + Send>) -> Self {
        Self { inner }
    }

    /// Receive the next value as JSON, awaiting until one is available.
    pub async fn recv_json(&mut self) -> Result<serde_json::Value, DbError> {
        poll_fn(|cx| self.inner.poll_recv_json(cx)).await
    }

    /// Non-blocking receive as JSON — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    pub fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError> {
        self.inner.try_recv_json()
    }
}
