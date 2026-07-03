//! Embassy buffer implementations for AimDB
//!
//! This module provides Embassy-specific implementations of the buffer traits
//! defined in `aimdb-core`. It uses Embassy's no_std async synchronization primitives:
//!
//! - **SPMC Ring**: `embassy_sync::pubsub::PubSubChannel` for bounded multi-consumer queues
//! - **SingleLatest**: `embassy_sync::watch::Watch` for latest-value semantics
//! - **Mailbox**: `embassy_sync::channel::Channel` (capacity=1) for single-slot mailbox
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────┐
//! │ EmbassyBuffer<T>    │  ←  BufferBackend trait
//! └──────────┬──────────┘
//!            │
//!     ┌──────┴──────────────────────┐
//!     │ EmbassyBufferInner<T>       │
//!     ├─────────────────────────────┤
//!     │ • PubSub     (SPMC Ring)    │
//!     │ • Watch      (SingleLatest) │
//!     │ • Channel    (Mailbox)      │
//!     └─────────────────────────────┘
//! ```
//!
//! # Limitations
//!
//! Embassy's sync primitives require compile-time constants for sizing:
//! - `PubSubChannel<M, T, CAP, SUBS, PUBS>` - fixed capacity and subscriber/publisher counts
//! - `Watch<M, T, N>` - fixed number of receivers
//!
//! The buffer configuration's runtime capacity parameter must match the const generic `CAP`.
//! A panic will occur if there's a mismatch. This is a design constraint of Embassy's no_std
//! implementation.
//!
//! # Arc-Based Readers
//!
//! To enable universal subscription support (BufferSubscribable trait), readers hold
//! Arc references to the buffer internals. This requires the `alloc` feature (enabled by default).

extern crate alloc;

use alloc::string::String;
use alloc::sync::Arc;
use core::task::{Context, Poll};

use aimdb_core::buffer::{Buffer, BufferCfg, BufferReader};
use aimdb_core::DbError;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::pubsub::{PubSubChannel, Subscriber, WaitResult};
use embassy_sync::watch::{Receiver as WatchReceiver, Watch};

#[cfg(feature = "observability")]
use aimdb_core::buffer::{BufferCounters, BufferMetrics, BufferMetricsSnapshot};

/// Embassy buffer implementation that wraps the appropriate Embassy primitive
/// based on the buffer configuration.
///
/// Uses Arc internally to enable owned readers that can outlive borrows,
/// making it compatible with the BufferSubscribable trait.
///
/// # Type Parameters
/// - `T`: The value type (must be `Clone` for Embassy primitives)
/// - `CAP`: Capacity for SPMC ring buffer (const generic)
/// - `SUBS`: Maximum subscribers for SPMC ring (const generic)
/// - `PUBS`: Maximum publishers for SPMC ring (const generic)
/// - `WATCH_N`: Maximum receivers for Watch channel (const generic)
///
/// # Example
/// ```no_run
/// use aimdb_embassy_adapter::EmbassyBuffer;
/// use aimdb_core::buffer::{Buffer, BufferReader};
///
/// // Create an SPMC ring buffer with capacity 32, 4 subscribers, 2 publishers
/// type MyBuffer = EmbassyBuffer<u32, 32, 4, 2, 4>;
///
/// # async fn example() {
/// use aimdb_core::buffer::Reader;
/// let buffer: MyBuffer = MyBuffer::new_spmc();
/// // `subscribe()` yields the concrete reader; wrap it in the ergonomic,
/// // allocation-free `Reader<T>` handle for `recv().await` (design 037 / W8).
/// let mut reader = Reader::new(Box::new(buffer.subscribe()));
/// buffer.push(42);
/// let value = reader.recv().await.unwrap();
/// # }
/// ```
pub struct EmbassyBuffer<
    T: Clone,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
    const WATCH_N: usize,
> {
    inner: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
    #[cfg(feature = "observability")]
    metrics: Arc<BufferCounters>,
}

