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

use async_channel::Sender;
use futures_channel::oneshot;
use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::{select_biased, FutureExt};
use hashbrown::HashMap;

use super::{
    BoxFut, BoxStream, Connection, Dispatch, EnvelopeCodec, Inbound, Listener, Outbound, Payload,
    RpcError, SessionLimits,
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
    /// A subscription pump finished on its own (stream exhausted) — reap it and
    /// prune its `cancels` entry by the carried sub id.
    SubDrained(String),
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
    let (event_tx, event_rx) = async_channel::bounded::<SubEvent>(EVENT_BUFFER);
    // Per-connection subscription pumps; the engine future is their sole owner.
    // Each resolves to its own sub id, so the loop can prune the matching
    // `cancels` entry (a no-op if Unsubscribe already removed it).
    let mut subs: FuturesUnordered<BoxFut<'static, String>> = FuturesUnordered::new();
    // sub-id → cancel handle; dropping/firing the oneshot cancels the pump.
    let mut cancels: HashMap<String, oneshot::Sender<()>> = HashMap::new();
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
                sub_id = subs.select_next_some() => Step::SubDrained(sub_id),
            }
        };

        match step {
            Step::Closed => break,
            // A pump finished on its own (stream exhausted), not via Unsubscribe;
            // drop its cancel handle so the ended subscription neither leaks nor
            // keeps counting against `max_subs_per_connection`.
            Step::SubDrained(sub_id) => {
                cancels.remove(&sub_id);
            }

            Step::Event(ev) => {
                out.clear();
                let encoded = codec
                    .encode(
                        Outbound::Event {
                            sub: &ev.sub,
                            seq: ev.seq,
                            data: ev.data,
                        },
                        &mut out,
                    )
                    .is_ok();
                if encoded && conn.send(&out).await.is_err() {
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
                                // Optional late-join snapshot, before the first event.
                                if let Some(data) = session.snapshot(&topic) {
                                    out.clear();
                                    if codec
                                        .encode(
                                            Outbound::Snapshot {
                                                topic: &topic,
                                                data,
                                            },
                                            &mut out,
                                        )
                                        .is_ok()
                                        && conn.send(&out).await.is_err()
                                    {
                                        break;
                                    }
                                }
                                let (cancel_tx, cancel_rx) = oneshot::channel();
                                cancels.insert(sub_id.clone(), cancel_tx);
                                subs.push(Box::pin(pump_subscription(
                                    sub_id,
                                    stream,
                                    event_tx.clone(),
                                    cancel_rx,
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
/// Returns its `sub_id` so [`run_session`] can prune the `cancels` entry for a
/// pump that ended on its own; the Unsubscribe path already removed it, so that
/// later prune is a no-op.
async fn pump_subscription(
    sub_id: String,
    mut stream: BoxStream<'static, Payload>,
    tx: Sender<SubEvent>,
    cancel: oneshot::Receiver<()>,
) -> String {
    // Fuse the cancel receiver: a bare `oneshot::Receiver` reports
    // `is_terminated()` once its sender drops, and `select_biased!` skips
    // terminated arms — so the cancel would never fire. `Fuse` keeps the arm
    // polled until it actually resolves.
    let mut cancel = cancel.fuse();
    let mut seq: u64 = 0;
    loop {
        // Independent arms, so a direct `select_biased!` is fine here.
        let data = select_biased! {
            // Resolves on explicit Unsubscribe (send) or on sender drop.
            _ = cancel => break,
            // `BoxStream` is not `FusedStream`, so fuse the per-iteration `next`.
            next = stream.next().fuse() => match next {
                Some(data) => data,
                None => break, // stream exhausted
            },
        };
        seq += 1;
        // Non-blocking: drop on a full funnel (slow-client protection); only a
        // disconnected funnel ends the pump.
        match tx.try_send(SubEvent {
            sub: sub_id.clone(),
            seq,
            data,
        }) {
            Ok(()) => {}
            Err(e) if e.is_full() => {} // drop on overflow
            Err(_) => break,            // funnel disconnected — connection gone
        }
    }
    sub_id
}

/// Accept connections from `listener` and serve each with [`run_session`],
/// bounded by [`SessionLimits::max_connections`]. The accept loop and all
/// per-connection futures share one [`FuturesUnordered`] — spawn-free.
pub async fn serve<L, C, D>(mut listener: L, codec: Arc<C>, dispatch: Arc<D>, config: SessionConfig)
where
    L: Listener,
    C: EnvelopeCodec + 'static,
    // `?Sized` so a caller can serve an `Arc<dyn Dispatch>`.
    D: Dispatch + 'static + ?Sized,
{
    let mut conns: FuturesUnordered<BoxFut<'static, ()>> = FuturesUnordered::new();

    loop {
        // Extract the accept result first, then act (keeps the `listener` and
        // `conns` borrows apart).
        let accept = {
            let mut accept = listener.accept().fuse();
            select_biased! {
                a = accept => a,
                // Parks on the empty set, so the accept arm keeps the select alive.
                () = conns.select_next_some() => continue,
            }
        };

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
            Err(_e) => {
                log_error!("accept failed: {:?}", _e);
                // Keep serving existing connections despite a transient accept error.
            }
        }
    }
}
