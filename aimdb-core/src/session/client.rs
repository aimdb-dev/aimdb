//! The proactive **client** engine of the session substrate — the dual of the
//! [`server`](super::server): it *dials* a [`Connection`] via a [`Dialer`],
//! *sends* [`Inbound`] / *receives* [`Outbound`], and demultiplexes replies by `id`.
//!
//! [`run_client`] owns the demux core and returns a [`ClientHandle`] for
//! caller-initiated RPC (`call`/`subscribe`/`write`) plus the engine future for
//! the runner to drive (spawn-free). [`pump_client`] is a thin wrapper that
//! mirrors records over the same engine.
//!
//! Runtime-neutral: the only runtime-specific primitive is *time* (reconnect
//! backoff + keepalive), via a [`RuntimeContext`](crate::RuntimeContext); everything else is
//! `futures` channels. The demux loop uses the same **extract-then-act** shape as
//! the server (compute a [`ClientStep`], then act once the arm borrows release).

use crate::session::events::{report, SessionEvent};

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use async_channel::{Receiver, Sender};
use futures_channel::oneshot;
use futures_util::{select_biased, FutureExt, StreamExt};
use hashbrown::HashMap;

use super::{
    BoxFut, BoxStream, CodecError, Connection, Dialer, EnvelopeCodec, Inbound, Outbound, Payload,
    RpcError, SubUpdate, TransportError,
};
use crate::AimDb;

/// Capacity of a subscription's client-side event sink. Bounded (was
/// `unbounded`) so a slow consumer can't grow memory without limit under a fast
/// wildcard set; matches the server's per-connection `EVENT_BUFFER`. On overflow
/// the run loop drops the update and lets the loss surface as a `seq` gap
/// (`SubUpdate::skipped`) on the next delivered update — for late-join snapshots
/// too, which a wildcard subscription can burst well past this cap.
const SUBSCRIBE_CHANNEL_CAP: usize = 256;

/// Ceiling on [`ClientConfig::max_offline_queue`]: the channel preallocates its
/// ring, so `usize::MAX` would abort in the allocator rather than reach it.
const MAX_COMMAND_QUEUE_CAP: usize = 8192;

/// Client engine knobs. Durations are in **milliseconds** so the engine stays
/// `no_std`-clean; plain milliseconds turned into `core::time::Duration` for the clock.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Redial after a dropped/failed connection instead of ending the engine.
    /// Replays outbound traffic only: pending calls fail and open subscriptions
    /// are not re-issued (so `pump_client` inbound mirroring stops after the first
    /// disconnect; outbound survives).
    pub reconnect: bool,
    /// Base delay (ms) before the first redial; subsequent redials grow
    /// exponentially, capped at [`max_reconnect_delay`](Self::max_reconnect_delay).
    pub reconnect_delay: u64,
    /// Upper bound (ms) for the reconnect backoff. Defaults to
    /// [`reconnect_delay`](Self::reconnect_delay) (a fixed delay).
    pub max_reconnect_delay: u64,
    /// Maximum redial attempts before giving up. `0` = unlimited (default).
    pub max_reconnect_attempts: usize,
    /// Send a keepalive `Ping` after this many ms of an idle connection; any
    /// traffic resets the idle window. `None` (default) disables it.
    pub keepalive_interval: Option<u64>,
    /// Capacity of the command channel, which buffers callers while the engine
    /// isn't draining it (a pending dial, the backoff between redials). A full
    /// channel evicts its oldest, visibly: a dropped `call` resolves
    /// [`RpcError::Internal`], a dropped `subscribe` ends its stream. Defaults to
    /// 256; clamped to `1..=MAX_COMMAND_QUEUE_CAP`.
    pub max_offline_queue: usize,
    /// Open every connection with a handshake, ahead of any caller command.
    /// What that means depends on the codec:
    ///
    /// - With a [`hello`](EnvelopeCodec::hello) request (AimX: `hello`), the
    ///   engine sends it once and goes straight on, **without waiting for the
    ///   reply**. The connection keeps frame order, so the server handles the
    ///   `hello` before anything sent after it; that is what tells a server on
    ///   a connection that never closes (a serial line) that a new client is
    ///   behind it, so it ends the previous client's session. The reply
    ///   arrives later and is dropped, except that a refused protocol version
    ///   is logged. A `hello` lost on the way is not resent.
    /// - Without one, the engine sends a `Ping` and waits for the `Pong`
    ///   before serving commands.
    pub sends_hello: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            reconnect: true,
            reconnect_delay: 200,
            max_reconnect_delay: 200,
            max_reconnect_attempts: 0,
            keepalive_interval: None,
            max_offline_queue: 256,
            sends_hello: false,
        }
    }
}

/// Correlation id of the handshake `hello` (see [`ClientConfig::sends_hello`]).
/// [`ClientHandle`] ids start at 1, so its reply never matches a caller's
/// call or subscription.
const HELLO_ID: u64 = 0;

/// Exponential backoff (ms) for the `attempt`-th redial (1-based), capped at
/// [`ClientConfig::max_reconnect_delay`].
fn backoff_delay(config: &ClientConfig, attempt: usize) -> u64 {
    let base = config.reconnect_delay;
    let cap = config.max_reconnect_delay.max(base);
    let shift = attempt.saturating_sub(1).min(16) as u32;
    base.saturating_mul(1u64 << shift).min(cap)
}