/// Inner buffer variants using Embassy primitives — deliberately kept
/// private (unlike `EmbassyBufferReader`, which must be `pub`): it is an
/// implementation detail, not part of the supported API. It does appear in
/// `EmbassyBufferReader`'s fields, which triggers a `private_interfaces`
/// warning since (unlike struct fields) enum variant fields can't carry a
/// visibility narrower than their enum — see the `#[allow(...)]` on
/// `EmbassyBufferReader` below.
enum EmbassyBufferInner<
    T: Clone,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
    const WATCH_N: usize,
> {
    /// SPMC Ring buffer using PubSubChannel
    /// Supports multiple publishers and subscribers with bounded capacity
    SpmcRing(PubSubChannel<CriticalSectionRawMutex, T, CAP, SUBS, PUBS>),

    /// SingleLatest buffer using Watch
    /// Latest value semantics with multiple receivers
    Watch(Watch<CriticalSectionRawMutex, T, WATCH_N>),

    /// Mailbox buffer using Channel with capacity 1
    /// Single-slot overwrite behavior (try_send drops oldest)
    Mailbox(Channel<CriticalSectionRawMutex, T, 1>),
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    /// Create a new SPMC ring buffer
    pub fn new_spmc() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::SpmcRing(PubSubChannel::new())),
            #[cfg(feature = "observability")]
            metrics: Arc::new(BufferCounters::new(CAP)),
        }
    }

    /// Create a new SingleLatest buffer
    pub fn new_watch() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::Watch(Watch::new())),
            #[cfg(feature = "observability")]
            metrics: Arc::new(BufferCounters::new(1)),
        }
    }

    /// Create a new Mailbox buffer
    pub fn new_mailbox() -> Self {
        Self {
            inner: Arc::new(EmbassyBufferInner::Mailbox(Channel::new())),
            #[cfg(feature = "observability")]
            metrics: Arc::new(BufferCounters::new(1)),
        }
    }
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > Buffer<T> for EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    type Reader = EmbassyBufferReader<T, CAP, SUBS, PUBS, WATCH_N>;

    fn new(cfg: &BufferCfg) -> Self {
        match cfg {
            BufferCfg::SpmcRing { capacity } => {
                // Note: Embassy requires compile-time capacity, so we use CAP const generic
                // Validate that the runtime capacity matches the const generic
                if *capacity != CAP {
                    panic!(
                        "BufferCfg::SpmcRing capacity ({}) does not match const generic CAP ({})",
                        capacity, CAP
                    );
                }
                Self::new_spmc()
            }
            BufferCfg::SingleLatest => Self::new_watch(),
            BufferCfg::Mailbox => Self::new_mailbox(),
        }
    }

    fn push(&self, value: T) {
        #[cfg(feature = "observability")]
        self.metrics.increment_produced();

        match &*self.inner {
            EmbassyBufferInner::SpmcRing(channel) => {
                // Use immediate publish to avoid blocking
                // This matches Tokio's broadcast behavior where send() doesn't block
                let publisher = channel.immediate_publisher();
                publisher.publish_immediate(value);
            }
            EmbassyBufferInner::Watch(watch) => {
                watch.sender().send(value);
            }
            EmbassyBufferInner::Mailbox(channel) => {
                // Try to send, if full, clear and try again (overwrite semantic)
                let sender = channel.sender();
                if sender.try_send(value.clone()).is_err() {
                    // Channel is full (has 1 old message), try to receive it to make space
                    let _ = channel.try_receive();
                    // Now try to send again
                    let _ = sender.try_send(value);
                }
            }
        }
    }

    /// The embassy subscriber/receiver is created **eagerly, here**, matching
    /// the Tokio adapter (design 039 F8/F9). Previously this was lazy (deferred
    /// to the first poll), which left a subscribe→first-poll window where a
    /// message produced before that first poll was silently missed for
    /// `SpmcRing` (no replay semantics — a subscriber only sees messages sent
    /// after it starts listening). Registering here closes that window.
    ///
    /// Slot exhaustion (all `SUBS`/`WATCH_N` registration slots taken) is
    /// recorded as `None` rather than panicking or failing `subscribe()`
    /// itself; it surfaces as `DbError::BufferClosed` on the first
    /// `poll_recv`/`try_recv` call (see [`EmbassyBufferReader`]).
    fn subscribe(&self) -> Self::Reader {
        match &*self.inner {
            EmbassyBufferInner::SpmcRing(channel) => EmbassyBufferReader::SpmcRing {
                subscriber: make_spmc_sub(channel).ok(),
                buffer: Arc::clone(&self.inner),
                #[cfg(feature = "observability")]
                metrics: Arc::clone(&self.metrics),
            },
            EmbassyBufferInner::Watch(watch) => EmbassyBufferReader::Watch {
                receiver: make_watch_rx(watch).ok(),
                buffer: Arc::clone(&self.inner),
                #[cfg(feature = "observability")]
                metrics: Arc::clone(&self.metrics),
            },
            EmbassyBufferInner::Mailbox(_) => EmbassyBufferReader::Mailbox {
                buffer: Arc::clone(&self.inner),
                #[cfg(feature = "observability")]
                metrics: Arc::clone(&self.metrics),
            },
        }
    }
}

