//! Consumer-facing reader handles.
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
#[cfg(feature = "remote")]
use alloc::vec::Vec;
use core::future::poll_fn;

use crate::buffer::BufferReader;
use crate::DbError;

#[cfg(feature = "remote")]
use crate::buffer::JsonBufferReader;

/// Owned, ergonomic handle over an erased [`BufferReader`].
///
/// Returned by `Consumer::subscribe`. This is the "boxed lane": one indirect
/// call per `recv`, zero AimDB-added heap allocations per message. (The generic
/// monomorphized `Reader<T, B>` fast lane remains dormant.)
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
/// Returned by `subscribe_json`. Awaiting `recv_json` or
/// `recv_json_bytes` wraps the corresponding poll method via
/// `core::future::poll_fn` — no extra per-message future box.
#[cfg(feature = "remote")]
pub struct JsonReader {
    inner: Box<dyn JsonBufferReader + Send>,
}

#[cfg(feature = "remote")]
impl JsonReader {
    /// Wrap an erased JSON reader in an ergonomic handle.
    pub fn new(inner: Box<dyn JsonBufferReader + Send>) -> Self {
        Self { inner }
    }

    /// Receive the next value as JSON, awaiting until one is available.
    pub async fn recv_json(&mut self) -> Result<serde_json::Value, DbError> {
        poll_fn(|cx| self.inner.poll_recv_json(cx)).await
    }

    /// Receive the next value as owned JSON bytes.
    pub async fn recv_json_bytes(&mut self) -> Result<Vec<u8>, DbError> {
        poll_fn(|cx| self.inner.poll_recv_json_bytes(cx)).await
    }

    /// Non-blocking receive as JSON — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    pub fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError> {
        self.inner.try_recv_json()
    }

    /// Non-blocking receive as owned JSON bytes — returns immediately.
    ///
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    pub fn try_recv_json_bytes(&mut self) -> Result<Vec<u8>, DbError> {
        self.inner.try_recv_json_bytes()
    }
}