/// A cheap-clone handle to a running [`run_client`] engine — the caller-facing
/// RPC surface. Every method funnels a command to the engine, which owns the
/// pending-call map and the wire.
#[derive(Clone)]
pub struct ClientHandle {
    cmd_tx: Sender<ClientCmd>,
    /// Wakes the engine to reclaim abandoned calls by inspection, when the exact
    /// [`ClientCmd::CancelCall`] never reaches it (see [`Self::enqueue`]).
    ///
    /// A channel, not a flag: the caller learns of an eviction only from
    /// `force_send`'s return value, by which point the engine may already have
    /// looked, and a flag set then sits unread beside a parked engine. Capacity
    /// one suffices — the sweep is idempotent, so one queued signal covers any
    /// number of lost cancellations.
    prune_tx: Sender<()>,
    /// Correlation ids, allocated caller-side and shared by every clone.
    ///
    /// The *caller* numbers its requests so that abandoning one is race-free:
    /// the id is known before the command is queued, so the cancellation that
    /// follows on the same FIFO channel always names an entry the engine has
    /// already created (see [`CancelOnDrop`]). With engine-assigned ids there is
    /// an unavoidable window in which the caller gives up before learning the id
    /// and has nothing to cancel. Calls and subscriptions draw from this one
    /// counter because they share an id space on the wire.
    ///
    /// `usize` rather than `u64`: 64-bit atomics are not native on every
    /// supported MCU (thumbv7em), and `portable_atomic::AtomicU64` covers that
    /// only via its `critical-section` fallback — a dependency the engine will
    /// not impose on every no_std consumer for a counter. The width bounds the
    /// ids one process can issue — 2^32 on a 32-bit target, all of them
    /// JSON-safe — and [`ClientHandle::next_id`] refuses rather than wraps at
    /// the ceiling.
    next_id: Arc<AtomicUsize>,
    /// The codec's [`EnvelopeCodec::carries_inline_envelopes`], for
    /// [`pump_client`]'s build-time link check.
    inline_envelopes: bool,
}

/// Commands the [`ClientHandle`] funnels to the engine. The caller allocates the
/// correlation `id`; the engine owns the demux maps keyed by it.
enum ClientCmd {
    Call {
        id: u64,
        method: String,
        params: Payload,
        reply: oneshot::Sender<CallReply>,
    },
    /// The caller abandoned call `id` (its future was dropped). Frees the
    /// pending-call entry immediately, without waiting for a reply that may
    /// never come.
    CancelCall {
        id: u64,
    },
    Subscribe {
        id: u64,
        topic: String,
        events: Sender<Result<SubUpdate, RpcError>>,
    },
    Write {
        topic: String,
        payload: Payload,
    },
}

/// Frees a call's engine-side state if the caller stops awaiting it.
///
/// A dropped [`ClientHandle::call`] future takes the reply channel's receiver
/// with it, which the engine cannot observe: a peer that holds the connection
/// open but never answers produces neither a `Reply` nor a disconnect. The guard
/// turns that silent drop into a [`ClientCmd::CancelCall`], so reclamation is
/// exact and immediate — it needs no later call, no keepalive tick, and no scan
/// of the pending table.
///
/// Alone among the commands it does not evict to make room — that would trade a
/// caller's queued `Write` for bookkeeping. A full queue falls back to
/// [`ClientHandle::prune_tx`], which frees the same entry without naming it.
struct CancelOnDrop<'a> {
    handle: &'a ClientHandle,
    id: u64,
    /// Cleared once the call resolves — a completed call has already left the
    /// table, so cancelling it would only add channel traffic.
    armed: bool,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Closed: the engine is gone with its table. Full: the sweep stands
            // in for the exact id.
            if self
                .handle
                .cmd_tx
                .try_send(ClientCmd::CancelCall { id: self.id })
                .is_err()
            {
                self.handle.request_prune();
            }
        }
    }
}

