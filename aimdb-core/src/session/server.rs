//! Phase 2 **server** engine — the reactive half of the shared session
//! substrate (doc 034 § Layer 2). Written once here; it generalizes the two
//! hand-rolled loops it will replace in Phases 3–4:
//!
//! - [`run_session`] = `remote/handler.rs`'s biased `select!` per-connection loop
//!   (RPC + streaming + writes over one [`Connection`]), transport-erased.
//! - [`serve`] = `remote/supervisor.rs`'s accept loop, generalized over
//!   [`Listener`] and honoring [`SessionLimits::max_connections`].
//!
//! Spawn-free: every per-connection and per-subscription task lives in a
//! [`FuturesUnordered`] owned by the engine future the runner drives — no
//! `tokio::spawn`.
//!
//! **Runtime-neutral (Phase 5).** This engine is purely reactive — it touches no
//! timer — so it carries *zero* runtime knowledge: `futures` channels + a
//! `select_biased!` over the wire, the event funnel, and the subscription task
//! set. No `tokio`/`embassy-*` here; it runs unchanged on both via the adapters.
//!
//! The loops use an **extract-then-act** shape: `select_biased!` only computes a
//! small [`Step`]/action value (it must not touch `conn`/`subs` while a sibling
//! arm's future still borrows them — unlike `tokio::select!`, `futures`' macro
//! keeps the non-selected futures alive across the handler), then the loop acts
//! on that value once the borrows release.

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
    /// Bounds for one session (connection cap is consulted by [`serve`];
    /// per-connection subscription cap by [`run_session`]).
    pub limits: SessionLimits,
    /// How identity is resolved:
    /// - `true` (UDS-style) — read one frame before authenticating and pass it
    ///   to [`Dispatch::authenticate`] as the in-band Hello.
    /// - `false` (WS-style, the default) — authenticate from
    ///   [`PeerInfo`](super::PeerInfo) alone (identity pre-resolved at the HTTP
    ///   upgrade), no frame consumed.
    pub reads_hello: bool,
    /// Emit an explicit [`Outbound::Subscribed`] ack when a subscription opens.
    /// - `false` (default, AimX-style) — the ack is implicit; events flow and
    ///   carry the subscription id back, no ack frame.
    /// - `true` (WS-style) — `run_session` emits `Subscribed { sub }` before the
    ///   first event, restoring the explicit ack WS clients wait on.
    pub acks_subscribe: bool,
}

/// Bound for the per-connection event funnel — caps how many pending outbound
/// updates a single connection may buffer before pumps start dropping (matches
/// the hand-rolled WS server's default per-client channel capacity).
const EVENT_BUFFER: usize = 256;

/// One subscription update on its way back to the connection's send half.
struct SubEvent {
    sub: String,
    seq: u64,
    data: Payload,
}

/// What [`run_session`]'s `select_biased!` decided this iteration. Extracted so
/// the connection/subscription work runs *after* the select's arm futures (and
/// their borrows of `conn`/`subs`) are dropped — see the module note.
enum Step {
    /// A logical frame arrived from the peer (decode + dispatch).
    Frame(Vec<u8>),
    /// Peer closed or the transport errored — end the session.
    Closed,
    /// A subscription update to encode and forward to the peer.
    Event(SubEvent),
    /// A subscription pump finished — nothing to do but reap it.
    SubDrained,
}

