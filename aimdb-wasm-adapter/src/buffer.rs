//! Single-threaded buffer implementation for the WASM runtime.
//!
//! Uses `Rc<RefCell<…>>` instead of atomics or channels — zero overhead for
//! the browser's single-threaded execution model.
//!
//! All three buffer types are supported:
//! - **SPMC Ring** — bounded `VecDeque` with per-reader cursors
//! - **SingleLatest** — single slot, version-tracked
//! - **Mailbox** — single slot, take-on-read semantics
//!
//! # Safety
//!
//! `WasmBuffer<T>` and `WasmBufferReader<T>` implement `Send + Sync` via
//! `unsafe impl` because `wasm32-unknown-unknown` is single-threaded.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use aimdb_core::buffer::{Buffer, BufferCfg, BufferReader, DynBuffer};
use aimdb_core::DbError;

// ============================================================================
// Buffer
// ============================================================================

/// Single-threaded buffer for the WASM runtime.
///
/// Wraps an `Rc<RefCell<…>>` inner enum that holds the actual buffer state.
/// All three AimDB buffer types (SPMC Ring, SingleLatest, Mailbox) share
/// this outer struct — the variant is determined by [`BufferCfg`] at
/// construction time.
pub struct WasmBuffer<T> {
    inner: Rc<RefCell<WasmBufferInner<T>>>,
}

// SAFETY: wasm32 is single-threaded — Rc<RefCell<…>> cannot be accessed concurrently
unsafe impl<T> Send for WasmBuffer<T> {}
unsafe impl<T> Sync for WasmBuffer<T> {}

/// Internal buffer state — one variant per buffer type.
enum WasmBufferInner<T> {
    /// Bounded ring buffer with independent consumer cursors.
    SpmcRing {
        /// Ring storage (oldest at front, newest at back).
        ring: VecDeque<T>,
        /// Maximum number of items.
        capacity: usize,
        /// Monotonic write counter — each push increments this.
        /// Readers track their own position against this counter.
        write_seq: u64,
        /// Wakers registered by readers waiting for new data.
        wakers: Vec<Waker>,
    },

    /// Only the latest value, skip intermediates.
    SingleLatest {
        /// Current value (None until first push).
        value: Option<T>,
        /// Monotonic version counter — incremented on each push.
        version: u64,
        /// Wakers registered by readers waiting for a new version.
        wakers: Vec<Waker>,
    },

    /// Single slot, overwrite semantics.
    Mailbox {
        /// Current slot value (taken on read).
        slot: Option<T>,
        /// Wakers registered by readers waiting for a value.
        wakers: Vec<Waker>,
    },
}

impl<T: Clone + Send + 'static> Buffer<T> for WasmBuffer<T> {
    type Reader = WasmBufferReader<T>;

    fn new(cfg: &BufferCfg) -> Self {
        let inner = match cfg {
            BufferCfg::SpmcRing { capacity } => WasmBufferInner::SpmcRing {
                ring: VecDeque::with_capacity(*capacity),
                capacity: *capacity,
                write_seq: 0,
                wakers: Vec::new(),
            },
            BufferCfg::SingleLatest => WasmBufferInner::SingleLatest {
                value: None,
                version: 0,
                wakers: Vec::new(),
            },
            BufferCfg::Mailbox => WasmBufferInner::Mailbox {
                slot: None,
                wakers: Vec::new(),
            },
        };

        WasmBuffer {
            inner: Rc::new(RefCell::new(inner)),
        }
    }

    fn push(&self, value: T) {
        let mut inner = self.inner.borrow_mut();
        match &mut *inner {
            WasmBufferInner::SpmcRing {
                ring,
                capacity,
                write_seq,
                wakers,
            } => {
                if ring.len() >= *capacity {
                    ring.pop_front();
                }
                ring.push_back(value);
                *write_seq += 1;
                wake_all(wakers);
            }
            WasmBufferInner::SingleLatest {
                value: slot,
                version,
                wakers,
            } => {
                *slot = Some(value);
                *version += 1;
                wake_all(wakers);
            }
            WasmBufferInner::Mailbox { slot, wakers } => {
                *slot = Some(value);
                wake_all(wakers);
            }
        }
    }

    fn subscribe(&self) -> Self::Reader {
        let inner = self.inner.borrow();
        let state = match &*inner {
            WasmBufferInner::SpmcRing { write_seq, .. } => {
                // New readers start at the current write position (no backfill).
                ReaderState::SpmcRing {
                    read_seq: *write_seq,
                }
            }
            WasmBufferInner::SingleLatest { version, .. } => {
                // Will fire on next push (version change).
                ReaderState::SingleLatest {
                    last_seen_version: *version,
                }
            }
            WasmBufferInner::Mailbox { .. } => ReaderState::Mailbox,
        };

        WasmBufferReader {
            buffer: Rc::clone(&self.inner),
            state,
        }
    }
}