impl ClientHandle {
    /// Funnel a command to the engine, never blocking: a full channel evicts its
    /// oldest, so the bound is the channel's own and holds across cloned handles
    /// without an admission lock. Dropping the evictee drops its reply sender,
    /// which is how the displaced caller learns. Fails once the engine has stopped.
    ///
    /// A [`ClientCmd::CancelCall`] is the one command that must not be evicted
    /// *silently*: its entry is already in the engine's table, and nothing else
    /// frees it before the connection ends. Eviction is FIFO-oldest and a
    /// cancellation is always younger than its own request, so an evicted one
    /// names an entry the engine already holds or never created — never one
    /// still in flight. It degrades to a [`Self::request_prune`] signal, which
    /// carries no id and so cannot itself be evicted.
    fn enqueue(&self, cmd: ClientCmd) -> Result<(), RpcError> {
        match self.cmd_tx.force_send(cmd) {
            Ok(Some(ClientCmd::CancelCall { .. })) => {
                self.request_prune();
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(_) => Err(RpcError::Internal),
        }
    }

    /// Ask the engine to reclaim every abandoned pending call. The sweep is
    /// idempotent, so a full channel already carries the request.
    fn request_prune(&self) {
        let _ = self.prune_tx.try_send(());
    }

    /// Allocate the next correlation id. Monotonic across reconnects, so an id
    /// is never reused by a later connection.
    ///
    /// Exhaustion is *refused*, not wrapped: `fetch_add` at the ceiling would
    /// restart at `0` and re-issue ids still held by live subscriptions, whose
    /// updates would then be demuxed into the wrong sink. Refusing turns that
    /// silent mis-route into a failed `call`/`subscribe`. The ceiling is
    /// `usize::MAX` — 2^32−1 on a 32-bit target, i.e. unreachable in practice
    /// (4 billion allocations on one connection lineage).
    ///
    /// [`RpcError::Internal`] doubles as "engine stopped" (see [`Self::enqueue`]);
    /// a dedicated variant would add wire surface (`session::aimx`'s codec maps
    /// every variant to a protocol string) for a case that cannot be recovered
    /// from anyway.
    fn next_id(&self) -> Result<u64, RpcError> {
        self.next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map(|id| id as u64)
            .map_err(|_| RpcError::Internal)
    }

    /// One-shot RPC: send a request and await its single reply. Returns
    /// [`RpcError::Internal`] if the engine has stopped or the connection drops
    /// before the reply arrives.
    ///
    /// Cancel-safe: dropping this future (a timeout losing the race, an aborted
    /// task) releases the engine's pending-call entry rather than leaving it
    /// until the connection ends.
    pub async fn call(
        &self,
        method: impl Into<String>,
        params: Payload,
    ) -> Result<Payload, RpcError> {
        let id = self.next_id()?;
        let (reply, rx) = oneshot::channel();
        self.enqueue(ClientCmd::Call {
            id,
            method: method.into(),
            params,
            reply,
        })?;
        // Armed across the await only: from here, any exit path that isn't the
        // reply itself tells the engine to drop the entry.
        let mut cancel = CancelOnDrop {
            handle: self,
            id,
            armed: true,
        };
        let reply = rx.await;
        cancel.armed = false;
        reply.map_err(|_| RpcError::Internal)?
    }

    /// Open a subscription; returns the stream of updates immediately (the engine
    /// sends the `Subscribe` request asynchronously). Dropping the stream stops
    /// local delivery. The stream is not re-subscribed on reconnect (see
    /// [`ClientConfig::reconnect`]) — re-call to resume.
    ///
    /// Each item is `Ok(`[`SubUpdate`]`)` for a delivered update, carrying the
    /// concrete record topic when the server tags it (wildcard subscriptions fan
    /// in many records under this one stream). A server rejection surfaces as a
    /// terminal `Err(`[`RpcError`]`)` item, letting the caller distinguish a
    /// denied subscription (do not retry) from a disconnect-shaped stream end
    /// (retry to resume).
    ///
    /// Late-join snapshots arrive first, the last of them flagged
    /// [`SubUpdate::snapshot_end`] with the burst's total loss in `skipped` (see
    /// [`Session::snapshots`](super::Session::snapshots)). Read that flag to
    /// close out the initial state, but drive the stream normally rather than
    /// looping *until* it: a subscription matching no records never emits one,
    /// and neither does one whose final snapshot frame was lost — the stream
    /// stays open and simply proceeds to live events.
    pub fn subscribe(
        &self,
        topic: impl Into<String>,
    ) -> Result<BoxStream<'static, Result<SubUpdate, RpcError>>, RpcError> {
        let (events, rx) =
            async_channel::bounded::<Result<SubUpdate, RpcError>>(SUBSCRIBE_CHANNEL_CAP);
        self.enqueue(ClientCmd::Subscribe {
            id: self.next_id()?,
            topic: topic.into(),
            events,
        })?;
        // The receiver is itself a `Stream<Item = SubUpdate>`.
        Ok(Box::pin(rx))
    }

    /// Fire-and-forget write to a remote topic (no reply).
    pub fn write(&self, topic: impl Into<String>, payload: Payload) -> Result<(), RpcError> {
        self.enqueue(ClientCmd::Write {
            topic: topic.into(),
            payload,
        })
    }
}

/// Build the client engine: returns a [`ClientHandle`] for issuing RPC and the
/// engine future to drive on the runner (spawn-free). The future runs until all
/// `ClientHandle` clones are dropped (graceful stop) — or, with
/// [`ClientConfig::reconnect`] off, until the first disconnect.
///
/// `ctx` is a [`RuntimeContext`](crate::RuntimeContext) (`db.runtime_ctx()`
/// inside an instance, `RuntimeContext::new(adapter)` outside one); the engine
/// uses its clock and sleep for the reconnect backoff and keepalive — the
/// *only* runtime dependency, so the rest of the engine is runtime-neutral.
pub fn run_client<D, C>(
    dialer: D,
    codec: C,
    config: ClientConfig,
    ctx: crate::RuntimeContext,
) -> (ClientHandle, BoxFut<'static, ()>)
where
    D: Dialer + 'static,
    C: EnvelopeCodec + 'static,
{
    let (cmd_tx, cmd_rx) =
        async_channel::bounded(config.max_offline_queue.clamp(1, MAX_COMMAND_QUEUE_CAP));
    // Capacity one: the signal is a request to sweep, not a queue of them.
    let (prune_tx, prune_rx) = async_channel::bounded(1);
    let handle = ClientHandle {
        cmd_tx,
        prune_tx,
        // Ids start at 1: `0` stays free as a "no correlation" sentinel for
        // protocols that want one.
        next_id: Arc::new(AtomicUsize::new(1)),
        inline_envelopes: codec.carries_inline_envelopes(),
    };
    let fut = Box::pin(client_loop(dialer, codec, config, cmd_rx, prune_rx, ctx));
    (handle, fut)
}

/// Why one connection's session ended — decides reconnect vs stop.
enum Ended {
    /// The connection dropped/errored; redial if configured.
    Disconnected,
    /// Every [`ClientHandle`] was dropped — stop the engine.
    HandlesDropped,
}

/// On engine exit, close and drain the command channel so buffered/in-flight
/// commands are dropped — each `ClientCmd::Call` drops its `reply` sender, so a
/// waiting [`ClientHandle::call`] resolves with [`RpcError::Internal`] instead of
/// hanging.
///
/// Needed because `async-channel` keeps buffered items alive while any `Sender`
/// exists, and dropping the `Receiver` only closes the queue without draining it.
struct DrainOnExit<'a>(&'a Receiver<ClientCmd>);