/// Explicit DynBuffer implementation for EmbassyBuffer
///
/// Required since there is no blanket impl - each adapter provides its own.
impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > aimdb_core::buffer::DynBuffer<T> for EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }

    fn subscribe_boxed(&self) -> alloc::boxed::Box<dyn aimdb_core::buffer::BufferReader<T> + Send> {
        alloc::boxed::Box::new(self.subscribe())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn peek(&self) -> Option<T> {
        match &*self.inner {
            // Watch stores the latest value natively; try_get() clones it
            // without consuming a receiver slot or advancing any cursor.
            EmbassyBufferInner::Watch(watch) => watch.try_get(),
            // Channel<_, T, 1>::try_peek() clones the pending slot without
            // removing it (the slot is drained once a consumer receives).
            // Mirrors the Tokio Mailbox arm.
            EmbassyBufferInner::Mailbox(channel) => channel.try_peek().ok(),
            // PubSub has no canonical latest — see design 031 §SPMC Ring.
            EmbassyBufferInner::SpmcRing(_) => None,
        }
    }

    #[cfg(feature = "observability")]
    fn metrics_snapshot(&self) -> Option<BufferMetricsSnapshot> {
        Some(<Self as BufferMetrics>::metrics(self))
    }

    #[cfg(feature = "observability")]
    fn reset_metrics(&self) {
        <Self as BufferMetrics>::reset_metrics(self);
    }
}

/// Implementation of BufferMetrics for EmbassyBuffer (metrics feature only).
///
/// Counter state is shared with readers via `Arc<BufferCounters>`. Occupancy
/// readings:
/// - `SpmcRing`: `PubSubChannel::len()` (current queued messages) over `CAP`.
/// - `Watch`: `(1, 1)` once a value has been published (tracked via the
///   `produced` counter — embassy's `Watch` doesn't expose state), else `(0, 1)`.
/// - `Mailbox`: `(1, 1)` if the slot is occupied, else `(0, 1)`.
#[cfg(feature = "observability")]
impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > BufferMetrics for EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>
{
    fn metrics(&self) -> BufferMetricsSnapshot {
        let snap_partial = self.metrics.snapshot((0, self.metrics.capacity()));
        let current_occupancy = match &*self.inner {
            EmbassyBufferInner::SpmcRing(channel) => channel.len(),
            EmbassyBufferInner::Watch(_) => {
                if snap_partial.produced_count > 0 {
                    1
                } else {
                    0
                }
            }
            EmbassyBufferInner::Mailbox(channel) => {
                if channel.is_full() {
                    1
                } else {
                    0
                }
            }
        };
        self.metrics
            .snapshot((current_occupancy, self.metrics.capacity()))
    }

    fn reset_metrics(&self) {
        self.metrics.reset();
    }
}

// ============================================================================
// Poll plumbing (design 037 / W8)
// ============================================================================
//
// `poll_recv` drives embassy-sync's *public* poll methods directly —
// `Subscriber::poll_next_message`, `Receiver::poll_changed`, and
// `Channel::poll_receive` — so there is zero allocation per message and no
// per-message future box. The reader stores the subscriber/receiver, created
// eagerly at `subscribe()` time (design 039 F8/F9); `try_recv` uses the
// matching `try_*` methods. No `unsafe` beyond the pre-existing `'static`
// borrow extension in the `make_*` helpers (the `Arc` keeps the primitive
// alive).

