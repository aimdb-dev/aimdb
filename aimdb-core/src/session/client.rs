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
//! backoff + keepalive), via the adapter's [`TimeOps`] clock; everything else is
//! `futures` channels. The demux loop uses the same **extract-then-act** shape as
//! the server (compute a [`ClientStep`], then act once the arm borrows release).

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use aimdb_executor::TimeOps;
use async_channel::{Receiver, Sender};
use futures_channel::oneshot;
use futures_util::{select_biased, FutureExt, StreamExt};
use hashbrown::HashMap;

use super::{
    BoxFut, BoxStream, Connection, Dialer, EnvelopeCodec, Inbound, Outbound, Payload, RpcError,
};
use crate::connector::SerializerKind;
use crate::router::RouterBuilder;
use crate::{AimDb, RuntimeAdapter};

/// Client engine knobs. Durations are in **milliseconds** so the engine stays
/// `no_std`-clean; the adapter's [`TimeOps`] turns them into its native `Duration`.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Redial after a dropped/failed connection instead of ending the engine.
    pub reconnect: bool,
    /// Base delay (ms) before the first redial; subsequent redials grow
    /// exponentially, capped at [`max_reconnect_delay`](Self::max_reconnect_delay).
    pub reconnect_delay: u64,
    /// Upper bound (ms) for the reconnect backoff. Defaults to
    /// [`reconnect_delay`](Self::reconnect_delay) (a fixed delay).
    pub max_reconnect_delay: u64,
    /// Maximum redial attempts before giving up. `0` = unlimited (default).
    pub max_reconnect_attempts: usize,
    /// Send a keepalive `Ping` after this many ms of an idle connection; the timer
    /// re-arms each iteration, so traffic resets it. `None` (default) disables it.
    pub keepalive_interval: Option<u64>,
    /// Cap on caller commands buffered while disconnected (oldest dropped past it).
    /// Defaults to `usize::MAX` (unbounded).
    pub max_offline_queue: usize,
    /// Key the subscription demux by **topic** instead of the request `id`.
    /// `false` (default): events echo the id. `true`: the wire pushes data keyed
    /// by topic, so `decode_outbound` returns the topic as `Event.sub`.
    pub topic_routed_subs: bool,
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
            max_offline_queue: usize::MAX,
            topic_routed_subs: false,
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

/// Bound the offline backlog: drop the oldest buffered commands beyond `cap`.
fn bound_offline_queue(cmd_rx: &Receiver<ClientCmd>, cap: usize) {
    while cmd_rx.len() > cap && cmd_rx.try_recv().is_ok() {}
}

/// A cheap-clone handle to a running [`run_client`] engine — the caller-facing
/// RPC surface. Every method funnels a command to the engine, which owns the
/// pending-call map and the wire.
#[derive(Clone)]
pub struct ClientHandle {
    cmd_tx: Sender<ClientCmd>,
}

/// Commands the [`ClientHandle`] funnels to the engine (the engine assigns the
/// correlation `id`, so it stays the sole owner of the demux map).
enum ClientCmd {
    Call {
        method: String,
        params: Payload,
        reply: oneshot::Sender<Result<Payload, RpcError>>,
    },
    Subscribe {
        topic: String,
        events: Sender<Payload>,
    },
    Write {
        topic: String,
        payload: Payload,
    },
}

impl ClientHandle {
    /// Funnel a command to the engine. The channel is unbounded, so `try_send`
    /// never blocks and only fails once the engine has stopped (receiver closed).
    fn enqueue(&self, cmd: ClientCmd) -> Result<(), RpcError> {
        self.cmd_tx.try_send(cmd).map_err(|_| RpcError::Internal)
    }

    /// One-shot RPC: send a request and await its single reply. Returns
    /// [`RpcError::Internal`] if the engine has stopped or the connection drops
    /// before the reply arrives.
    pub async fn call(
        &self,
        method: impl Into<String>,
        params: Payload,
    ) -> Result<Payload, RpcError> {
        let (reply, rx) = oneshot::channel();
        self.enqueue(ClientCmd::Call {
            method: method.into(),
            params,
            reply,
        })?;
        rx.await.map_err(|_| RpcError::Internal)?
    }