impl Drop for DrainOnExit<'_> {
    fn drop(&mut self) {
        self.0.close();
        while self.0.try_recv().is_ok() {}
    }
}

/// How an inbound update competes for room in its subscription's sink.
///
/// Every update stops one slot short of a full sink: that last slot belongs to
/// the terminal error [`end_sub`] may still have to deliver, so a slow
/// subscriber sees *why* its stream ended rather than a disconnect-shaped EOF.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// A live event: dropped when the sink is full.
    Event,
    /// A snapshot with more of its burst to come: dropped when the sink is
    /// full, and additionally stops one more slot short so
    /// [`Delivery::BurstEnd`] always fits.
    BurstBody,
    /// The burst's final snapshot: carries the burst's whole loss count and
    /// [`SubUpdate::snapshot_end`], delivered into the slot
    /// [`Delivery::BurstBody`] stopped short of.
    ///
    /// Only a `BurstBody` reserves that slot, so a single-snapshot burst rests
    /// on ordering instead: the sink predates the subscribe reply, and the
    /// server sends every snapshot before registering the event pump. A burst
    /// with no snapshots sends no `BurstEnd` at all.
    BurstEnd,
}

/// Route one subscription update (snapshot or event) to its sink, folding any
/// loss since the last delivery into its [`SubUpdate::skipped`].
///
/// Server `seq` counts true production across the *whole* subscription — the
/// late-join snapshot burst (`1..=N`) and then its events (`N + 1`…), with
/// server-side buffer lag folded in via `+= skipped + 1` and funnel drops
/// bumping the counter before dropping. So one delta against the last delivered
/// seq captures every loss point, plus any prior local full-channel drop that
/// left `last_seq` behind.
///
/// A gap only *reaches* the subscriber on an update that is actually delivered,
/// which is why the burst reserves a slot for its final snapshot: a burst
/// truncated at the tail would otherwise stay silent until some later event
/// closed the sequence, and a static subscription may never produce one.
///
/// `seq` is peer-supplied and unvalidated, so neither its monotonicity nor its
/// range can be assumed: the arithmetic saturates, and the cursor only ever
/// advances. A repeated or reordered frame is still delivered — dropping it
/// would discard data over a number — but it cannot rewind the cursor into
/// reporting a gap that never happened.
///
/// `update` carries the frame's topic, value and stamp; `deliver` fills in the
/// loss accounting (`skipped`, `snapshot_end`).
fn deliver(
    subs: &mut HashMap<String, Sender<Result<SubUpdate, RpcError>>>,
    last_seq: &mut HashMap<String, u64>,
    sub: &str,
    seq: u64,
    mut update: SubUpdate,
    mode: Delivery,
) {
    let prev = last_seq.get(sub).copied().unwrap_or(0);
    let skipped = seq.saturating_sub(prev.saturating_add(1));
    // `None` here is a late update for a dropped sub — ignore.
    if let Some(tx) = subs.get(sub) {
        // Hold the last slot back for a terminal error, and a burst body one
        // more for the burst's final snapshot. Only this loop ever sends, so
        // `len()` can only *fall* before the `try_send` below — a slot seen
        // free here stays free, making those deliveries infallible rather than
        // merely likely.
        let reserved = if mode == Delivery::BurstBody { 2 } else { 1 };
        if tx.capacity().is_some_and(|cap| tx.len() + reserved >= cap) {
            // Slow consumer: drop this update but leave `last_seq` so the
            // shortfall folds into the next delivered update's `skipped`.
            return;
        }
        update.skipped = skipped;
        update.snapshot_end = mode == Delivery::BurstEnd;
        match tx.try_send(Ok(update)) {
            // Delivered — advance the per-sub cursor, never rewind it.
            Ok(()) => {
                last_seq.insert(sub.to_string(), seq.max(prev));
            }
            // Full despite the check above: only a channel without a capacity
            // gets here. Same as a slow consumer.
            Err(e) if e.is_full() => {}
            // Receiver gone — the sub was dropped; prune it.
            Err(_) => {
                subs.remove(sub);
                last_seq.remove(sub);
            }
        }
    }
}

/// End a subscription with a terminal `err` and drop its sink.
///
/// The error lands in the slot [`deliver`] never fills, so it reaches the
/// subscriber even behind a full queue of buffered updates.
fn end_sub(
    subs: &mut HashMap<String, Sender<Result<SubUpdate, RpcError>>>,
    last_seq: &mut HashMap<String, u64>,
    sub: &str,
    err: RpcError,
) {
    if let Some(tx) = subs.remove(sub) {
        let _ = tx.try_send(Err(err));
        last_seq.remove(sub);
    }
}

