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
//! backoff + keepalive), via the adapter's dyn-safe `RuntimeOps` clock; everything else is
//! `futures` channels. The demux loop uses the same **extract-then-act** shape as
//! the server (compute a [`ClientStep`], then act once the arm borrows release).

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
    BoxFut, BoxStream, Connection, Dialer, EnvelopeCodec, Inbound, Outbound, Payload, RpcError,
    SubUpdate, TransportError,
};
use crate::router::RouterBuilder;
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
    /// Send a Ping handshake on connect and await the Pong before serving caller
    /// commands. A real protocol swaps Ping/Pong for its Hello.
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
/// `clock` is the adapter's dyn-safe [`RuntimeOps`](crate::executor::RuntimeOps)
/// (e.g. `db.runtime_ops()`); the engine uses it for the reconnect backoff and
/// keepalive — the *only* runtime dependency, so the rest of the engine is
/// runtime-neutral.
pub fn run_client<D, C>(
    dialer: D,
    codec: C,
    config: ClientConfig,
    clock: Arc<dyn crate::executor::RuntimeOps>,
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
    };
    let fut = Box::pin(client_loop(dialer, codec, config, cmd_rx, prune_rx, clock));
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// A live event: dropped when the sink is full.
    Event,
    /// A snapshot with more of its burst to come: dropped when the sink is
    /// full, and additionally stops one slot short so [`Delivery::BurstEnd`]
    /// always fits.
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
fn deliver(
    subs: &mut HashMap<String, Sender<Result<SubUpdate, RpcError>>>,
    last_seq: &mut HashMap<String, u64>,
    sub: &str,
    seq: u64,
    topic: Option<Arc<str>>,
    data: Payload,
    mode: Delivery,
) {
    let prev = last_seq.get(sub).copied().unwrap_or(0);
    let skipped = seq.saturating_sub(prev.saturating_add(1));
    // `None` here is a late update for a dropped sub — ignore.
    if let Some(tx) = subs.get(sub) {
        // Hold the last slot back for the burst's final snapshot. Only this
        // loop ever sends, so `len()` can only *fall* before the `try_send`
        // below — a slot seen free here stays free, making that final delivery
        // infallible rather than merely likely.
        if mode == Delivery::BurstBody && tx.capacity().is_some_and(|cap| tx.len() + 1 >= cap) {
            return; // folds into the next delivered update's `skipped`
        }
        let update = SubUpdate {
            topic,
            data,
            skipped,
            snapshot_end: mode == Delivery::BurstEnd,
        };
        match tx.try_send(Ok(update)) {
            // Delivered — advance the per-sub cursor, never rewind it.
            Ok(()) => {
                last_seq.insert(sub.to_string(), seq.max(prev));
            }
            // Slow consumer: drop this update but leave `last_seq` so the
            // shortfall folds into the next delivered update's `skipped`.
            Err(e) if e.is_full() => {}
            // Receiver gone — the sub was dropped; prune it.
            Err(_) => {
                subs.remove(sub);
                last_seq.remove(sub);
            }
        }
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
    clock: Arc<dyn crate::executor::RuntimeOps>,
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
                log_warn!("client dial failed: {:?}", e);
                // `Closed` is terminal — the dialer signals it will never succeed
                // again (e.g. the caller stopped the bridge), so retrying would
                // spin a permanently-failing redial forever. Only transient
                // failures (`Io`) earn a backoff+retry. Other transports' dialers
                // map connect failures to `Io`, never `Closed`, so this is safe.
                if e == TransportError::Closed {
                    return;
                }
                match reconnect_after(&mut attempt, &config, &*clock).await {
                    true => continue,
                    false => return,
                }
            }
        };

        match drive_connection(conn, &codec, &cmd_rx, &prune_rx, &config, &*clock).await {
            Ended::HandlesDropped => return,
            Ended::Disconnected => match reconnect_after(&mut attempt, &config, &*clock).await {
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
    clock: &dyn crate::executor::RuntimeOps,
) -> bool {
    if !config.reconnect {
        return false;
    }
    *attempt += 1;
    if config.max_reconnect_attempts != 0 && *attempt >= config.max_reconnect_attempts {
        log_warn!(
            "client giving up after {} reconnect attempts",
            config.max_reconnect_attempts
        );
        return false;
    }
    clock
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
    clock: &dyn crate::executor::RuntimeOps,
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
    // clock read, no allocation) and the boxed `clock.sleep` stays armed for a
    // full idle window — re-created roughly once per interval, not on every
    // processed frame (`dyn RuntimeOps::sleep` heap-allocates its future).
    let mut last_activity = clock.now_nanos();
    let mut keepalive_timer =
        keepalive_ms.map(|ms| clock.sleep(core::time::Duration::from_millis(ms)).fuse());

    // Handshake-as-caller: prove the link with Ping/Pong before serving commands.
    if config.sends_hello {
        out.clear();
        if codec.encode_inbound(Inbound::Ping, &mut out).is_err() || conn.send(&out).await.is_err()
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
            last_activity = clock.now_nanos();
        }

        match step {
            ClientStep::Inbound(recv) => {
                let frame = match recv {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => return Ended::Disconnected,
                };
                match codec.decode_outbound(&frame) {
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
                            if let Some(tx) = subs.remove(&id.to_string()) {
                                let _ = tx.try_send(Err(err));
                                last_seq.remove(&id.to_string());
                            }
                        }
                    }
                    Ok(Outbound::Event {
                        sub,
                        seq,
                        topic,
                        data,
                    }) => deliver(
                        &mut subs,
                        &mut last_seq,
                        sub,
                        seq,
                        topic.map(Arc::from),
                        data,
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
                        data,
                    }) => deliver(
                        &mut subs,
                        &mut last_seq,
                        sub,
                        seq,
                        Some(Arc::from(topic)),
                        data,
                        if last {
                            Delivery::BurstEnd
                        } else {
                            Delivery::BurstBody
                        },
                    ),
                    Ok(Outbound::Pong) => {}
                    // Explicit subscribe ack — informational; the sink already exists.
                    Ok(Outbound::Subscribed { .. }) => {}
                    Err(_e) => continue, // skip a malformed frame, keep the connection
                }
            }

            // A cancellation was dropped by the bounded command queue, so the
            // engine can't be told *which* entry died — but it can see it.
            ClientStep::Prune => sweep_abandoned(&mut pending),

            ClientStep::Keepalive => {
                // `keepalive_timer` is `Some` whenever this step fires.
                let interval_ms = keepalive_ms.unwrap_or(0);
                let idle_ms = clock.now_nanos().saturating_sub(last_activity) / 1_000_000;
                if idle_ms >= interval_ms {
                    // Genuinely idle for a full window: ping and re-arm.
                    out.clear();
                    if codec.encode_inbound(Inbound::Ping, &mut out).is_ok()
                        && conn.send(&out).await.is_err()
                    {
                        return Ended::Disconnected;
                    }
                    last_activity = clock.now_nanos();
                    keepalive_timer = Some(
                        clock
                            .sleep(core::time::Duration::from_millis(interval_ms))
                            .fuse(),
                    );
                } else {
                    // Activity happened while the timer was armed: no ping,
                    // re-arm for the remainder of the idle window.
                    keepalive_timer = Some(
                        clock
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
/// - **outbound** routes (`db.collect_outbound_routes`) stream local record
///   updates to the remote via [`ClientHandle::write`];
/// - **inbound** routes (`db.collect_inbound_routes`) subscribe to the remote and
///   produce each update into the local record through the producer/arbiter path
///   — single-writer-per-key stays intact (a mirrored-in record is produced
///   through its inbound producer, never a direct co-writer). Mirroring is
///   latest-state and best-effort — a gap the server reports
///   ([`SubUpdate::skipped`]) is logged and stepped over, never backfilled.
///
/// Returns one spawn-free pump future per route for the runner to drive
/// (mirroring the `ConnectorBuilder::build -> Vec<BoxFuture>` spine); it drives
/// the **same** engine as [`run_client`], never a second one.
///
/// Reconnect caveat: inbound pumps subscribe once and are not replayed across a
/// reconnect (see [`ClientConfig::reconnect`]); outbound mirroring is unaffected.
pub fn pump_client(db: &AimDb, scheme: &str, handle: &ClientHandle) -> Vec<BoxFut<'static, ()>> {
    // The runtime context for context-aware (de)serializers.
    let ctx = db.runtime_ctx();
    let mut pumps: Vec<BoxFut<'static, ()>> = Vec::new();

    // --- outbound: local record updates -> remote `write` ------------------
    for crate::OutboundRoute {
        topic: destination,
        source,
        ..
    } in db.collect_outbound_routes(scheme)
    {
        let handle = handle.clone();
        let ctx = ctx.clone();
        pumps.push(Box::pin(async move {
            let mut reader = source.subscribe();
            loop {
                // The fused reader yields destination + serialized payload
                // (serialize failures are logged and skipped inside it).
                let msg = match reader.recv(&ctx).await {
                    Ok(m) => m,
                    // Lagged (ring overflow) — skip the gap, keep mirroring.
                    Err(crate::DbError::BufferLagged { .. }) => continue,
                    // Buffer closed — the record is gone; end this mirror.
                    Err(_) => break,
                };
                // Dynamic destination (topic provider) or the static link target.
                let dest = msg.dest.unwrap_or_else(|| destination.clone());
                if handle
                    .write(dest, Payload::from(msg.payload.as_slice()))
                    .is_err()
                {
                    break; // engine stopped — all handles dropped
                }
            }
        }));
    }

    // --- inbound: remote events -> local producer (via the Router) ---------
    // The Router applies each route's deserializer and produces the value; one
    // subscription per unique remote topic feeds it.
    let router = Arc::new(RouterBuilder::from_routes(db.collect_inbound_routes(scheme)).build());
    for id in router.resource_ids() {
        pumps.push(Box::pin(inbound_pump(
            handle.clone(),
            router.clone(),
            id,
            ctx.clone(),
        )));
    }

    pumps
}

/// Drive one inbound mirror: subscribe to remote topic `id` and produce every
/// update into the local record through the [`Router`](crate::router::Router).
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
async fn inbound_pump(
    handle: ClientHandle,
    router: Arc<crate::router::Router>,
    id: Arc<str>,
    ctx: crate::RuntimeContext,
) {
    let mut stream = match handle.subscribe(id.as_ref()) {
        Ok(s) => s,
        Err(_e) => return,
    };
    while let Some(update) = stream.next().await {
        match update {
            Ok(update) => {
                // Per the loss contract above: surface the hole, keep mirroring.
                if update.skipped > 0 {
                    log_warn!(
                        "mirror gap on '{}': {} update(s) lost (latest-state, no resync)",
                        id.as_ref(),
                        update.skipped
                    );

                    #[cfg(feature = "defmt")]
                    defmt::warn!(
                        "mirror gap on '{}': {} update(s) lost (latest-state, no resync)",
                        id.as_ref(),
                        update.skipped
                    );
                }
                let _ = router.route(id.as_ref(), &update.data, &ctx);
            }
            // Server rejected the subscription — terminal, not replayed.
            Err(_e) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{CodecError, PeerInfo, TransportResult};
    use core::future::pending;

    /// Sink connection: swallows every frame and never yields one, i.e. a peer
    /// that keeps the link open and answers nothing.
    #[derive(Default)]
    struct SilentPeer {
        peer: PeerInfo,
    }

    impl Connection for SilentPeer {
        fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
            Box::pin(pending())
        }
        fn send<'a>(&'a mut self, _frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
            Box::pin(async { Ok(()) })
        }
        fn peer(&self) -> &PeerInfo {
            &self.peer
        }
    }

    /// Frames are never inspected in these tests — only the pending-call
    /// bookkeeping is.
    struct NullCodec;

    impl EnvelopeCodec for NullCodec {
        fn decode(&self, _frame: &[u8]) -> Result<Inbound, CodecError> {
            Err(CodecError::Malformed)
        }
        fn encode(&self, _msg: Outbound<'_>, _out: &mut Vec<u8>) -> Result<(), CodecError> {
            Ok(())
        }
        fn encode_inbound(&self, _msg: Inbound, _out: &mut Vec<u8>) -> Result<(), CodecError> {
            Ok(())
        }
        fn decode_outbound<'a>(&self, _frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
            Err(CodecError::Malformed)
        }
    }

    /// A handle whose channel nobody drains — an engine still dialing, or
    /// sleeping between redials. `cap` is [`ClientConfig::max_offline_queue`].
    /// The prune receiver comes back too: dropping it would close the signal
    /// channel and silently no-op every [`ClientHandle::request_prune`].
    fn test_handle_capped(cap: usize) -> (ClientHandle, Receiver<ClientCmd>, Receiver<()>) {
        let (cmd_tx, cmd_rx) = async_channel::bounded(cap.max(1));
        let (prune_tx, prune_rx) = async_channel::bounded(1);
        (
            ClientHandle {
                cmd_tx,
                prune_tx,
                next_id: Arc::new(AtomicUsize::new(1)),
            },
            cmd_rx,
            prune_rx,
        )
    }

    fn test_handle() -> (ClientHandle, Receiver<ClientCmd>, Receiver<()>) {
        test_handle_capped(256)
    }

    /// Poll a call once — enough to queue its command — then abandon it, which
    /// is what a lost timeout race does to the future.
    async fn abandon_call(handle: &ClientHandle, method: &'static str) {
        let call = handle.call(method, Payload::from(&b"x"[..]));
        assert!(
            tokio::time::timeout(core::time::Duration::ZERO, call)
                .await
                .is_err(),
            "the call must not resolve"
        );
    }

    /// The caller half of the reclaim path: abandoning a call queues a
    /// `CancelCall` naming exactly that call, so the engine never has to guess
    /// which entry died.
    #[tokio::test]
    async fn abandoning_a_call_queues_its_cancellation() {
        let (handle, cmd_rx, _prune_rx) = test_handle();
        abandon_call(&handle, "one").await;

        match cmd_rx.try_recv() {
            Ok(ClientCmd::Call { id: 1, .. }) => {}
            _ => panic!("the call must be queued first"),
        }
        match cmd_rx.try_recv() {
            Ok(ClientCmd::CancelCall { id: 1 }) => {}
            _ => panic!("abandoning the call must queue its cancellation"),
        }
        assert!(cmd_rx.is_empty(), "no other command is emitted");
    }

    /// A call that resolves normally must not also queue a cancellation — the
    /// engine already dropped its entry when it routed the reply.
    #[tokio::test]
    async fn a_resolved_call_queues_no_cancellation() {
        let (handle, cmd_rx, _prune_rx) = test_handle();
        let call = handle.call("one", Payload::from(&b"x"[..]));
        let answer = async {
            // Take the reply channel out of the queued command and answer it.
            match cmd_rx.recv().await {
                Ok(ClientCmd::Call { reply, .. }) => {
                    let _ = reply.send(Ok(Payload::from(&b"ok"[..])));
                }
                _ => panic!("the call must be queued"),
            }
        };
        let (reply, ()) = futures_util::future::join(call, answer).await;
        assert_eq!(&*reply.expect("the call resolves"), b"ok");
        assert!(
            cmd_rx.is_empty(),
            "a resolved call must not queue a cancellation"
        );
    }

    /// Regression for the *concurrent* timeout batch: every call is issued and
    /// tracked while its receiver is live, then all of them are abandoned at
    /// once, with keepalive disabled and no later call to trigger a sweep.
    ///
    /// Each entry the engine frees drops its reply sender, which the (retained)
    /// receiver observes as `Canceled` — so this asserts the table actually
    /// empties rather than that some reclaim path merely ran. Pre-fix all 1000
    /// entries stayed until the connection ended.
    #[tokio::test]
    async fn a_concurrent_batch_of_cancellations_frees_every_entry() {
        const BATCH: u64 = 1_000;
        let (cmd_tx, cmd_rx) = async_channel::unbounded::<ClientCmd>();
        let config = ClientConfig {
            reconnect: false,
            // The point of the test: nothing but the cancellations can reclaim.
            keepalive_interval: None,
            ..Default::default()
        };
        let clock = crate::executor::test_support::NoopRuntimeOps;
        // Never signalled: this asserts the by-id path reclaims on its own.
        let (_prune_tx, prune_rx) = async_channel::bounded::<()>(1);
        let engine = drive_connection(
            Box::new(SilentPeer::default()),
            &NullCodec,
            &cmd_rx,
            &prune_rx,
            &config,
            &clock,
        );

        let exercise = async {
            // All in flight at once: every receiver is alive as its call is
            // tracked, so nothing is reclaimable until the batch is abandoned.
            let mut replies = Vec::new();
            for id in 1..=BATCH {
                let (reply, rx) = oneshot::channel();
                cmd_tx
                    .try_send(ClientCmd::Call {
                        id,
                        method: String::from("hang"),
                        params: Payload::from(&b"x"[..]),
                        reply,
                    })
                    .expect("the channel is unbounded");
                replies.push(rx);
            }
            // Then the whole batch times out.
            for id in 1..=BATCH {
                cmd_tx
                    .try_send(ClientCmd::CancelCall { id })
                    .expect("the channel is unbounded");
            }
            for (i, rx) in replies.into_iter().enumerate() {
                assert!(
                    rx.await.is_err(),
                    "call {} was still held by the engine",
                    i + 1
                );
            }
        };

        futures_util::pin_mut!(engine);
        futures_util::pin_mut!(exercise);
        match futures_util::future::select(engine, exercise).await {
            futures_util::future::Either::Left(_) => panic!("the engine ended early"),
            futures_util::future::Either::Right(((), _)) => {}
        }
    }

    /// The id counter refuses rather than wraps at its ceiling: a wrapped id
    /// would re-issue one a live subscription still holds, and the engine would
    /// demux that subscription's updates into the wrong sink. Both allocating
    /// call sites fail, and neither reaches the engine — so no pending entry is
    /// ever created under an id that would be mis-routed.
    #[tokio::test]
    async fn exhausted_ids_are_refused_not_wrapped() {
        let (cmd_tx, cmd_rx) = async_channel::bounded(256);
        let (prune_tx, _prune_rx) = async_channel::bounded(1);
        let handle = ClientHandle {
            cmd_tx,
            prune_tx,
            next_id: Arc::new(AtomicUsize::new(usize::MAX)),
        };

        // Bounded at zero: the refusal happens before the id is queued, so it
        // resolves on the first poll. A wrapping counter would instead queue the
        // call and await a reply forever — the timeout turns that regression
        // into a failure rather than a hang.
        let refused = tokio::time::timeout(
            core::time::Duration::ZERO,
            handle.call("one", Payload::from(&b"x"[..])),
        )
        .await
        .expect("a refused call must not reach the engine and await a reply");
        assert!(
            matches!(refused, Err(RpcError::Internal)),
            "a call at the id ceiling must be refused"
        );
        assert!(
            handle.subscribe("tele").is_err(),
            "a subscription at the id ceiling must be refused"
        );
        assert!(
            cmd_rx.is_empty(),
            "a refused allocation must not reach the engine"
        );
    }

    /// Evict-oldest puts a cancellation on the same lossy footing as a write,
    /// but dropping one silently strands its pending entry until the connection
    /// ends — so the eviction has to be noticed. Nothing drains the queue here:
    /// the saturated state a pending dial or a backoff sleep produces.
    #[tokio::test]
    async fn an_evicted_cancellation_raises_the_prune_signal() {
        let (handle, cmd_rx, prune_rx) = test_handle_capped(1);

        handle
            .enqueue(ClientCmd::CancelCall { id: 1 })
            .expect("the engine is still running");
        assert!(prune_rx.is_empty(), "nothing has been evicted yet");

        // Fills the single slot, displacing the cancellation.
        handle
            .write("tele", Payload::from(&b"x"[..]))
            .expect("the engine is still running");

        assert_eq!(cmd_rx.len(), 1, "the cap still holds");
        assert_eq!(
            prune_rx.len(),
            1,
            "an evicted cancellation must fall back to a sweep"
        );
    }

    /// The negative control: eviction is the documented, caller-visible cost of
    /// the bound for ordinary commands. Signalling on every one would drag an
    /// `O(N)` sweep into steady-state backpressure.
    #[tokio::test]
    async fn an_evicted_write_raises_no_prune_signal() {
        let (handle, _cmd_rx, prune_rx) = test_handle_capped(1);

        for topic in ["one", "two"] {
            handle
                .write(topic, Payload::from(&b"x"[..]))
                .expect("the engine is still running");
        }

        assert!(
            prune_rx.is_empty(),
            "an evicted write is expected loss, not a leak"
        );
    }

    /// A cancellation is bookkeeping; a queued `Write` is work the caller asked
    /// for. Cap 2, so the write and the call itself both fit and the
    /// cancellation is the only command that finds the queue full.
    #[tokio::test]
    async fn a_cancellation_never_evicts_a_queued_write() {
        let (handle, cmd_rx, prune_rx) = test_handle_capped(2);

        handle
            .write("tele", Payload::from(&b"x"[..]))
            .expect("the engine is still running");
        abandon_call(&handle, "hang").await;

        assert_eq!(cmd_rx.len(), 2, "the cap still holds");
        match cmd_rx.try_recv() {
            Ok(ClientCmd::Write { topic, .. }) => assert_eq!(topic, "tele"),
            _ => panic!("the caller's write must survive the cancellation"),
        }
        match cmd_rx.try_recv() {
            Ok(ClientCmd::Call { id: 1, .. }) => {}
            _ => panic!("the call itself must still be queued"),
        }
        assert_eq!(
            prune_rx.len(),
            1,
            "the cancellation it could not queue must fall back to a sweep"
        );
    }

    /// The reclaim itself: an abandoned entry goes, a live one stays. Identity
    /// is the dropped receiver, which is why the sweep can stand in for a
    /// cancellation that never arrived.
    #[test]
    fn the_sweep_frees_abandoned_entries_and_keeps_live_ones() {
        let mut pending: PendingCalls = HashMap::new();
        let (abandoned, rx) = oneshot::channel();
        let (live, _live_rx) = oneshot::channel();
        pending.insert(1, abandoned);
        pending.insert(2, live);
        drop(rx); // the caller's future went away

        sweep_abandoned(&mut pending);

        assert!(
            !pending.contains_key(&1),
            "the abandoned call must be freed"
        );
        assert!(
            pending.contains_key(&2),
            "a caller still awaiting its reply must not be dropped"
        );
    }

    /// End to end at capacity 1 against a peer that answers nothing: the exact
    /// loss the bound can inflict. The engine files a call while its caller
    /// still waits, the caller then abandons it — the cancellation lands in the
    /// drained queue — and a write evicts that cancellation. The engine still
    /// reclaims: keepalive off and no later command, so the signal is the only
    /// thing that can wake the sweep.
    ///
    /// Consuming the signal *is* the sweep running (one match arm), and
    /// [`the_sweep_frees_abandoned_entries_and_keeps_live_ones`] pins what it
    /// then does. The table is engine-local, and it cannot be watched from a
    /// retained receiver either: an entry whose receiver is retained is exactly
    /// the entry the sweep must keep. The timeout turns "never woke" into a
    /// failure rather than a hang.
    #[tokio::test]
    async fn an_evicted_cancellation_is_still_reclaimed() {
        use core::future::Future;

        let (handle, cmd_rx, prune_rx) = test_handle_capped(1);
        let config = ClientConfig {
            reconnect: false,
            // Nothing but the signal may reclaim.
            keepalive_interval: None,
            ..Default::default()
        };
        let clock = crate::executor::test_support::NoopRuntimeOps;
        let engine = drive_connection(
            Box::new(SilentPeer::default()),
            &NullCodec,
            &cmd_rx,
            &prune_rx,
            &config,
            &clock,
        );

        let exercise = async {
            // Queue the call, then let the engine file it *while the caller
            // still waits* — a receiver already dropped when the command is
            // read is discarded on arrival and never reaches the table.
            let mut call = Box::pin(handle.call("hang", Payload::from(&b"x"[..])));
            core::future::poll_fn(|cx| {
                assert!(
                    call.as_mut().poll(cx).is_pending(),
                    "the call must not resolve"
                );
                core::task::Poll::Ready(())
            })
            .await;
            tokio::task::yield_now().await;
            assert!(cmd_rx.is_empty(), "the engine must have filed the call");

            // Abandon it: the queue is drained, so the cancellation lands. No
            // yield before the write — the engine must not get the chance to
            // remove the entry by id.
            drop(call);
            assert_eq!(cmd_rx.len(), 1, "the cancellation must be queued");
            assert!(prune_rx.is_empty(), "nothing has fallen back to a sweep");
            handle
                .write("tele", Payload::from(&b"x"[..]))
                .expect("the engine is still running");
            assert_eq!(prune_rx.len(), 1, "the eviction must have been noticed");

            // The wake-up is the point: the engine is parked on a silent peer
            // with keepalive off, so only the signal itself can reach it.
            tokio::time::timeout(core::time::Duration::from_secs(5), async {
                while !prune_rx.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the signal must wake the engine and be consumed");
        };

        futures_util::pin_mut!(engine);
        futures_util::pin_mut!(exercise);
        match futures_util::future::select(engine, exercise).await {
            futures_util::future::Either::Left(_) => panic!("the engine ended early"),
            futures_util::future::Either::Right(((), _)) => {}
        }
    }

    /// The cap is a hard bound: nothing drains this channel, as during a pending
    /// dial or a backoff sleep.
    #[tokio::test]
    async fn the_command_queue_is_bounded() {
        let (handle, cmd_rx, _prune_rx) = test_handle_capped(2);

        for topic in ["one", "two", "three", "four"] {
            handle
                .write(topic, Payload::from(&b"x"[..]))
                .expect("the engine is still running");
        }

        assert_eq!(cmd_rx.len(), 2, "the backlog must not exceed the cap");
        // Oldest-first: the two most recent writes are the ones kept.
        for expected in ["three", "four"] {
            match cmd_rx.try_recv() {
                Ok(ClientCmd::Write { topic, .. }) => assert_eq!(topic, expected),
                _ => panic!("the newest writes must survive eviction"),
            }
        }
    }

    /// `usize::MAX` was the old "unbounded" sentinel; preallocated, it would abort
    /// in the allocator, so it has to degrade to the ceiling instead.
    #[tokio::test]
    async fn an_unbounded_cap_degrades_to_the_ceiling() {
        struct UndialableRemote;
        impl Dialer for UndialableRemote {
            fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
                Box::pin(async { Err(TransportError::Closed) })
            }
        }

        let config = ClientConfig {
            max_offline_queue: usize::MAX,
            ..Default::default()
        };
        let (handle, _engine) = run_client(
            UndialableRemote,
            NullCodec,
            config,
            Arc::new(crate::executor::test_support::NoopRuntimeOps),
        );

        handle
            .write("t", Payload::from(&b"x"[..]))
            .expect("the engine is still running");
    }

    /// A zero cap is clamped, not honored: every command reaches the engine
    /// through this channel, so a zero-capacity one could never deliver.
    #[tokio::test]
    async fn a_zero_cap_is_clamped_to_one() {
        let (handle, cmd_rx, _prune_rx) = test_handle_capped(0);
        let (reply, rx) = oneshot::channel();

        handle
            .enqueue(ClientCmd::Call {
                id: 1,
                method: String::from("one"),
                params: Payload::from(&b"x"[..]),
                reply,
            })
            .expect("the engine is still running");
        handle
            .write("later", Payload::from(&b"x"[..]))
            .expect("the engine is still running");

        assert_eq!(cmd_rx.len(), 1, "a clamped cap still holds exactly one");
        assert!(rx.await.is_err(), "and the evicted caller learns it");
    }

    /// The cap is the channel's own, so it holds across cloned handles without an
    /// admission lock: a full channel evicts rather than admitting a second sender.
    #[tokio::test]
    async fn cloned_handles_cannot_exceed_the_cap() {
        let (handle, cmd_rx, _prune_rx) = test_handle_capped(1);
        let other = handle.clone();

        handle
            .write("first", Payload::from(&b"x"[..]))
            .expect("the engine is still running");
        other
            .write("second", Payload::from(&b"x"[..]))
            .expect("the engine is still running");

        assert_eq!(cmd_rx.len(), 1, "clones share one bounded channel");
        match cmd_rx.try_recv() {
            Ok(ClientCmd::Write { topic, .. }) => assert_eq!(topic, "second"),
            _ => panic!("oldest-first eviction must hold across clones"),
        }
    }

    /// A trimmed command is not silently lost: it carried the only sender of its
    /// reply channel, so the caller resolves rather than awaiting a reply the
    /// engine will never send.
    #[tokio::test]
    async fn a_trimmed_call_fails_its_caller() {
        let (handle, _cmd_rx, _prune_rx) = test_handle_capped(1);
        let (reply, rx) = oneshot::channel();
        handle
            .enqueue(ClientCmd::Call {
                id: 1,
                method: String::from("one"),
                params: Payload::from(&b"x"[..]),
                reply,
            })
            .expect("the engine is still running");

        // At cap 1 the next command displaces it.
        handle
            .write("later", Payload::from(&b"x"[..]))
            .expect("the engine is still running");

        assert!(
            rx.await.is_err(),
            "a trimmed call must not leave its caller waiting"
        );
    }

    type SubSinks = HashMap<String, Sender<Result<SubUpdate, RpcError>>>;
    type SubCursors = HashMap<String, u64>;

    /// One sink and its cursor, for driving [`deliver`] directly.
    fn test_sink() -> (SubSinks, SubCursors, Receiver<Result<SubUpdate, RpcError>>) {
        let (tx, rx) = async_channel::bounded(SUBSCRIBE_CHANNEL_CAP);
        let mut subs = HashMap::new();
        subs.insert(String::from("1"), tx);
        (subs, HashMap::new(), rx)
    }

    fn deliver_event(subs: &mut SubSinks, last_seq: &mut SubCursors, seq: u64) {
        deliver(
            subs,
            last_seq,
            "1",
            seq,
            None,
            Payload::from(&b"x"[..]),
            Delivery::Event,
        );
    }

    /// `seq` arrives off the wire unvalidated, so the ceiling is reachable by a
    /// peer. Computing the gap as `prev + 1` panicked in debug on the *next*
    /// frame, and wrapped to a nonsense gap in release.
    #[tokio::test]
    async fn a_saturated_sequence_does_not_overflow() {
        let (mut subs, mut last_seq, rx) = test_sink();

        deliver_event(&mut subs, &mut last_seq, u64::MAX);
        deliver_event(&mut subs, &mut last_seq, 5);

        let skips: Vec<u64> = core::iter::from_fn(|| rx.try_recv().ok())
            .map(|u| u.unwrap().skipped)
            .collect();
        assert_eq!(skips.len(), 2, "both frames must be delivered");
        assert_eq!(skips[1], 0, "a saturated cursor cannot manufacture a gap");
    }

    /// A repeated or reordered `seq` is delivered but must not rewind the cursor:
    /// a lower cursor makes the *next* valid frame look like a jump, reporting a
    /// loss that never happened.
    #[tokio::test]
    async fn a_rewound_sequence_does_not_invent_a_gap() {
        let (mut subs, mut last_seq, rx) = test_sink();

        for seq in [1, 3, 2, 4] {
            deliver_event(&mut subs, &mut last_seq, seq);
        }

        let skips: Vec<u64> = core::iter::from_fn(|| rx.try_recv().ok())
            .map(|u| u.unwrap().skipped)
            .collect();
        assert_eq!(
            skips,
            alloc::vec![0, 1, 0, 0],
            "only the real gap (2 missing between 1 and 3) may be reported"
        );
        assert_eq!(last_seq.get("1"), Some(&4));
    }

    /// The mirror's loss contract (see [`inbound_pump`]): a server-reported gap
    /// is stepped over, not treated as a fault. The update *carrying* `skipped`
    /// is itself produced into the local record — it is current state, which is
    /// all a mirror promises — and delivery continues past it.
    #[tokio::test]
    async fn a_mirror_gap_still_routes_and_keeps_mirroring() {
        let seen: Arc<spin::Mutex<Vec<Vec<u8>>>> = Arc::new(spin::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let ingest: crate::connector::IngestFn =
            Arc::new(move |_ctx: &crate::RuntimeContext, bytes: &[u8]| {
                recorder.lock().push(bytes.to_vec());
                Ok(())
            });
        let routes = alloc::vec![(String::from("tele"), ingest)];
        let router = Arc::new(RouterBuilder::from_routes(routes).build());
        let ctx =
            crate::RuntimeContext::new(Arc::new(crate::executor::test_support::NoopRuntimeOps));
        let (handle, cmd_rx, _prune_rx) = test_handle();

        let pump = inbound_pump(handle, router, Arc::from("tele"), ctx);
        let remote = async {
            // The pump subscribes on its first poll; take its sink and play a
            // gapped update followed by a clean one.
            let events = match cmd_rx.recv().await {
                Ok(ClientCmd::Subscribe { events, .. }) => events,
                _ => panic!("the pump must subscribe"),
            };
            for (data, skipped) in [(&b"gapped"[..], 7u64), (&b"after"[..], 0)] {
                events
                    .send(Ok(SubUpdate {
                        topic: None,
                        data: Payload::from(data),
                        skipped,
                        snapshot_end: false,
                    }))
                    .await
                    .expect("the pump holds the receiver");
            }
            // Dropping the sink ends the stream, so the pump returns.
            drop(events);
        };
        futures_util::future::join(pump, remote).await;

        let seen = seen.lock();
        assert_eq!(seen.len(), 2, "the gap must not drop or end the mirror");
        assert_eq!(
            seen[0].as_slice(),
            b"gapped",
            "the update reporting the gap must still be produced"
        );
        assert_eq!(seen[1].as_slice(), b"after", "mirroring must continue");
    }
}