    /// Open a subscription; returns the stream of updates immediately (the engine
    /// sends the `Subscribe` request asynchronously). Dropping the stream stops
    /// local delivery.
    pub fn subscribe(
        &self,
        topic: impl Into<String>,
    ) -> Result<BoxStream<'static, Payload>, RpcError> {
        let (events, rx) = async_channel::unbounded::<Payload>();
        self.enqueue(ClientCmd::Subscribe {
            topic: topic.into(),
            events,
        })?;
        // The receiver is itself a `Stream<Item = Payload>`.
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
/// `clock` is the adapter's [`TimeOps`] runtime (e.g. `db.runtime_arc()`); the
/// engine uses it for the reconnect backoff and keepalive — the *only* runtime
/// dependency, so the rest of the engine is runtime-neutral.
pub fn run_client<D, C, R>(
    dialer: D,
    codec: C,
    config: ClientConfig,
    clock: Arc<R>,
) -> (ClientHandle, BoxFut<'static, ()>)
where
    D: Dialer + 'static,
    C: EnvelopeCodec + 'static,
    R: TimeOps + 'static,
{
    let (cmd_tx, cmd_rx) = async_channel::unbounded();
    let handle = ClientHandle { cmd_tx };
    let fut = Box::pin(client_loop(dialer, codec, config, cmd_rx, clock));
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

/// What [`drive_connection`]'s `select_biased!` decided this iteration — extracted
/// so the work runs after the arm futures' borrow of `conn` releases.
enum ClientStep {
    /// A frame (or close/error) arrived from the server.
    Inbound(super::TransportResult<Option<Vec<u8>>>),
    /// The keepalive timer fired — send a `Ping`.
    Keepalive,
    /// A caller command (or `None` = all handles dropped).
    Cmd(Option<ClientCmd>),
}

async fn client_loop<D, C, R>(
    dialer: D,
    codec: C,
    config: ClientConfig,
    cmd_rx: Receiver<ClientCmd>,
    clock: Arc<R>,
) where
    D: Dialer,
    C: EnvelopeCodec,
    R: TimeOps,
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
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::warn!("client dial failed: {:?}", _e);
                match reconnect_after(&mut attempt, &config, &cmd_rx, &*clock).await {
                    true => continue,
                    false => return,
                }
            }
        };

        match drive_connection(conn, &codec, &cmd_rx, &config, &*clock).await {
            Ended::HandlesDropped => return,
            Ended::Disconnected => {
                match reconnect_after(&mut attempt, &config, &cmd_rx, &*clock).await {
                    true => continue,
                    false => return,
                }
            }
        }
    }
}

/// Decide whether to redial: honor `reconnect`, the attempt cap, the offline-queue
/// bound, and the exponential backoff sleep (via the runtime clock). Returns
/// `true` to retry, `false` to stop the engine.
async fn reconnect_after<R: TimeOps>(
    attempt: &mut usize,
    config: &ClientConfig,
    cmd_rx: &Receiver<ClientCmd>,
    clock: &R,
) -> bool {
    if !config.reconnect {
        return false;
    }
    *attempt += 1;
    if config.max_reconnect_attempts != 0 && *attempt >= config.max_reconnect_attempts {
        #[cfg(feature = "tracing")]
        tracing::warn!(
            "client giving up after {} reconnect attempts",
            config.max_reconnect_attempts
        );
        return false;
    }
    bound_offline_queue(cmd_rx, config.max_offline_queue);
    clock
        .sleep(clock.millis(backoff_delay(config, *attempt)))
        .await;
    true
}

