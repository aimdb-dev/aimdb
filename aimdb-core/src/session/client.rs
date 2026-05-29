//! Phase 2 **client** engine — the proactive half of the shared session
//! substrate (doc 034 § "shared with a client engine"; doc 035 Client
//! capability). Std-only, the dual of [`server`](super::server): it *dials* a
//! [`Connection`] via a [`Dialer`] instead of accepting one, *sends* [`Inbound`]
//! and *receives* [`Outbound`] (roles swapped vs the server), and demultiplexes
//! replies by `id`.
//!
//! Per the Phase 2 client-surface gate (resolved: **one engine, both
//! surfaces**), [`run_client`] owns the demux-by-`id` core and returns a
//! [`ClientHandle`] exposing caller-initiated RPC (`call`/`subscribe`/`write`).
//! Record *mirroring* (`pump_client(db, scheme, …)`) is a thin wrapper that
//! lands in **Phase 3** alongside the AimX route collection it needs — it will
//! drive this same engine, not a second one.
//!
//! Spawn-free: [`run_client`] returns the engine future for the runner to drive;
//! it never spawns.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::{
    BoxFut, BoxStream, Connection, Dialer, EnvelopeCodec, Inbound, Outbound, Payload, RpcError,
};
use crate::connector::SerializerKind;
use crate::router::RouterBuilder;
use crate::{AimDb, RuntimeAdapter};

/// Client engine knobs.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Redial after a dropped/failed connection instead of ending the engine.
    pub reconnect: bool,
    /// Delay before each redial when `reconnect` is set.
    pub reconnect_delay: Duration,
    /// Send a Ping handshake on connect and wait for the Pong before accepting
    /// caller commands (the proactive "handshake-as-caller"). Mirrors the
    /// server's `reads_hello`; a real protocol swaps Ping/Pong for its Hello.
    pub sends_hello: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            reconnect: true,
            reconnect_delay: Duration::from_millis(200),
            sends_hello: false,
        }
    }
}

/// A cheap-clone handle to a running [`run_client`] engine — the caller-facing
/// RPC surface. Every method funnels a command to the engine, which owns the
/// pending-call map and the wire.
#[derive(Clone)]
pub struct ClientHandle {
    cmd_tx: mpsc::UnboundedSender<ClientCmd>,
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
        events: mpsc::UnboundedSender<Payload>,
    },
    Write {
        topic: String,
        payload: Payload,
    },
}

impl ClientHandle {
    /// One-shot RPC: send a request and await its single reply. Returns
    /// [`RpcError::Internal`] if the engine has stopped or the connection drops
    /// before the reply arrives.
    pub async fn call(
        &self,
        method: impl Into<String>,
        params: Payload,
    ) -> Result<Payload, RpcError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(ClientCmd::Call {
                method: method.into(),
                params,
                reply,
            })
            .map_err(|_| RpcError::Internal)?;
        rx.await.map_err(|_| RpcError::Internal)?
    }

    /// Open a subscription; returns a stream of updates immediately (the
    /// `Subscribe` request is sent to the server asynchronously by the engine).
    /// Dropping the stream stops local delivery; an explicit remote Unsubscribe
    /// is left to Phase 3 (the connector mirroring path).
    pub fn subscribe(
        &self,
        topic: impl Into<String>,
    ) -> Result<BoxStream<'static, Payload>, RpcError> {
        let (events, rx) = mpsc::unbounded_channel::<Payload>();
        self.cmd_tx
            .send(ClientCmd::Subscribe {
                topic: topic.into(),
                events,
            })
            .map_err(|_| RpcError::Internal)?;
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }

    /// Fire-and-forget write to a remote topic (no reply).
    pub fn write(&self, topic: impl Into<String>, payload: Payload) -> Result<(), RpcError> {
        self.cmd_tx
            .send(ClientCmd::Write {
                topic: topic.into(),
                payload,
            })
            .map_err(|_| RpcError::Internal)
    }
}