/// What [`drive_connection`]'s `select_biased!` decided this iteration — extracted
/// so the work runs after the arm futures' borrow of `conn` releases.
enum ClientStep {
    /// A frame (or close/error) arrived from the server.
    Inbound(super::TransportResult<Option<Vec<u8>>>),
    /// The keepalive timer fired — send a `Ping`.
    Keepalive,
    /// A cancellation was lost on the way here — reclaim abandoned calls by
    /// inspecting the pending table instead of by id.
    Prune,
    /// A caller command (or `None` = all handles dropped).
    Cmd(Option<ClientCmd>),
}

async fn client_loop<D, C>(
    dialer: D,
    codec: C,
    config: ClientConfig,
    cmd_rx: Receiver<ClientCmd>,
    prune_rx: Receiver<()>,
    ctx: crate::RuntimeContext,
) where
    D: Dialer,
    C: EnvelopeCodec,
{
    // Whenever the engine returns, fail any buffered/in-flight calls (see guard).
    let _drain = DrainOnExit(&cmd_rx);
    // Consecutive failed attempts; drives backoff and the attempt cap.
    let mut attempt: usize = 0;
    loop {
        let conn = match dialer.connect().await {
            Ok(conn) => {
                attempt = 0;
                conn
            }
            Err(e) => {
                report(SessionEvent::DialFailed { error: &e });
                // `Closed` is terminal — the dialer signals it will never succeed
                // again (e.g. the caller stopped the bridge), so retrying would
                // spin a permanently-failing redial forever. Only transient
                // failures (`Io`, `Busy`) earn a backoff+retry. Other transports' dialers
                // map connect failures to `Io`, never `Closed`, so this is safe.
                if e == TransportError::Closed {
                    return;
                }
                match reconnect_after(&mut attempt, &config, &ctx).await {
                    true => continue,
                    false => return,
                }
            }
        };

        match drive_connection(conn, &codec, &cmd_rx, &prune_rx, &config, &ctx).await {
            Ended::HandlesDropped => return,
            Ended::Disconnected => match reconnect_after(&mut attempt, &config, &ctx).await {
                true => continue,
                false => return,
            },
        }
    }
}

/// Decide whether to redial: honor `reconnect`, the attempt cap, and the
/// exponential backoff sleep (via the runtime clock). Returns `true` to retry,
/// `false` to stop the engine.
async fn reconnect_after(
    attempt: &mut usize,
    config: &ClientConfig,
    ctx: &crate::RuntimeContext,
) -> bool {
    if !config.reconnect {
        return false;
    }
    *attempt += 1;
    if config.max_reconnect_attempts != 0 && *attempt >= config.max_reconnect_attempts {
        report(SessionEvent::ReconnectExhausted {
            attempts: config.max_reconnect_attempts,
        });
        return false;
    }
    ctx.time()
        .sleep(core::time::Duration::from_millis(backoff_delay(
            config, *attempt,
        )))
        .await;
    true
}

/// What a caller awaiting [`ClientHandle::call`] eventually receives.
type CallReply = Result<Payload, RpcError>;

/// The engine's in-flight call table: correlation `id` → the caller's reply
/// channel.
type PendingCalls = HashMap<u64, oneshot::Sender<CallReply>>;

/// Reclaim every entry whose caller stopped waiting — the fallback for a
/// [`ClientCmd::CancelCall`] the bounded command queue dropped.
///
/// A dropped receiver is exactly what an abandoned [`ClientHandle::call`] leaves
/// behind, so this needs no id and cannot miss an earlier loss. `O(N)`, but
/// reached only from the eviction path; a surviving cancellation stays `O(1)`.
fn sweep_abandoned(pending: &mut PendingCalls) {
    pending.retain(|_, reply| !reply.is_canceled());
}