/// Buffer-name strings used both in [`make_spmc_sub`]'s/[`make_watch_rx`]'s
/// own error and in `poll_recv`/`try_recv`'s reconstructed one (design 039
/// F8/F9) — kept as constants so the two can't drift apart.
const SPMC_BUFFER_NAME: &str = "embassy spmc ring";
const WATCH_BUFFER_NAME: &str = "embassy watch";

/// Persistent SpmcRing subscriber with a lifetime extended to `'static` (the
/// owning `Arc<EmbassyBufferInner>` keeps the channel alive for the reader).
type SpmcSub<T, const CAP: usize, const SUBS: usize, const PUBS: usize> =
    Subscriber<'static, CriticalSectionRawMutex, T, CAP, SUBS, PUBS>;

/// Persistent Watch receiver, `'static` for the same reason as [`SpmcSub`].
type WatchRx<T, const WATCH_N: usize> = WatchReceiver<'static, CriticalSectionRawMutex, T, WATCH_N>;

/// Create the persistent SpmcRing subscriber, extending its borrow to `'static`.
///
/// SAFETY: two invariants must both hold, or this is a use-after-free:
/// 1. The `Arc<EmbassyBufferInner>` in the reader keeps the `PubSubChannel`
///    alive for the reader's whole life, so the `'static` subscriber never
///    outlives the channel *while the Arc is alive*.
/// 2. `EmbassyBufferReader::SpmcRing`'s field declaration order places
///    `subscriber` **before** `buffer: Arc<..>`, so Rust's in-order field
///    drop runs the subscriber's `Drop` (which calls `unregister_subscriber`
///    on the channel) *before* the `Arc` can be released and the channel
///    freed. Reordering those fields — even putting `buffer` first —
///    reopens a use-after-free window between the guard's drop and the
///    Arc's drop. Do not reorder without re-verifying this. (Same invariant
///    applies to `EmbassyBufferReader::Watch` / [`make_watch_rx`].)
fn make_spmc_sub<
    T: Clone + Send + 'static,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
>(
    channel: &PubSubChannel<CriticalSectionRawMutex, T, CAP, SUBS, PUBS>,
) -> Result<SpmcSub<T, CAP, SUBS, PUBS>, DbError> {
    let channel_static: &'static PubSubChannel<CriticalSectionRawMutex, T, CAP, SUBS, PUBS> =
        unsafe { &*(channel as *const _) };
    channel_static.subscriber().map_err(|_| {
        defmt::error!(
            "AimDB: SpmcRing subscriber slot exhausted (max SUBS={}). \
             Increase the CONSUMERS const generic on buffer_sized<CAP, CONSUMERS>. \
             Count one slot per .tap(), .link_to() connector, and each transform_join input.",
            SUBS
        );
        DbError::BufferClosed {
            buffer_name: String::from(SPMC_BUFFER_NAME),
        }
    })
}

/// Create the persistent Watch receiver, extending its borrow to `'static`.
///
/// SAFETY: see [`make_spmc_sub`] — both the Arc-keeps-it-alive invariant and
/// the field-declaration-order-guarantees-drop-order invariant apply here too.
fn make_watch_rx<T: Clone + Send + 'static, const WATCH_N: usize>(
    watch: &Watch<CriticalSectionRawMutex, T, WATCH_N>,
) -> Result<WatchRx<T, WATCH_N>, DbError> {
    let watch_static: &'static Watch<CriticalSectionRawMutex, T, WATCH_N> =
        unsafe { &*(watch as *const _) };
    watch_static.receiver().ok_or(DbError::BufferClosed {
        buffer_name: String::from(WATCH_BUFFER_NAME),
    })
}