/// Drive one dialed [`Connection`]: optional handshake, then `biased` demux of
/// server frames (resolve `Reply` by `id`, route `Event`/`Snapshot` to their
/// subscription channels) interleaved with caller commands. Pending state is
/// per-connection: a disconnect fails outstanding calls (their `oneshot`
/// senders drop → callers see [`RpcError::Internal`]).
async fn drive_connection<C, R>(
    mut conn: Box<dyn Connection>,
    codec: &C,
    cmd_rx: &Receiver<ClientCmd>,
    config: &ClientConfig,
    clock: &R,
) -> Ended
where
    C: EnvelopeCodec + ?Sized,
    R: TimeOps,
{
    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, oneshot::Sender<Result<Payload, RpcError>>> = HashMap::new();
    // sub-id → event sink. The sub-id is `id.to_string()` of the opening
    // request, matching the server's derivation so `Event.sub` routes back.
    let mut subs: HashMap<String, Sender<Payload>> = HashMap::new();
    let mut out = Vec::new();
    let keepalive_ms = config.keepalive_interval;

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
            // Idle keepalive, re-armed each iteration; with no interval it parks on
            // `pending()` forever. `!Unpin`, so pin it for the arm.
            let mut keepalive = core::pin::pin!(async {
                match keepalive_ms {
                    Some(ms) => clock.sleep(clock.millis(ms)).await,
                    None => core::future::pending::<()>().await,
                }
            }
            .fuse());
            // `recv()` is `!Unpin`, so pin it for the arm.
            let mut cmd = core::pin::pin!(cmd_rx.recv().fuse());
            select_biased! {
                // ---- inbound from server: Reply / Event / Snapshot / Pong --
                r = recv => ClientStep::Inbound(r),
                // ---- keepalive: send a Ping when the idle timer fires ------
                _ = keepalive => ClientStep::Keepalive,
                // ---- caller commands from ClientHandle ---------------------
                // `recv()` errors only when every `ClientHandle` is dropped → `None`.
                c = cmd => ClientStep::Cmd(c.ok()),
            }
        };

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
                        } else if result.is_err() {
                            // A subscribe is acked implicitly by its events; the
                            // server replies only on failure, carrying the subscribe
                            // `id` (never a pending call). Drop the event sink so the
                            // stream ends instead of hanging.
                            subs.remove(&id.to_string());
                        }
                    }
                    Ok(Outbound::Event { sub, seq: _, data }) => {
                        let dead = match subs.get(sub) {
                            Some(tx) => tx.try_send(data).is_err(),
                            None => false, // late event for a dropped sub — ignore
                        };
                        if dead {
                            subs.remove(sub);
                        }
                    }
                    Ok(Outbound::Snapshot { topic, data }) => {
                        if let Some(tx) = subs.get(topic) {
                            let _ = tx.try_send(data);
                        }
                    }
                    Ok(Outbound::Pong) => {}
                    // Explicit subscribe ack — informational; the sink already exists.
                    Ok(Outbound::Subscribed { .. }) => {}
                    Err(_e) => continue, // skip a malformed frame, keep the connection
                }
            }

            ClientStep::Keepalive => {
                out.clear();
                if codec.encode_inbound(Inbound::Ping, &mut out).is_ok()
                    && conn.send(&out).await.is_err()
                {
                    return Ended::Disconnected;
                }
            }

            ClientStep::Cmd(cmd) => {
                let cmd = match cmd {
                    Some(cmd) => cmd,
                    None => return Ended::HandlesDropped, // all handles dropped
                };
                match cmd {
                    ClientCmd::Call {
                        method,
                        params,
                        reply,
                    } => {
                        let id = next_id;
                        next_id += 1;
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
                    ClientCmd::Subscribe { topic, events } => {
                        let id = next_id;
                        next_id += 1;
                        // Demux key: topic (topic-routed) or the request id.
                        let key = if config.topic_routed_subs {
                            topic.clone()
                        } else {
                            id.to_string()
                        };
                        subs.insert(key, events);
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
///   through its inbound producer, never a direct co-writer).
///
/// Returns one spawn-free pump future per route for the runner to drive
/// (mirroring the `ConnectorBuilder::build -> Vec<BoxFuture>` spine); it drives
/// the **same** engine as [`run_client`], never a second one.
pub fn pump_client<R>(
    db: &AimDb<R>,
    scheme: &str,
    handle: &ClientHandle,
) -> Vec<BoxFut<'static, ()>>
where
    R: RuntimeAdapter + 'static,
{
    // The type-erased runtime context for context-aware (de)serializers.
    let ctx = db.runtime_any();
    let mut pumps: Vec<BoxFut<'static, ()>> = Vec::new();

    // --- outbound: local record updates -> remote `write` ------------------
    for (destination, consumer, serializer, _config, topic_provider) in
        db.collect_outbound_routes(scheme)
    {
        let handle = handle.clone();
        let ctx = ctx.clone();
        pumps.push(Box::pin(async move {
            let mut reader = match consumer.subscribe_any().await {
                Ok(r) => r,
                Err(_e) => return,
            };
            while let Ok(value) = reader.recv_any().await {
                // Dynamic destination (topic provider) or the static link target.
                let dest = topic_provider
                    .as_ref()
                    .and_then(|p| p.topic_any(&*value))
                    .unwrap_or_else(|| destination.clone());
                let bytes = match &serializer {
                    SerializerKind::Raw(ser) => match ser(&*value) {
                        Ok(b) => b,
                        Err(_e) => continue,
                    },
                    SerializerKind::Context(ser) => match ser(ctx.clone(), &*value) {
                        Ok(b) => b,
                        Err(_e) => continue,
                    },
                };
                if handle.write(dest, Payload::from(bytes.as_slice())).is_err() {
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
        let handle = handle.clone();
        let router = router.clone();
        let ctx = ctx.clone();
        pumps.push(Box::pin(async move {
            let mut stream = match handle.subscribe(id.as_ref()) {
                Ok(s) => s,
                Err(_e) => return,
            };
            while let Some(payload) = stream.next().await {
                let _ = router.route(id.as_ref(), &payload, Some(&ctx)).await;
            }
        }));
    }

    pumps
}