/// Drive one dialed [`Connection`]: optional handshake, then `biased` demux of
/// server frames (resolve `Reply` by `id`, route `Event`/`Snapshot` to their
/// subscription channels) interleaved with caller commands. Pending state is
/// per-connection: a disconnect fails outstanding calls (their `oneshot`
/// senders drop → callers see [`RpcError::Internal`]). Calls the *caller*
/// abandoned arrive as [`ClientCmd::CancelCall`] and are removed by id in O(1),
/// so an unanswering peer that keeps the link open can't grow the table — with
/// no dependence on later traffic or on the keepalive being enabled. One the
/// command queue dropped instead arrives on `prune_rx` as a request to sweep.
/// `pending` is per-connection, so a signal raised while dialing lands on a
/// fresh table — a no-op, since the disconnect already dropped every entry.
async fn drive_connection<C>(
    mut conn: Box<dyn Connection>,
    codec: &C,
    cmd_rx: &Receiver<ClientCmd>,
    prune_rx: &Receiver<()>,
    config: &ClientConfig,
    ctx: &crate::RuntimeContext,
) -> Ended
where
    C: EnvelopeCodec + ?Sized,
{
    let mut pending: PendingCalls = HashMap::new();
    // sub-id → event sink. The sub-id is `id.to_string()` of the opening
    // request, matching the server's derivation so `Event.sub` routes back.
    let mut subs: HashMap<String, Sender<Result<SubUpdate, RpcError>>> = HashMap::new();
    // sub-id → last *delivered* wire `seq`. Baseline is 0, so the server's first
    // seq (`1`) is the expected first delivery; any shortfall is loss. Not
    // advanced on a locally-dropped event, so a full-channel drop also surfaces
    // as a gap on the next delivery.
    let mut last_seq: HashMap<String, u64> = HashMap::new();
    let mut out = Vec::new();
    let keepalive_ms = config.keepalive_interval;
    // Keepalive is deadline-based: activity only records a timestamp (one dyn
    // clock read, no allocation) and the boxed `ctx.time().sleep` stays armed for a
    // full idle window — re-created roughly once per interval, not on every
    // processed frame (`dyn RuntimeOps::sleep` heap-allocates its future).
    let mut last_activity = ctx.time().now().as_nanos();
    let mut keepalive_timer = keepalive_ms.map(|ms| {
        ctx.time()
            .sleep(core::time::Duration::from_millis(ms))
            .fuse()
    });

    // Handshake-as-caller, before serving commands: the codec's hello, which
    // also starts a fresh session on the server, or else a Ping/Pong.
    if config.sends_hello {
        if let Some((method, params)) = codec.hello() {
            // Sent once, not awaited: frame order puts it ahead of every
            // command, and the demux loop below drops its reply.
            out.clear();
            let hello = Inbound::Request {
                id: HELLO_ID,
                method: method.into(),
                params,
            };
            if codec.encode_inbound(hello, &mut out).is_err() || conn.send(&out).await.is_err() {
                return Ended::Disconnected;
            }
        } else {
            out.clear();
            if codec.encode_inbound(Inbound::Ping, &mut out).is_err()
                || conn.send(&out).await.is_err()
            {
                return Ended::Disconnected;
            }
            match conn.recv().await {
                Ok(Some(frame)) => match codec.decode_outbound(&frame) {
                    Ok(Outbound::Pong) => {}
                    _ => return Ended::Disconnected,
                },
                _ => return Ended::Disconnected,
            }
        }
    }

    loop {
        // Biased toward the server read. The select only decides the next step.
        let step = {
            let mut recv = conn.recv().fuse();
            // The armed idle timer (see above); with no interval it parks on
            // `pending()` forever. `Either` re-borrows the persistent timer,
            // so this is allocation-free per iteration.
            let mut keepalive = match keepalive_timer.as_mut() {
                Some(timer) => futures_util::future::Either::Left(timer),
                None => futures_util::future::Either::Right(futures_util::future::pending::<()>()),
            };
            // `recv()` is `!Unpin`, so pin it for the arm.
            let mut cmd = core::pin::pin!(cmd_rx.recv().fuse());
            let mut prune = core::pin::pin!(async {
                if prune_rx.recv().await.is_err() {
                    // Handles all dropped — the `cmd` arm reports that. A closed
                    // channel resolves immediately and this arm outranks `cmd`,
                    // so park rather than spin ahead of the shutdown.
                    futures_util::future::pending::<()>().await;
                }
            }
            .fuse());
            select_biased! {
                // ---- inbound from server: Reply / Event / Snapshot / Pong --
                r = recv => ClientStep::Inbound(r),
                // ---- keepalive: the idle timer fired ------------------------
                _ = keepalive => ClientStep::Keepalive,
                // ---- a lost cancellation asked for a sweep ------------------
                // Above `cmd`: a full command queue is what raises the signal,
                // so a lower arm would be starved by its own trigger.
                _ = prune => ClientStep::Prune,
                // ---- caller commands from ClientHandle ---------------------
                // `recv()` errors only when every `ClientHandle` is dropped → `None`.
                c = cmd => ClientStep::Cmd(c.ok()),
            }
        };

        // Frames and commands are link activity; only a genuinely idle link
        // needs a Ping. A sweep is local bookkeeping with no wire traffic, so a
        // busy-cancelling caller must not suppress the keepalive with it.
        if !matches!(step, ClientStep::Keepalive | ClientStep::Prune) {
            last_activity = ctx.time().now().as_nanos();
        }

        match step {
            ClientStep::Inbound(recv) => {
                let frame = match recv {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => return Ended::Disconnected,
                };
                match codec.decode_outbound(&frame) {
                    // The reply to the handshake hello, which nobody awaits.
                    // Only a refusal is worth a word: the server keeps
                    // serving, but on a protocol this client may not speak.
                    Ok(Outbound::Reply {
                        id: HELLO_ID,
                        result,
                    }) => {
                        if result == Err(RpcError::VersionMismatch) {
                            report(SessionEvent::HelloRefused);
                        }
                    }
                    Ok(Outbound::Reply { id, result }) => {
                        if let Some(tx) = pending.remove(&id) {
                            let _ = tx.send(result);
                        } else if let Err(err) = result {
                            // A subscribe is acked implicitly by its events; the
                            // server replies only on failure, carrying the subscribe
                            // `id` (never a pending call). Surface the rejection as a
                            // terminal error item before dropping the sink, so the
                            // subscriber can tell "denied" from a disconnect-shaped
                            // stream end instead of re-subscribing forever.
                            end_sub(&mut subs, &mut last_seq, &id.to_string(), err);
                        }
                    }
                    Ok(Outbound::Event {
                        sub,
                        seq,
                        topic,
                        ts,
                        data,
                    }) => deliver(
                        &mut subs,
                        &mut last_seq,
                        sub,
                        seq,
                        match topic {
                            Some(topic) => SubUpdate::tagged(Arc::from(topic), data, ts),
                            None => SubUpdate::new(data, ts),
                        },
                        Delivery::Event,
                    ),
                    // Snapshots ride the same accounting as events (they share
                    // the subscription's `seq` space), so a late-join burst that
                    // overruns a slow consumer's sink is reported as `skipped`
                    // instead of vanishing. The burst's final snapshot is
                    // delivered unconditionally, so "one snapshot per matched
                    // record" is checkable the moment the burst ends — no live
                    // event required.
                    Ok(Outbound::Snapshot {
                        sub,
                        seq,
                        last,
                        topic,
                        ts,
                        data,
                    }) => deliver(
                        &mut subs,
                        &mut last_seq,
                        sub,
                        seq,
                        SubUpdate::tagged(Arc::from(topic), data, ts),
                        if last {
                            Delivery::BurstEnd
                        } else {
                            Delivery::BurstBody
                        },
                    ),
                    Ok(Outbound::Pong) => {}
                    // Explicit subscribe ack — informational; the sink already exists.
                    Ok(Outbound::Subscribed { .. }) => {}
                    // A value without its stamp breaks the protocol for that
                    // subscription: end it with an error the subscriber sees,
                    // as a refusal does, instead of dropping the frame unseen.
                    Err(CodecError::MissingStamp { sub }) => {
                        end_sub(&mut subs, &mut last_seq, &sub, RpcError::Protocol)
                    }
                    Err(_e) => continue, // skip a malformed frame, keep the connection
                }
            }

            // A cancellation was dropped by the bounded command queue, so the
            // engine can't be told *which* entry died — but it can see it.
            ClientStep::Prune => sweep_abandoned(&mut pending),

            ClientStep::Keepalive => {
                // `keepalive_timer` is `Some` whenever this step fires.
                let interval_ms = keepalive_ms.unwrap_or(0);
                let idle_ms = ctx.time().now().as_nanos().saturating_sub(last_activity) / 1_000_000;
                if idle_ms >= interval_ms {
                    // Genuinely idle for a full window: ping and re-arm.
                    out.clear();
                    if codec.encode_inbound(Inbound::Ping, &mut out).is_ok()
                        && conn.send(&out).await.is_err()
                    {
                        return Ended::Disconnected;
                    }
                    last_activity = ctx.time().now().as_nanos();
                    keepalive_timer = Some(
                        ctx.time()
                            .sleep(core::time::Duration::from_millis(interval_ms))
                            .fuse(),
                    );
                } else {
                    // Activity happened while the timer was armed: no ping,
                    // re-arm for the remainder of the idle window.
                    keepalive_timer = Some(
                        ctx.time()
                            .sleep(core::time::Duration::from_millis(interval_ms - idle_ms))
                            .fuse(),
                    );
                }
            }

            ClientStep::Cmd(cmd) => {
                let cmd = match cmd {
                    Some(cmd) => cmd,
                    None => return Ended::HandlesDropped, // all handles dropped
                };
                match cmd {
                    ClientCmd::Call {
                        id,
                        method,
                        params,
                        reply,
                    } => {
                        // The caller may have given up while the command sat in
                        // the queue — don't spend a wire request on a reply
                        // nobody is waiting for. Its `CancelCall` is already
                        // queued behind this one and will find nothing to free.
                        if reply.is_canceled() {
                            continue;
                        }
                        pending.insert(id, reply);
                        out.clear();
                        let sent = codec
                            .encode_inbound(Inbound::Request { id, method, params }, &mut out)
                            .is_ok()
                            && conn.send(&out).await.is_ok();
                        if !sent {
                            if let Some(tx) = pending.remove(&id) {
                                let _ = tx.send(Err(RpcError::Internal));
                            }
                            return Ended::Disconnected;
                        }
                    }
                    // The caller abandoned this call: drop its reply channel now
                    // rather than holding it until a reply that may never arrive
                    // (or the disconnect). A resolved or never-dispatched call
                    // is simply absent — removing it is a no-op.
                    ClientCmd::CancelCall { id } => {
                        pending.remove(&id);
                    }
                    ClientCmd::Subscribe { id, topic, events } => {
                        subs.insert(id.to_string(), events);
                        out.clear();
                        let sent = codec
                            .encode_inbound(Inbound::Subscribe { id, topic }, &mut out)
                            .is_ok()
                            && conn.send(&out).await.is_ok();
                        if !sent {
                            return Ended::Disconnected;
                        }
                    }
                    ClientCmd::Write { topic, payload } => {
                        out.clear();
                        let sent = codec
                            .encode_inbound(Inbound::Write { topic, payload }, &mut out)
                            .is_ok()
                            && conn.send(&out).await.is_ok();
                        if !sent {
                            return Ended::Disconnected;
                        }
                    }
                }
            }
        }
    }
}