/// Explicit DynBuffer implementation so WasmBuffer can be stored as a trait object.
impl<T: Clone + Send + 'static> DynBuffer<T> for WasmBuffer<T> {
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value);
    }

    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ============================================================================
// Reader
// ============================================================================

/// Single-threaded buffer reader for the WASM runtime.
///
/// Created by [`WasmBuffer::subscribe()`]. Each reader maintains independent
/// state (cursor position, last-seen version) and can advance at its own pace.
pub struct WasmBufferReader<T> {
    buffer: Rc<RefCell<WasmBufferInner<T>>>,
    state: ReaderState,
}

// SAFETY: wasm32 is single-threaded — no concurrent access possible
unsafe impl<T> Send for WasmBufferReader<T> {}
unsafe impl<T> Sync for WasmBufferReader<T> {}

/// Per-reader tracking state.
enum ReaderState {
    /// For SPMC Ring: the sequence number of the next item to read.
    SpmcRing { read_seq: u64 },
    /// For SingleLatest: the version we last observed.
    SingleLatest { last_seen_version: u64 },
    /// For Mailbox: no extra state (take-on-read).
    Mailbox,
}

impl<T: Clone + Send + 'static> BufferReader<T> for WasmBufferReader<T> {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>> {
        Box::pin(WasmRecvFuture { reader: self })
    }

    fn try_recv(&mut self) -> Result<T, DbError> {
        let mut inner = self.buffer.borrow_mut();
        match (&mut *inner, &mut self.state) {
            (
                WasmBufferInner::SpmcRing {
                    ring, write_seq, ..
                },
                ReaderState::SpmcRing { read_seq },
            ) => {
                if *read_seq >= *write_seq {
                    return Err(DbError::BufferEmpty);
                }
                // Calculate offset into the ring
                let ring_len = ring.len() as u64;
                let oldest_seq = write_seq.saturating_sub(ring_len);

                if *read_seq < oldest_seq {
                    // Reader fell behind — skip to oldest available
                    let lag_count = oldest_seq - *read_seq;
                    *read_seq = oldest_seq;
                    return Err(DbError::BufferLagged {
                        lag_count,
                        _buffer_name: (),
                    });
                }

                let offset = (*read_seq - oldest_seq) as usize;
                let value = ring[offset].clone();
                *read_seq += 1;
                Ok(value)
            }
            (
                WasmBufferInner::SingleLatest { value, version, .. },
                ReaderState::SingleLatest { last_seen_version },
            ) => {
                if *version == *last_seen_version {
                    return Err(DbError::BufferEmpty);
                }
                match value {
                    Some(v) => {
                        *last_seen_version = *version;
                        Ok(v.clone())
                    }
                    None => Err(DbError::BufferEmpty),
                }
            }
            (WasmBufferInner::Mailbox { slot, .. }, ReaderState::Mailbox) => {
                slot.take().ok_or(DbError::BufferEmpty)
            }
            _ => unreachable!("reader state mismatch"),
        }
    }
}

// ============================================================================
// Async recv future
// ============================================================================

/// Future returned by `WasmBufferReader::recv()`.
///
/// On each poll:
/// 1. Try to read a value (non-blocking).
/// 2. If available, return `Poll::Ready(Ok(value))`.
/// 3. If not, register the waker and return `Poll::Pending`.
///
/// The waker is woken when `WasmBuffer::push()` fires.
struct WasmRecvFuture<'a, T> {
    reader: &'a mut WasmBufferReader<T>,
}

// SAFETY: wasm32 is single-threaded
unsafe impl<T> Send for WasmRecvFuture<'_, T> {}

impl<T: Clone + Send + 'static> Future for WasmRecvFuture<'_, T> {
    type Output = Result<T, DbError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        // Try non-blocking read first
        match this.reader.try_recv() {
            Ok(value) => Poll::Ready(Ok(value)),
            Err(e @ DbError::BufferLagged { .. }) => Poll::Ready(Err(e)),
            Err(DbError::BufferEmpty) => {
                // Register waker so we get woken on next push
                let mut inner = this.reader.buffer.borrow_mut();
                let wakers = match &mut *inner {
                    WasmBufferInner::SpmcRing { wakers, .. } => wakers,
                    WasmBufferInner::SingleLatest { wakers, .. } => wakers,
                    WasmBufferInner::Mailbox { wakers, .. } => wakers,
                };
                // Replace existing waker for this reader if present, or add new one.
                // For simplicity, we always push. Wakers are drained on each push().
                wakers.push(cx.waker().clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Wake all registered wakers and clear the list.
fn wake_all(wakers: &mut Vec<Waker>) {
    for waker in wakers.drain(..) {
        waker.wake();
    }
}
