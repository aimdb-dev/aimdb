//! The reactive **server** engine of the session substrate.
//!
//! - [`run_session`] drives one accepted [`Connection`]: a biased `select_biased!`
//!   loop interleaving inbound requests (RPC + subscribe + write) with outbound
//!   subscription events.
//! - [`serve`] is the accept loop over a [`Listener`], honoring
//!   [`SessionLimits::max_connections`].
//!
//! Spawn-free (every per-connection/-subscription task lives in a
//! [`FuturesUnordered`] the runner drives) and runtime-neutral (purely reactive,
//! so no timer and no `tokio`/`embassy-*`).
//!
//! The loops use an **extract-then-act** shape: `select_biased!` only computes a
//! small [`Step`], then the loop acts on it once the arm futures (and their
//! borrows of `conn`/`subs`) drop — `futures`' macro, unlike `tokio::select!`,
//! keeps non-selected futures alive across the handler.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use async_channel::Sender;
use futures_channel::oneshot;
use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::{select_biased, FutureExt};
use hashbrown::HashMap;

use super::{
    BoxFut, BoxStream, Connection, Dispatch, EnvelopeCodec, Inbound, Listener, Outbound, Payload,
    RpcError, SessionLimits, SubUpdate, TransportError,
};

/// Per-session engine knobs.
#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    /// Connection cap (consulted by [`serve`]) and per-connection subscription
    /// cap (by [`run_session`]).
    pub limits: SessionLimits,
    /// If `true`, read one frame before authenticating and pass it to
    /// [`Dispatch::authenticate`] as an in-band Hello. If `false` (default),
    /// authenticate from [`PeerInfo`](super::PeerInfo) alone.
    pub reads_hello: bool,
    /// If `true`, emit an explicit [`Outbound::Subscribed`] ack before the first
    /// event. If `false` (default) the ack is implicit (events carry the
    /// subscription id back).
    pub acks_subscribe: bool,
}

/// Bound for the per-connection event funnel: pending outbound updates a
/// connection may buffer before pumps start dropping.
const EVENT_BUFFER: usize = 256;

/// One subscription update on its way back to the connection's send half.
struct SubEvent {
    sub: String,
    seq: u64,
    /// Concrete record topic, when the subscription tags its updates
    /// (see [`SubUpdate::topic`]).
    topic: Option<Arc<str>>,
    data: Payload,
}

/// What [`run_session`]'s `select_biased!` decided this iteration — extracted so
/// the work runs after the arm futures' borrows of `conn`/`subs` release.
enum Step {
    /// A logical frame arrived from the peer (decode + dispatch).
    Frame(Vec<u8>),
    /// Peer closed or the transport errored — end the session.
    Closed,
    /// A subscription update to encode and forward to the peer.
    Event(SubEvent),
    /// A subscription pump finished — reap it and prune its `cancels` entry,
    /// but only while that entry is still the pump's own (same generation).
    SubDrained(PumpEnd),
}

/// How a subscription pump ended, returned to [`run_session`].
struct PumpEnd {
    sub_id: String,
    /// Which `cancels` entry was the pump's own.
    generation: u64,
    /// The stream ran out on its own, not through a cancel or a closed funnel.
    exhausted: bool,
}