/// Build the client engine: returns a [`ClientHandle`] for issuing RPC and the
/// engine future to drive on the runner (spawn-free). The future runs until all
/// `ClientHandle` clones are dropped (graceful stop) — or, with
/// [`ClientConfig::reconnect`] off, until the first disconnect.
pub fn run_client<D, C>(
    dialer: D,
    codec: C,
    config: ClientConfig,
) -> (ClientHandle, BoxFut<'static, ()>)
where
    D: Dialer + 'static,
    C: EnvelopeCodec + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = ClientHandle { cmd_tx };
    let fut = Box::pin(client_loop(dialer, codec, config, cmd_rx));
    (handle, fut)
}

/// Why one connection's session ended — decides reconnect vs stop.
enum Ended {
    /// The connection dropped/errored; redial if configured.
    Disconnected,
    /// Every [`ClientHandle`] was dropped — stop the engine.
    HandlesDropped,
}

async fn client_loop<D, C>(
    dialer: D,
    codec: C,
    config: ClientConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<ClientCmd>,
) where
    D: Dialer,
    C: EnvelopeCodec,
{
    loop {
        let conn = match dialer.connect().await {
            Ok(conn) => conn,
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::warn!("client dial failed: {:?}", _e);
                if config.reconnect {
                    tokio::time::sleep(config.reconnect_delay).await;
                    continue;
                }
                return;
            }
        };

        match drive_connection(conn, &codec, &mut cmd_rx, &config).await {
            Ended::HandlesDropped => return,
            Ended::Disconnected => {
                if config.reconnect {
                    tokio::time::sleep(config.reconnect_delay).await;
                    continue;
                }
                return;
            }
        }
    }
}

/// Drive one dialed [`Connection`]: optional handshake, then `biased` demux of
/// server frames (resolve `Reply` by `id`, route `Event`/`Snapshot` to their
/// subscription channels) interleaved with caller commands. Pending state is
/// per-connection: a disconnect fails outstanding calls (their `oneshot`
/// senders drop → callers see [`RpcError::Internal`]).
async fn drive_connection<C>(
    mut conn: Box<dyn Connection>,
    codec: &C,
    cmd_rx: &mut mpsc::UnboundedReceiver<ClientCmd>,
    config: &ClientConfig,
) -> Ended
where
    C: EnvelopeCodec + ?Sized,
{
    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, oneshot::Sender<Result<Payload, RpcError>>> = HashMap::new();
    // sub-id → event sink. The sub-id is `id.to_string()` of the opening
    // request, matching the server's derivation so `Event.sub` routes back.
    let mut subs: HashMap<String, mpsc::UnboundedSender<Payload>> = HashMap::new();
    let mut out = Vec::new();

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
        tokio::select! {
            biased;

            // ---- inbound from server: Reply / Event / Snapshot / Pong ------
            recv = conn.recv() => {
                let frame = match recv {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => return Ended::Disconnected,
                };
                match codec.decode_outbound(&frame) {
                    Ok(Outbound::Reply { id, result }) => {
                        if let Some(tx) = pending.remove(&id) {
                            let _ = tx.send(result);
                        } else if result.is_err() {
                            // Subscribe-ack contract: a successful subscribe is
                            // acknowledged implicitly by its events flowing; the
                            // server replies only on *failure* (unknown record,
                            // sub cap). Such a Reply carries the subscribe `id`,
                            // which was never registered as a pending call — so
                            // drop the matching event sink to end the stream
                            // (`None`) instead of leaving it hanging forever.
                            subs.remove(&id.to_string());
                        }
                    }
                    Ok(Outbound::Event { sub, seq: _, data }) => {
                        let dead = match subs.get(sub) {
                            Some(tx) => tx.send(data).is_err(),
                            None => false, // late event for a dropped sub — ignore
                        };
                        if dead {
                            subs.remove(sub);
                        }
                    }
                    Ok(Outbound::Snapshot { topic, data }) => {
                        if let Some(tx) = subs.get(topic) {
                            let _ = tx.send(data);
                        }
                    }
                    Ok(Outbound::Pong) => {}
                    Err(_e) => continue, // skip a malformed frame, keep the connection
                }
            }

            // ---- caller commands from ClientHandle -------------------------
            cmd = cmd_rx.recv() => {
                let cmd = match cmd {
                    Some(cmd) => cmd,
                    None => return Ended::HandlesDropped, // all handles dropped
                };
                match cmd {
                    ClientCmd::Call { method, params, reply } => {
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
    use futures_util::StreamExt;

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