/// Mirror records between a local [`AimDb`] and a remote peer over a running
/// [`run_client`] engine — the connector-link half of the client capability.
///
/// For the given connector `scheme` (e.g. `"aimx"`):
/// - **outbound** routes ([`OutboundRoutes`](crate::OutboundRoutes)) stream
///   local record updates to the remote via [`ClientHandle::write`], all from
///   one task;
/// - **inbound** routes (`inbound`, an [`InboundDispatch`](crate::InboundDispatch)) subscribe to the remote and
///   produce each update into the local record through the producer/arbiter path
///   — single-writer-per-key stays intact (a mirrored-in record is produced
///   through its inbound producer, never a direct co-writer). Mirroring is
///   latest-state and best-effort — a gap the server reports
///   ([`SubUpdate::skipped`]) is logged and stepped over, never backfilled.
///
/// Returns spawn-free futures for the runner to drive (mirroring the
/// `ConnectorBuilder::build -> Vec<BoxFuture>` spine): one for every outbound
/// route together, when there are any, and one per inbound subscription. They
/// drive the **same** engine as [`run_client`], never a second one. Fails when
/// an outbound link cannot be served (see
/// [`OutboundRoutes::new`](crate::OutboundRoutes::new)).
///
/// [`ClientHandle::write`] enqueues onto the engine's bounded command queue
/// and displaces the oldest command when it is full, so during an outage that
/// queue, not the record buffer, decides what survives.
///
/// Reconnect caveat: inbound pumps subscribe once and are not replayed across a
/// reconnect (see [`ClientConfig::reconnect`]); outbound mirroring is unaffected.
pub fn pump_client(
    db: &AimDb,
    scheme: &str,
    inbound: crate::InboundDispatch,
    handle: &ClientHandle,
) -> crate::DbResult<Vec<BoxFut<'static, ()>>> {
    let mut pumps: Vec<BoxFut<'static, ()>> = Vec::new();

    // --- outbound: local record updates -> remote `write`, one task --------
    // Skips, lag and closed routes are handled and counted inside
    // `OutboundRoutes`; `next` ends once every route has closed.
    let mut outbound = crate::OutboundRoutes::new(db, scheme)?;

    // The codec cannot carry an inline envelope header: refuse such a link
    // here, not as an encode failure that drops the session for every link.
    if !handle.inline_envelopes {
        refuse_inline_envelopes(scheme, &outbound, &inbound)?;
    }
    if !outbound.routes().is_empty() {
        let handle = handle.clone();
        pumps.push(Box::pin(async move {
            while let Some(msg) = outbound.next().await {
                let payload = match msg.payload {
                    crate::OutboundPayload::Borrowed(bytes) => Payload::from(bytes),
                    crate::OutboundPayload::Owned(bytes) => Payload::from(bytes),
                };
                if handle.write(msg.topic, payload).is_err() {
                    break; // engine stopped — all handles dropped
                }
            }
        }));
    }

    // --- inbound: remote events -> local producer --------------------------
    // The dispatcher applies each route's deserializer and produces the
    // value; one subscription per unique remote topic feeds it.
    for id in inbound.subscriptions() {
        pumps.push(Box::pin(inbound_pump(handle.clone(), inbound.clone(), id)));
    }

    Ok(pumps)
}