/// Drive one accepted [`Connection`] until it closes.
///
/// Authenticates once, then interleaves (biased toward inbound reads, so a chatty
/// subscription cannot starve the RPC path) incoming requests, outgoing
/// subscription events, and reaping of finished subscription pumps. Dropping the
/// engine cancels every live subscription.
pub async fn run_session<C, D>(
    mut conn: Box<dyn Connection>,
    codec: &C,
    dispatch: &D,
    config: &SessionConfig,
) where
    C: EnvelopeCodec + ?Sized,
    D: Dispatch + ?Sized,
{
    // Resolve the session context (Hello-frame or peer-only — see `reads_hello`).
    let first = if config.reads_hello {
        match conn.recv().await {
            Ok(Some(frame)) => Some(frame),
            // Peer closed or errored before sending the Hello — nothing to serve.
            _ => return,
        }
    } else {
        None
    };
    let ctx = match dispatch.authenticate(conn.peer(), first.as_deref()).await {
        Ok(ctx) => ctx,
        Err(_e) => {
            log_warn!("session authenticate rejected: {:?}", _e);
            return;
        }
    };

    // Open the per-connection session once; the loop threads `&mut` into it.
    let mut session = dispatch.open(&ctx);

    // Event funnel: every per-subscription pump sends updates here; the main loop
    // is the sole writer to the connection. Bounded, so a slow client cannot grow
    // it without limit — pumps drop on overflow (events carry a monotonic `seq`).
    let (mut event_tx, mut event_rx) = async_channel::bounded::<SubEvent>(EVENT_BUFFER);
    // Per-connection subscription pumps; the engine future is their sole owner.
    // Each resolves to its own sub id and generation, so the loop can prune the
    // matching `cancels` entry (a no-op if Unsubscribe already removed it).
    let mut subs: FuturesUnordered<BoxFut<'static, PumpEnd>> = FuturesUnordered::new();
    // sub-id → (generation, cancel handle); dropping/firing the oneshot cancels
    // the pump. The generation tells a pump's own entry from a later
    // subscription that reuses its id after an Unsubscribe: a pump that ends
    // late must not remove that one.
    let mut cancels: HashMap<String, (u64, oneshot::Sender<()>)> = HashMap::new();
    let mut next_generation: u64 = 0;
    // Whether this connection has seen a handshake; a second one starts a new
    // session (see `Session::starts_session`).
    let mut greeted = false;
    // Reused encode scratch buffer.
    let mut out = Vec::new();

    loop {
        // Biased toward inbound reads. The select only decides the next step; the
        // work happens after, in the `match`, once the arm borrows release.
        let step = {
            // Per-iteration futures, fused for the select. `event_rx.recv()` is
            // `!Unpin`, so pin it; `subs` (a `FusedStream`) parks on the empty set
            // while the always-active `recv` arm keeps the select alive.
            let mut recv = conn.recv().fuse();
            let mut event = core::pin::pin!(event_rx.recv().fuse());
            select_biased! {
                // ---- inbound: one logical frame from the peer --------------
                r = recv => match r {
                    Ok(Some(frame)) => Step::Frame(frame),
                    Ok(None) | Err(_) => Step::Closed, // peer closed / transport error
                },
                // ---- outbound: a subscription update to forward ------------
                ev = event => match ev {
                    Ok(ev) => Step::Event(ev),
                    // Funnel closed — only if every sender dropped, which can't
                    // happen while the loop holds `event_tx`, so this is
                    // unreachable; end the session defensively if it ever does.
                    Err(_) => Step::Closed,
                },
                // ---- drain finished subscription pumps ---------------------
                end = subs.select_next_some() => Step::SubDrained(end),
            }
        };

        match step {
            Step::Closed => break,
            // A pump finished; drop its cancel handle so the ended subscription
            // neither leaks nor keeps counting against
            // `max_subs_per_connection` — unless the id now belongs to a newer
            // subscription (Unsubscribe, then Subscribe with the same id).
            Step::SubDrained(end) => {
                let own = cancels
                    .get(&end.sub_id)
                    .is_some_and(|(g, _)| *g == end.generation);
                if !own {
                    continue;
                }
                cancels.remove(&end.sub_id);
                // A subscription the client still holds ended on its own: its
                // source closed, or never opened (a buffer with no subscriber
                // slot left). Tell the client, which turns the error reply to
                // its subscribe id into a terminal error, instead of leaving an
                // acknowledged subscription that never delivers.
                if end.exhausted {
                    // Its last events may still sit in the funnel: the pump
                    // runs inside the reap arm, so a stream that is ready at
                    // once fills the funnel and ends in one poll. Send them
                    // first, so the error is the subscription's last word.
                    let mut flushed = true;
                    while let Ok(ev) = event_rx.try_recv() {
                        if !send_event(&mut conn, codec, &mut out, ev).await {
                            flushed = false;
                            break;
                        }
                    }
                    if !flushed {
                        break;
                    }
                    log_warn!("subscription {} ended: its source closed", end.sub_id);
                    if let Ok(id) = end.sub_id.parse::<u64>() {
                        send_reply_err(&mut conn, codec, &mut out, id, RpcError::Internal).await;
                    }
                }
            }

            Step::Event(ev) => {
                if !send_event(&mut conn, codec, &mut out, ev).await {
                    break;
                }
            }

            Step::Frame(frame) => {
                let msg = match codec.decode(&frame) {
                    Ok(msg) => msg,
                    Err(_e) => continue, // skip a malformed frame, keep the session
                };
                match msg {
                    Inbound::Request { id, method, params } => {
                        if session.starts_session(&method) {
                            if greeted {
                                // A new client on a connection that never closed
                                // (a serial line): nothing of the old session
                                // may reach it or outlive it. Dropping the pumps
                                // releases their buffer subscribers; a fresh
                                // funnel discards their queued events.
                                log_info!(
                                    "new handshake on a live connection: ending {} subscription(s) of the previous client",
                                    cancels.len()
                                );
                                subs = FuturesUnordered::new();
                                cancels.clear();
                                (event_tx, event_rx) =
                                    async_channel::bounded::<SubEvent>(EVENT_BUFFER);
                                session = dispatch.open(&ctx);
                            }
                            greeted = true;
                        }
                        let result = session.call(&method, params).await;
                        out.clear();
                        if codec
                            .encode(Outbound::Reply { id, result }, &mut out)
                            .is_err()
                        {
                            continue;
                        }
                        if conn.send(&out).await.is_err() {
                            break;
                        }
                    }
                    Inbound::Subscribe { id, topic } => {
                        // The opening request id is the subscription's routing key;
                        // events carry it back as `Outbound::Event.sub`.
                        let sub_id = id.to_string();
                        // An id that still names a live subscription is refused:
                        // replacing it would cancel the live one without a word.
                        if cancels.contains_key(&sub_id) {
                            log_warn!("subscribe refused: id {} is still live", sub_id);
                            send_reply_err(&mut conn, codec, &mut out, id, RpcError::Denied).await;
                            continue;
                        }
                        if cancels.len() >= config.limits.max_subs_per_connection {
                            send_reply_err(&mut conn, codec, &mut out, id, RpcError::Denied).await;
                            continue;
                        }
                        match session.subscribe(&topic).await {
                            Ok(stream) => {
                                // Optional explicit ack (see `acks_subscribe`).
                                if config.acks_subscribe {
                                    out.clear();
                                    if codec
                                        .encode(Outbound::Subscribed { sub: &sub_id }, &mut out)
                                        .is_ok()
                                        && conn.send(&out).await.is_err()
                                    {
                                        break;
                                    }
                                }
                                // Optional late-join snapshots (one per covered
                                // record), before the first event. They share
                                // the subscription's `seq` space — numbered
                                // `1..=N` here, events continue at `N + 1` — so
                                // every snapshot is accounted for end-to-end:
                                // one dropped by a slow client sink (or skipped
                                // by the encode failure below) leaves a hole the
                                // client reports as `skipped`, and a loss at the
                                // tail of the burst surfaces on the first event.
                                let snaps = session.snapshots(&topic);
                                let total = snaps.len();
                                let mut seq: u64 = 0;
                                let mut send_failed = false;
                                for (i, (snap_topic, data)) in snaps.into_iter().enumerate() {
                                    seq += 1;
                                    out.clear();
                                    let encoded = codec.encode(
                                        Outbound::Snapshot {
                                            sub: &sub_id,
                                            seq,
                                            // The client reserves a sink slot
                                            // for this one, so the burst's
                                            // loss total always lands.
                                            last: i + 1 == total,
                                            topic: &snap_topic,
                                            data,
                                        },
                                        &mut out,
                                    );
                                    if let Err(_e) = encoded {
                                        // A record whose serialized bytes aren't
                                        // valid for this envelope — broken for
                                        // every read of it, not just here, so say
                                        // so rather than dropping it in silence.
                                        // `seq` already advanced, so the client
                                        // still counts it as loss; if this was the
                                        // burst's last snapshot the `last` flag
                                        // goes with it and no `snapshot_end`
                                        // reaches the subscriber.
                                        log_warn!(
                                            "snapshot encode failed, skipping: sub={} topic={} err={:?}",
                                            sub_id,
                                            snap_topic,
                                            _e
                                        );
                                        continue;
                                    }
                                    if conn.send(&out).await.is_err() {
                                        send_failed = true;
                                        break;
                                    }
                                }
                                if send_failed {
                                    break;
                                }
                                let (cancel_tx, cancel_rx) = oneshot::channel();
                                let generation = next_generation;
                                next_generation += 1;
                                cancels.insert(sub_id.clone(), (generation, cancel_tx));
                                subs.push(Box::pin(pump_subscription(
                                    sub_id,
                                    generation,
                                    stream,
                                    event_tx.clone(),
                                    cancel_rx,
                                    seq,
                                )));
                            }
                            Err(e) => {
                                send_reply_err(&mut conn, codec, &mut out, id, e).await;
                            }
                        }
                    }
                    Inbound::Unsubscribe { sub } => {
                        // Dropping the sender resolves the pump's cancel future.
                        cancels.remove(&sub);
                    }
                    Inbound::Write { topic, payload } => {
                        // Fire-and-forget; single-writer-per-key stays intact.
                        let _ = session.write(&topic, payload).await;
                    }
                    Inbound::Ping => {
                        out.clear();
                        if codec.encode(Outbound::Pong, &mut out).is_ok()
                            && conn.send(&out).await.is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
    }

    // Dropping `subs` here cancels every live subscription pump.
    drop(subs);
}

/// Encode + send one subscription event. `false` only when the connection is
/// gone; an event that does not encode is skipped.
async fn send_event<C: EnvelopeCodec + ?Sized>(
    conn: &mut Box<dyn Connection>,
    codec: &C,
    out: &mut Vec<u8>,
    ev: SubEvent,
) -> bool {
    out.clear();
    let encoded = codec
        .encode(
            Outbound::Event {
                sub: &ev.sub,
                seq: ev.seq,
                topic: ev.topic.as_deref(),
                data: ev.data,
            },
            out,
        )
        .is_ok();
    !encoded || conn.send(out).await.is_ok()
}

/// Encode + send a `Reply` carrying an [`RpcError`]; best-effort (a send/encode
/// failure just ends this attempt — the caller's loop handles a dead connection
/// on its next `send`).
async fn send_reply_err<C: EnvelopeCodec + ?Sized>(
    conn: &mut Box<dyn Connection>,
    codec: &C,
    out: &mut Vec<u8>,
    id: u64,
    err: RpcError,
) {
    out.clear();
    if codec
        .encode(
            Outbound::Reply {
                id,
                result: Err(err),
            },
            out,
        )
        .is_ok()
    {
        let _ = conn.send(out).await;
    }
}

/// Pump one `Session::subscribe` stream into the connection's event funnel,
/// tagging each update with a monotonic `seq`. Ends when the stream finishes or
/// the cancel handle is dropped/fired (Unsubscribe or connection teardown).
///
/// `start_seq` is the last sequence number the late-join snapshot burst used
/// (`0` when there were none), so events continue the *same* counter and a
/// snapshot lost at the tail of the burst still shows up as a gap on the first
/// event.
///
/// Returns its `sub_id` and `generation` so [`run_session`] can prune the
/// `cancels` entry for a pump that ended on its own; the Unsubscribe path
/// already removed it, and a newer subscription under the same id has another
/// generation, so that later prune leaves both alone. `exhausted` tells a
/// stream that ran out from a cancel or a closed funnel.
async fn pump_subscription(
    sub_id: String,
    generation: u64,
    mut stream: BoxStream<'static, SubUpdate>,
    tx: Sender<SubEvent>,
    cancel: oneshot::Receiver<()>,
    start_seq: u64,
) -> PumpEnd {
    // Fuse the cancel receiver: a bare `oneshot::Receiver` reports
    // `is_terminated()` once its sender drops, and `select_biased!` skips
    // terminated arms — so the cancel would never fire. `Fuse` keeps the arm
    // polled until it actually resolves.
    let mut cancel = cancel.fuse();
    let mut seq: u64 = start_seq;
    let mut exhausted = false;
    loop {
        // Independent arms, so a direct `select_biased!` is fine here.
        let update = select_biased! {
            // Resolves on explicit Unsubscribe (send) or on sender drop.
            _ = cancel => break,
            // `BoxStream` is not `FusedStream`, so fuse the per-iteration `next`.
            next = stream.next().fuse() => match next {
                Some(update) => update,
                None => {
                    exhausted = true;
                    break;
                }
            },
        };
        seq += update.skipped + 1;
        // Non-blocking: drop on a full funnel (slow-client protection); only a
        // disconnected funnel ends the pump.
        match tx.try_send(SubEvent {
            sub: sub_id.clone(),
            seq,
            topic: update.topic,
            data: update.data,
        }) {
            Ok(()) => {}
            Err(e) if e.is_full() => {} // drop on overflow
            Err(_) => break,            // funnel disconnected — connection gone
        }
    }
    PumpEnd {
        sub_id,
        generation,
        exhausted,
    }
}

/// Accept connections from `listener` and serve each with [`run_session`],
/// bounded by [`SessionLimits::max_connections`]. The accept loop and all
/// per-connection futures share one [`FuturesUnordered`] — spawn-free.
///
/// Runs until the listener reports [`TransportError::Closed`] (no more
/// connections will come) and every open connection has ended, then returns.
/// A listener that never closes keeps it running.
pub async fn serve<L, C, D>(mut listener: L, codec: Arc<C>, dispatch: Arc<D>, config: SessionConfig)
where
    L: Listener,
    C: EnvelopeCodec + 'static,
    // `?Sized` so a caller can serve an `Arc<dyn Dispatch>`.
    D: Dispatch + 'static + ?Sized,
{
    let mut conns: FuturesUnordered<BoxFut<'static, ()>> = FuturesUnordered::new();
    // Set by a failed accept: the next one yields once before it starts.
    let mut after_error = false;

    loop {
        // Extract the accept result first, then act (keeps the `listener` and
        // `conns` borrows apart).
        let accept = {
            // After an error, the next accept first returns `Pending` once, so
            // this select falls through to the connections: a listener that
            // fails at once, every time, cannot starve them, and the task
            // stays cancellable.
            let mut accept = PendingOnce::new(listener.accept(), after_error).fuse();
            select_biased! {
                a = accept => a,
                // Parks on the empty set, so the accept arm keeps the select alive.
                () = conns.select_next_some() => continue,
            }
        };

        after_error = accept.is_err();
        match accept {
            Ok(conn) => {
                // Soft cap; `len()` counts finished-but-not-yet-reaped futures, so
                // under an accept flood (the biased `accept` arm starves the reap
                // arm) it may read high transiently — acceptable for a soft cap.
                if conns.len() >= config.limits.max_connections {
                    log_warn!(
                        "max_connections={} reached, refusing client",
                        config.limits.max_connections
                    );
                    drop(conn);
                    continue;
                }
                let codec = codec.clone();
                let dispatch = dispatch.clone();
                let cfg = config.clone();
                conns.push(Box::pin(async move {
                    run_session(conn, codec.as_ref(), dispatch.as_ref(), &cfg).await;
                }));
            }
            // The listener hands out no more connections: stop accepting, and
            // serve the open ones to their end. Calling `accept` again would
            // only fail again, at once — a spin that starves those connections.
            Err(TransportError::Closed) => {
                log_info!(
                    "listener closed; serving {} open connection(s) to their end",
                    conns.len()
                );
                while conns.next().await.is_some() {}
                return;
            }
            Err(_e) => {
                log_error!("accept failed: {:?}", _e);
                // Keep serving existing connections despite a transient accept
                // error; the next accept yields first (`after_error`).
            }
        }
    }
}

/// `inner`, but when `yield_first` is set, its first poll returns `Pending`
/// (waking itself) without polling `inner`. Runtime-neutral: one turn back to
/// the executor and to the sibling arms of a select.
struct PendingOnce<F> {
    inner: F,
    yield_first: bool,
}

impl<F> PendingOnce<F> {
    fn new(inner: F, yield_first: bool) -> Self {
        Self { inner, yield_first }
    }
}

impl<F: Future + Unpin> Future for PendingOnce<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.yield_first {
            self.yield_first = false;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll(cx)
    }
}