/// Reader for Embassy buffers — one variant per buffer kind, each holding
/// only the state it needs (design 039 F8/F9; mirrors `TokioBufferReader`'s
/// existing enum shape on the tokio adapter).
///
/// The persistent Subscriber/Receiver is registered **eagerly, at
/// `subscribe()` time** — not lazily on first poll — closing the
/// subscribe→first-poll window where a message produced before that first
/// poll was silently missed. Slot exhaustion is recorded as `None` and
/// surfaces as `DbError::BufferClosed` on first use (see `poll_recv`/
/// `try_recv`) — reconstructed fresh from `SPMC_BUFFER_NAME`/
/// `WATCH_BUFFER_NAME` rather than stored, since `DbError` is not `Clone`
/// (it wraps non-`Clone` types like `std::io::Error` in some `std`-only
/// variants) and the message is always the same fixed text per buffer kind.
// `EmbassyBufferInner` is deliberately private (implementation detail, not
// supported API), but enum variant fields can't carry a visibility narrower
// than their enum — unlike struct fields, which default private. Silencing
// rather than making `EmbassyBufferInner` `pub` keeps it out of the crate's
// actual public surface (it wouldn't become nameable either way, since
// nothing re-exports it, but `pub` would still widen what `cargo doc`/
// `pub(crate)`-auditing tools report).
#[allow(private_interfaces)]
pub enum EmbassyBufferReader<
    T: Clone + Send + 'static,
    const CAP: usize,
    const SUBS: usize,
    const PUBS: usize,
    const WATCH_N: usize,
> {
    SpmcRing {
        // Drop order matters: `subscriber` holds a `&'static` ref *into*
        // `buffer` (see make_spmc_sub). Rust drops struct/variant fields in
        // declaration order, so `subscriber` MUST be declared — and
        // therefore dropped — before `buffer`. Reordering these fields
        // reopens a use-after-free; see the SAFETY comment on make_spmc_sub.
        subscriber: Option<SpmcSub<T, CAP, SUBS, PUBS>>,
        buffer: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
        /// Shared counter state (cloned from the parent buffer at subscribe time).
        #[cfg(feature = "observability")]
        metrics: Arc<BufferCounters>,
    },
    Watch {
        // Same drop-order requirement as `SpmcRing::subscriber`; see
        // make_watch_rx's SAFETY comment.
        receiver: Option<WatchRx<T, WATCH_N>>,
        buffer: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
        #[cfg(feature = "observability")]
        metrics: Arc<BufferCounters>,
    },
    Mailbox {
        buffer: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
        #[cfg(feature = "observability")]
        metrics: Arc<BufferCounters>,
    },
}