/// Fails if a link of `scheme` frames its payloads in an inline envelope
/// (it is recorded with
/// [`WIRE_ENVELOPE_KEY`](crate::connector::WIRE_ENVELOPE_KEY)), naming the
/// first such link, outbound before inbound.
///
/// For a connector whose [`EnvelopeCodec`] reports
/// [`carries_inline_envelopes`](EnvelopeCodec::carries_inline_envelopes) as
/// `false`, such as one speaking AimX: call it from `build()`, once the routes
/// are collected. [`pump_client`] already does for client connectors.
pub fn refuse_inline_envelopes(
    scheme: &str,
    outbound: &crate::OutboundRoutes,
    inbound: &crate::InboundDispatch,
) -> crate::DbResult<()> {
    let inline = |config: &crate::transport::ConnectorConfig| {
        config
            .protocol_options
            .iter()
            .any(|(key, _)| key == crate::connector::WIRE_ENVELOPE_KEY)
    };
    let refused = outbound
        .routes()
        .iter()
        .find(|route| inline(&route.config))
        .map(|route| route.default_topic.clone())
        .or_else(|| {
            inbound
                .routes()
                .iter()
                .find(|route| inline(&route.config))
                .map(|route| route.topic.clone())
        });
    match refused {
        None => Ok(()),
        Some(topic) => Err(crate::DbError::InvalidOperation {
            operation: alloc::format!("link {scheme}://{topic}"),
            reason: "this connector's codec carries bare payloads only; \
                     an inline envelope cannot be sent or received on it"
                .into(),
        }),
    }
}

/// Drive one inbound mirror: subscribe to remote topic `id` and produce every
/// update into the local record through the [`InboundDispatch`](crate::InboundDispatch).
///
/// **Loss contract: a mirror is latest-state, best-effort.** A mirrored record
/// answers "what is the current value", not "what was every value" — so a gap
/// reported by the server ([`SubUpdate::skipped`], raised when a subscription's
/// sink overran) is *logged and stepped over*, never backfilled. The mirror does
/// not resync, and a consumer must not read a mirrored record as a complete
/// history. Callers that need gap-aware delivery subscribe through
/// `AimxConnection` (`aimdb-client`), which surfaces `skipped` per update;
/// [`ClientHandle::subscribe`] carries the same metadata for anyone riding the
/// handle directly.
async fn inbound_pump(handle: ClientHandle, inbound: crate::InboundDispatch, id: Arc<str>) {
    let mut stream = match handle.subscribe(id.as_ref()) {
        Ok(s) => s,
        Err(_e) => return,
    };
    while let Some(update) = stream.next().await {
        match update {
            Ok(update) => {
                // Per the loss contract above: surface the hole, keep mirroring.
                if update.skipped > 0 {
                    report(SessionEvent::MirrorGap {
                        topic: id.as_ref(),
                        skipped: update.skipped,
                    });
                }
                inbound.dispatch(id.as_ref(), &update.data);
            }
            // Server rejected the subscription — terminal, not replayed.
            Err(_e) => break,
        }
    }
}

#[cfg(test)]
mod tests;