/// Drive one accepted [`Connection`] until it closes.
///
/// Authenticates once, then interleaves — `biased`, request-read first so a
/// chatty subscription cannot starve the RPC path — incoming requests, outgoing
/// subscription events funneled from the per-subscription pumps, and draining of
/// finished subscription futures. Dropping the engine (runner cancelled) drops
/// `subs`, cancelling every live subscription.
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
            #[cfg(feature = "tracing")]
            tracing::warn!("session authenticate rejected: {:?}", _e);
            return;
        }
    };

    // Open the per-connection session once. It owns the connection's mutable
    // dispatch state (e.g. `record.drain` cursors); the loop below threads
    // `&mut` into its `call` / `subscribe` / `write`.
    let mut session = dispatch.open(&ctx);

    // Event funnel: every per-subscription pump sends its updates here; the main
    // loop is the sole writer to the connection. **Bounded** so a slow client
    // (one whose socket is backpressured, stalling the main loop) cannot grow the
    // funnel without limit — the pumps drop on overflow rather than accumulate
    // (events carry a monotonic `seq`, so a client can detect the gap). This
    // restores the bounded-buffer slow-client protection the hand-rolled loops had.
    let (event_tx, event_rx) = async_channel::bounded::<SubEvent>(EVENT_BUFFER);
    // Per-connection subscription pumps; the engine future is their sole owner.
    let mut subs: FuturesUnordered<BoxFut<'static, ()>> = FuturesUnordered::new();
    // sub-id → cancel handle (dropping/sending the oneshot cancels the pump,
    // race-free unlike a bare `Notify`).
    let mut cancels: HashMap<String, oneshot::Sender<()>> = HashMap::new();
    // Reused encode scratch buffer.
    let mut out = Vec::new();

    loop {
        // `biased`, request-read first so a chatty subscription cannot starve the
        // RPC path. The `select_biased!` block only *decides* the next step — it
        // must not touch `conn`/`subs` while a sibling arm's future still borrows
        // them; the work happens after, in the `match`.
        let step = {
            // Per-iteration futures, fused for the `select_biased!` arms. The
            // channel `recv()` is `!Unpin` (async-channel holds a pinned
            // listener), so it is pinned in place; `subs` is a `FuturesUnordered`
            // (`Unpin` + `FusedStream`), so `select_next_some` parks on the empty
            // set and the always-active `recv` arm keeps the select alive.
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
                    Err(_) => Step::SubDrained, // funnel closed (tx held, so unreachable)
                },
                // ---- drain finished subscription pumps ---------------------
                () = subs.select_next_some() => Step::SubDrained,
            }
        };

        match step {
            Step::Closed => break,
            Step::SubDrained => {}

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
                        // The request id that opened the subscription is its
                        // routing key; events carry it back as `Outbound::Event.sub`.
                        let sub_id = id.to_string();
                        if cancels.len() >= config.limits.max_subs_per_connection {
                            send_reply_err(&mut conn, codec, &mut out, id, RpcError::Denied).await;
                            continue;
                        }
                        match session.subscribe(&topic).await {
                            Ok(stream) => {
                                // Optional explicit ack (WS-style); AimX leaves
                                // `acks_subscribe` off so its ack stays implicit.
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
                        // Fire-and-forget; routes through the session (single-writer-per-key intact).
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

    // Sole owner of `subs` and `cancels` drops here → every live subscription
    // pump is cancelled.
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
async fn pump_subscription(
    sub_id: String,
    mut stream: BoxStream<'static, Payload>,
    tx: Sender<SubEvent>,
    cancel: oneshot::Receiver<()>,
) {
    // `oneshot::Receiver` reports `is_terminated() == true` once its sender drops
    // (the cancel signal!), and `select_biased!` *skips* terminated arms — so a
    // bare `cancel` arm would never fire on Unsubscribe. Fuse it once: `Fuse`'s
    // `is_terminated` stays false until the fused future itself yields `Ready`, so
    // the arm is polled and the cancellation is observed.
    let mut cancel = cancel.fuse();
    let mut seq: u64 = 0;
    loop {
        // `cancel` and `stream` are independent, and neither handler touches the
        // other's borrow, so this stays a direct `select_biased!`.
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
        // `try_send` keeps the pump non-blocking: a backpressured funnel drops
        // this update (slow-client protection) rather than stalling the bus; only
        // a disconnected funnel ends the pump.
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
}

/// Accept connections from `listener` and serve each with [`run_session`],
/// bounded by [`SessionLimits::max_connections`]. The accept loop and all
/// per-connection futures share one [`FuturesUnordered`] — spawn-free, mirroring
/// `remote/supervisor.rs`.
pub async fn serve<L, C, D>(mut listener: L, codec: Arc<C>, dispatch: Arc<D>, config: SessionConfig)
where
    L: Listener,
    C: EnvelopeCodec + 'static,
    // `?Sized` so a caller can serve an `Arc<dyn Dispatch>` (the generic
    // `SessionServerConnector` does, to stay protocol-agnostic). `run_session`
    // already accepts `?Sized`; `serve` only uses `dispatch` via `clone`/`as_ref`.
    D: Dispatch + 'static + ?Sized,
{
    let mut conns: FuturesUnordered<BoxFut<'static, ()>> = FuturesUnordered::new();

    loop {
        // Extract the accept result first (the accept future borrows `listener`,
        // and pushing/reaping borrows `conns` — keep them apart), then act.
        let accept = {
            let mut accept = listener.accept().fuse();
            select_biased! {
                a = accept => a,
                // `select_next_some` parks on the empty-`FuturesUnordered` case,
                // so the accept arm keeps the select alive without a guard.
                () = conns.select_next_some() => continue,
            }
        };

        match accept {
            Ok(conn) => {
                // Soft cap; `len()` is conservative (a completed-but-undrained
                // future still counts), which only ever refuses one extra.
                if conns.len() >= config.limits.max_connections {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
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
                #[cfg(feature = "tracing")]
                tracing::error!("accept failed: {:?}", _e);
                // Keep serving existing connections despite a transient accept error.
            }
        }
    }
}