impl<
        T: Clone + Send + 'static,
        const CAP: usize,
        const SUBS: usize,
        const PUBS: usize,
        const WATCH_N: usize,
    > BufferReader<T> for EmbassyBufferReader<T, CAP, SUBS, PUBS, WATCH_N>
{
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, DbError>> {
        match self {
            EmbassyBufferReader::SpmcRing {
                subscriber,
                #[cfg(feature = "observability")]
                metrics,
                ..
            } => {
                let Some(sub) = subscriber else {
                    return Poll::Ready(Err(DbError::BufferClosed {
                        buffer_name: String::from(SPMC_BUFFER_NAME),
                    }));
                };
                // Poll directly via embassy-sync's public `poll_next_message`
                // (no future box, no allocation per message; lag preserved).
                match sub.poll_next_message(cx) {
                    Poll::Ready(WaitResult::Message(value)) => {
                        #[cfg(feature = "observability")]
                        metrics.increment_consumed();
                        Poll::Ready(Ok(value))
                    }
                    Poll::Ready(WaitResult::Lagged(n)) => {
                        #[cfg(feature = "observability")]
                        metrics.add_dropped(n);
                        Poll::Ready(Err(DbError::BufferLagged {
                            lag_count: n,
                            buffer_name: String::from(SPMC_BUFFER_NAME),
                        }))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
            EmbassyBufferReader::Watch {
                receiver,
                #[cfg(feature = "observability")]
                metrics,
                ..
            } => {
                let Some(rx) = receiver else {
                    return Poll::Ready(Err(DbError::BufferClosed {
                        buffer_name: String::from(WATCH_BUFFER_NAME),
                    }));
                };
                match rx.poll_changed(cx) {
                    Poll::Ready(value) => {
                        #[cfg(feature = "observability")]
                        metrics.increment_consumed();
                        Poll::Ready(Ok(value))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
            EmbassyBufferReader::Mailbox {
                buffer,
                #[cfg(feature = "observability")]
                metrics,
            } => {
                let EmbassyBufferInner::Mailbox(channel) = &**buffer else {
                    unreachable!("Mailbox reader always wraps EmbassyBufferInner::Mailbox");
                };
                match channel.poll_receive(cx) {
                    Poll::Ready(value) => {
                        #[cfg(feature = "observability")]
                        metrics.increment_consumed();
                        Poll::Ready(Ok(value))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    fn try_recv(&mut self) -> Result<T, DbError> {
        match self {
            EmbassyBufferReader::SpmcRing {
                subscriber,
                #[cfg(feature = "observability")]
                metrics,
                ..
            } => {
                let Some(sub) = subscriber else {
                    return Err(DbError::BufferClosed {
                        buffer_name: String::from(SPMC_BUFFER_NAME),
                    });
                };
                // Lag-honest (design 039 F10): `try_next_message` (not the
                // `_pure` variant) surfaces `WaitResult::Lagged` instead of
                // silently discarding it, mirroring `poll_recv` above.
                match sub.try_next_message() {
                    Some(WaitResult::Message(value)) => {
                        #[cfg(feature = "observability")]
                        metrics.increment_consumed();
                        Ok(value)
                    }
                    Some(WaitResult::Lagged(n)) => {
                        #[cfg(feature = "observability")]
                        metrics.add_dropped(n);
                        Err(DbError::BufferLagged {
                            lag_count: n,
                            buffer_name: String::from(SPMC_BUFFER_NAME),
                        })
                    }
                    None => Err(DbError::BufferEmpty),
                }
            }
            EmbassyBufferReader::Watch {
                receiver,
                #[cfg(feature = "observability")]
                metrics,
                ..
            } => {
                let Some(rx) = receiver else {
                    return Err(DbError::BufferClosed {
                        buffer_name: String::from(WATCH_BUFFER_NAME),
                    });
                };
                match rx.try_changed() {
                    Some(value) => {
                        #[cfg(feature = "observability")]
                        metrics.increment_consumed();
                        Ok(value)
                    }
                    None => Err(DbError::BufferEmpty),
                }
            }
            EmbassyBufferReader::Mailbox {
                buffer,
                #[cfg(feature = "observability")]
                metrics,
            } => {
                let EmbassyBufferInner::Mailbox(channel) = &**buffer else {
                    unreachable!("Mailbox reader always wraps EmbassyBufferInner::Mailbox");
                };
                match channel.try_receive() {
                    Ok(val) => {
                        #[cfg(feature = "observability")]
                        metrics.increment_consumed();
                        Ok(val)
                    }
                    Err(_) => Err(DbError::BufferEmpty),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Host-test scaffolding ────────────────────────────────────────────
    // The crate links `defmt` (workspace dep) and embassy-time's
    // `defmt-timestamp-uptime`, but on the host neither a defmt logger nor a
    // time driver exists. `host_test_stubs!` provides no-op stubs so the test
    // binary links (expanded here for the lib test binary; integration tests
    // expand it themselves). Run via the `test` Make target, or directly:
    //   cargo test -p aimdb-embassy-adapter \
    //       --no-default-features --features "alloc,embassy-sync,embassy-time"
    // (`embassy-runtime` would pull the cortex-m executor, which can't host-build.)
    crate::host_test_stubs!();

    // Note: Embassy tests typically run on actual embedded targets or with embassy-executor
    // For now, these are basic compilation tests. Integration tests would need embassy-executor.

    #[test]
    fn test_buffer_creation() {
        // Test that buffers can be created
        type TestBuffer = EmbassyBuffer<u32, 10, 4, 2, 4>;
        let _spmc: TestBuffer = TestBuffer::new_spmc();
        let _watch: TestBuffer = TestBuffer::new_watch();
        let _mailbox: TestBuffer = TestBuffer::new_mailbox();
    }

    #[test]
    fn test_buffer_from_cfg() {
        type TestBuffer = EmbassyBuffer<u32, 10, 4, 2, 4>;

        let cfg1 = BufferCfg::SpmcRing { capacity: 10 };
        let _buf1: TestBuffer = Buffer::new(&cfg1);

        let cfg2 = BufferCfg::SingleLatest;
        let _buf2: TestBuffer = Buffer::new(&cfg2);

        let cfg3 = BufferCfg::Mailbox;
        let _buf3: TestBuffer = Buffer::new(&cfg3);
    }

    // ========================================================================
    // F1 regression: reader field drop order (design 039)
    //
    // `spmc_subscriber`/`watch_receiver` hold `&'static` refs pointer-extended
    // into `buffer: Arc<..>` (see make_spmc_sub / make_watch_rx SAFETY
    // comments). This test alone cannot detect a use-after-free under a plain
    // `cargo test` run — its job is to give Miri something to catch:
    //   cargo +nightly miri test -p aimdb-embassy-adapter \
    //       --no-default-features --features "alloc,embassy-sync,embassy-time" \
    //       test_reader_outlives_buffer_no_uaf
    // ========================================================================

    #[test]
    fn test_reader_outlives_buffer_no_uaf() {
        type TestBuffer = EmbassyBuffer<u32, 4, 2, 1, 2>;

        // SpmcRing and Watch both go through the unsafe 'static-extension
        // path once `try_recv` forces lazy subscriber/receiver creation.
        // Mailbox has no such state; included for parity/symmetry.
        for make in [
            TestBuffer::new_spmc as fn() -> TestBuffer,
            TestBuffer::new_watch,
            TestBuffer::new_mailbox,
        ] {
            // Buffer dropped first, reader (holding the last Arc) dropped
            // after forcing lazy state creation — the order that was UAF
            // before the F1 field reorder.
            let buffer = make();
            let mut reader = buffer.subscribe();
            drop(buffer);
            let _ = reader.try_recv();
            drop(reader);

            // Reverse order too, for completeness — always safe, no
            // 'static-extended state outlives the buffer.
            let buffer = make();
            let mut reader = buffer.subscribe();
            let _ = reader.try_recv();
            drop(reader);
            drop(buffer);
        }
    }

    // ========================================================================
    // F8/F9/F10 regression tests (design 039) — eager subscribe-time
    // registration, slot exhaustion surfaced on first use, lag-honest
    // try_recv. Synchronous (try_recv only), no embassy executor needed.
    // ========================================================================

    #[test]
    fn test_subscribe_registers_eagerly() {
        type TestBuffer = EmbassyBuffer<i32, 4, 2, 1, 2>;
        let buffer: TestBuffer = TestBuffer::new_spmc();

        // Subscribe A, push X, subscribe B (after the push), push Y.
        let mut reader_a = buffer.subscribe();
        Buffer::push(&buffer, 1);
        let mut reader_b = buffer.subscribe();
        Buffer::push(&buffer, 2);

        // A registered before the first push, so it sees both messages — the
        // subscribe→first-poll loss window (F9) is gone. B only sees the
        // message pushed after it subscribed (no replay — standard SPMC
        // cursor semantics, distinct from Watch's D1 behavior).
        assert_eq!(reader_a.try_recv().unwrap(), 1);
        assert_eq!(reader_a.try_recv().unwrap(), 2);
        assert_eq!(reader_b.try_recv().unwrap(), 2);
        assert!(matches!(reader_b.try_recv(), Err(DbError::BufferEmpty)));
    }

    #[test]
    fn test_subscribe_slot_exhaustion_surfaces_on_first_poll() {
        type TestBuffer = EmbassyBuffer<i32, 4, 2, 1, 2>; // SUBS = 2

        let buffer: TestBuffer = TestBuffer::new_spmc();
        let _r1 = buffer.subscribe();
        let _r2 = buffer.subscribe();

        // Both SUBS slots are taken. The eager `subscribe()` call itself
        // must not panic — it stores the failure, not propagate it.
        let mut over_limit = buffer.subscribe();

        // The failure surfaces on first use.
        assert!(matches!(
            over_limit.try_recv(),
            Err(DbError::BufferClosed { .. })
        ));
    }

    #[test]
    fn test_try_recv_spmc_ring_handles_lag() {
        type TestBuffer = EmbassyBuffer<i32, 4, 2, 1, 2>; // CAP = 4

        let buffer: TestBuffer = TestBuffer::new_spmc();
        let mut reader = buffer.subscribe();

        // Write 10 values into a capacity-4 ring — the reader falls behind.
        for i in 0..10 {
            Buffer::push(&buffer, i);
        }

        // Lag must surface as `BufferLagged`, not be silently dropped
        // (design 039 F10 — `try_next_message`, not `_pure`).
        let first = reader.try_recv();
        assert!(
            matches!(first, Err(DbError::BufferLagged { .. })),
            "expected BufferLagged, got {:?}",
            first
        );

        // Subsequent calls drain the values still in the ring.
        let mut values = alloc::vec::Vec::new();
        loop {
            match reader.try_recv() {
                Ok(val) => values.push(val),
                Err(DbError::BufferEmpty) => break,
                Err(DbError::BufferLagged { .. }) => continue,
                Err(e) => panic!("unexpected error: {:?}", e),
            }
        }
        assert!(!values.is_empty());
    }

    // ========================================================================
    // peek() Tests — non-destructive buffer-native reads (design 031)
    //
    // push()/peek() are synchronous and lock a CriticalSectionRawMutex; the
    // `critical-section` std impl in dev-dependencies provides the host-side
    // implementation, so these run without an embassy executor. Run with:
    //   cargo test -p aimdb-embassy-adapter \
    //       --no-default-features --features "alloc,embassy-sync,embassy-time"
    // ========================================================================

    use aimdb_core::buffer::DynBuffer;

    type PeekBuffer = EmbassyBuffer<i32, 8, 4, 2, 4>;

    #[test]
    fn test_peek_single_latest_empty() {
        let buffer: PeekBuffer = PeekBuffer::new_watch();
        assert_eq!(DynBuffer::peek(&buffer), None);
    }

    #[test]
    fn test_peek_single_latest_returns_latest() {
        let buffer: PeekBuffer = PeekBuffer::new_watch();
        DynBuffer::push(&buffer, 1);
        DynBuffer::push(&buffer, 2);
        DynBuffer::push(&buffer, 3);
        assert_eq!(DynBuffer::peek(&buffer), Some(3));
    }

    #[test]
    fn test_peek_single_latest_is_non_destructive() {
        let buffer: PeekBuffer = PeekBuffer::new_watch();
        DynBuffer::push(&buffer, 42);
        // Multiple peeks return the same value.
        assert_eq!(DynBuffer::peek(&buffer), Some(42));
        assert_eq!(DynBuffer::peek(&buffer), Some(42));
    }

    #[test]
    fn test_peek_mailbox_empty() {
        let buffer: PeekBuffer = PeekBuffer::new_mailbox();
        assert_eq!(DynBuffer::peek(&buffer), None);
    }

    #[test]
    fn test_peek_mailbox_returns_pending() {
        let buffer: PeekBuffer = PeekBuffer::new_mailbox();
        DynBuffer::push(&buffer, 7);
        assert_eq!(DynBuffer::peek(&buffer), Some(7));
        // Peek is non-destructive: the slot is still occupied.
        assert_eq!(DynBuffer::peek(&buffer), Some(7));
    }

    #[test]
    fn test_peek_mailbox_reflects_overwrite() {
        let buffer: PeekBuffer = PeekBuffer::new_mailbox();
        DynBuffer::push(&buffer, 1);
        DynBuffer::push(&buffer, 2);
        assert_eq!(DynBuffer::peek(&buffer), Some(2));
    }

    #[test]
    fn test_peek_spmc_ring_returns_none() {
        // PubSub has no canonical latest — see design 031 §SPMC Ring.
        let buffer: PeekBuffer = PeekBuffer::new_spmc();
        assert_eq!(DynBuffer::peek(&buffer), None);
        DynBuffer::push(&buffer, 1);
        DynBuffer::push(&buffer, 2);
        assert_eq!(DynBuffer::peek(&buffer), None);
    }
}
