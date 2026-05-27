//! WebSocket client connector implementation.
//!
//! [`WsClientConnectorImpl`] manages a `tokio-tungstenite` WebSocket connection
//! to a remote AimDB server, with:
//!
//! - **Inbound routing**: `ServerMessage::Data/Snapshot` → `Router::route()`
//! - **Outbound publishing**: `subscribe_any() → recv_any() → Write` message
//! - **Reconnection**: exponential backoff with configurable limits
//! - **Keepalive**: periodic `Ping` messages
//! - **Offline queue**: queued writes during disconnection

use std::{collections::VecDeque, pin::Pin, sync::Arc, time::Duration};

use aimdb_core::{
    router::Router,
    transport::{ConnectorConfig, PublishError},
    OutboundRoute,
};
use aimdb_ws_protocol::{ClientMessage, ServerMessage};
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};

/// Boxed `()`-yielding future used for the connector's nested
/// `FuturesUnordered`. Identical in shape to `aimdb_core::builder::BoxFuture`.
type BoxFuture = Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>>;

/// Aliases for the split halves of the underlying WebSocket stream.
/// Defined once so the reconnect-watcher's `NewLoops` payload type
/// stays readable.
type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWriteSink =
    futures_util::stream::SplitSink<WsStream, tokio_tungstenite::tungstenite::Message>;
type WsReadStream = futures_util::stream::SplitStream<WsStream>;

/// Sent from the reconnect watcher to the outer connector future after a
/// successful reconnect. The outer loop pushes one write-loop future and
/// one read-loop future built from these halves into its
/// `FuturesUnordered`.
struct NewLoops {
    write_sink: WsWriteSink,
    read_stream: WsReadStream,
    write_rx: mpsc::UnboundedReceiver<String>,
}

// ════════════════════════════════════════════════════════════════════
// Configuration
// ════════════════════════════════════════════════════════════════════

/// Internal configuration for the WS client connector.
pub(crate) struct WsClientConfig {
    pub url: String,
    pub auto_reconnect: bool,
    pub max_reconnect_attempts: usize,
    pub keepalive_interval: Option<Duration>,
    pub max_offline_queue: usize,
    pub subscribe_topics: Vec<String>,
}

// ════════════════════════════════════════════════════════════════════
// Connection status
// ════════════════════════════════════════════════════════════════════

/// Connection state of the WS client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
    Reconnecting,
}

// ════════════════════════════════════════════════════════════════════
// Shared state
// ════════════════════════════════════════════════════════════════════

/// Shared mutable state protected by a Mutex.
struct SharedState {
    status: ConnectionStatus,
    pending_writes: VecDeque<String>,
    max_offline_queue: usize,
    /// The current write channel sender. Swapped atomically on reconnect so
    /// that all producers (outbound publishers, publish(), keepalive) always
    /// send through the live connection.
    write_tx: mpsc::UnboundedSender<String>,
}

// ════════════════════════════════════════════════════════════════════
// Connector implementation
// ════════════════════════════════════════════════════════════════════

/// Live WebSocket client connector.
///
/// Created by [`WsClientConnectorBuilder::build()`]. Manages the connection
/// lifecycle and spawns background tasks for:
///
/// - Receiving server messages and routing them via `Router`
/// - Sending outbound data from local record changes
/// - Keepalive pings
/// - Automatic reconnection
pub struct WsClientConnectorImpl {
    /// Shared state for status, offline queue, and the current write channel.
    state: Arc<Mutex<SharedState>>,
    /// Router for inbound data (server → local buffers).
    #[allow(dead_code)]
    router: Arc<Router>,
}

impl WsClientConnectorImpl {
    /// Connect to the remote WebSocket server and return a handle plus
    /// the infrastructure future that drives the read/write/keepalive
    /// loops and the reconnect watcher.
    ///
    /// The returned [`BoxFuture`] owns a [`FuturesUnordered`] holding all
    /// background loops; dropping it (via the runner being cancelled)
    /// terminates every loop in one step. On successful reconnect the
    /// watcher sends a [`NewLoops`] message that the outer future
    /// translates into two fresh futures pushed onto the set.
    pub(crate) async fn connect<R>(
        config: WsClientConfig,
        router: Arc<Router>,
        db: &aimdb_core::builder::AimDb<R>,
    ) -> Result<(Self, BoxFuture), String>
    where
        R: aimdb_executor::RuntimeAdapter + 'static,
    {
        // Connect to the remote server
        let (ws_stream, _response) = tokio_tungstenite::connect_async(&config.url)
            .await
            .map_err(|e| format!("WebSocket connection failed: {e}"))?;

        #[cfg(feature = "tracing")]
        tracing::info!("WS client: connected to {}", config.url);

        let (ws_write, ws_read) = ws_stream.split();

        // Channel for sending text frames from any task to the write loop
        let (write_tx, write_rx) = mpsc::unbounded_channel::<String>();

        let state = Arc::new(Mutex::new(SharedState {
            status: ConnectionStatus::Connected,
            pending_writes: VecDeque::new(),
            max_offline_queue: config.max_offline_queue,
            write_tx,
        }));

        // ── Send subscribe message ──────────────────────────────────
        // The mpsc buffers this until the write loop is first polled by
        // the runner; the message is delivered as soon as the outer
        // future starts.
        if !config.subscribe_topics.is_empty() {
            let sub_msg = ClientMessage::Subscribe {
                topics: config.subscribe_topics.clone(),
            };
            if let Ok(json) = serde_json::to_string(&sub_msg) {
                let _ = state.lock().await.write_tx.send(json);
            }
        }

        let reconnect_url = config.url.clone();
        let reconnect_topics = config.subscribe_topics.clone();
        let auto_reconnect = config.auto_reconnect;
        let max_reconnect_attempts = config.max_reconnect_attempts;
        let keepalive_interval = config.keepalive_interval;
        let runtime_ctx: Arc<dyn core::any::Any + Send + Sync> = db.runtime_any();

        // Channel from the reconnect watcher to the outer future. The
        // watcher sends a `NewLoops` on each successful reconnect; the
        // outer future pushes a fresh write+read future onto its set.
        let (new_loops_tx, mut new_loops_rx) = mpsc::unbounded_channel::<NewLoops>();

        let state_for_future = state.clone();
        let router_for_future = router.clone();
        let runtime_ctx_for_future = runtime_ctx.clone();

        let connector_future: BoxFuture = Box::pin(async move {
            let mut tasks: FuturesUnordered<BoxFuture> = FuturesUnordered::new();

            // Initial write loop. On exit, mark the connection as
            // disconnected so the reconnect watcher notices.
            {
                let state_for_write = state_for_future.clone();
                tasks.push(Box::pin(async move {
                    Self::run_write_loop(ws_write, write_rx).await;

                    #[cfg(feature = "tracing")]
                    tracing::warn!("WS client: write loop ended");

                    state_for_write.lock().await.status = ConnectionStatus::Disconnected;
                }));
            }

            // Initial read loop.
            {
                let router_for_read = router_for_future.clone();
                let ctx_for_read = runtime_ctx_for_future.clone();
                tasks.push(Box::pin(async move {
                    Self::run_read_loop(ws_read, &router_for_read, Some(&ctx_for_read)).await;

                    #[cfg(feature = "tracing")]
                    tracing::warn!("WS client: read loop ended");
                }));
            }

            // Keepalive.
            if let Some(interval) = keepalive_interval {
                let ka_state = state_for_future.clone();
                tasks.push(Box::pin(Self::run_keepalive(ka_state, interval)));
            }

            // Reconnect watcher.
            if auto_reconnect {
                let watcher_state = state_for_future.clone();
                let watcher_router = router_for_future.clone();
                let watcher_ctx = runtime_ctx_for_future.clone();
                let watcher_tx = new_loops_tx.clone();
                tasks.push(Box::pin(Self::run_reconnect_watcher(
                    watcher_state,
                    reconnect_url,
                    reconnect_topics,
                    watcher_router,
                    max_reconnect_attempts,
                    Some(watcher_ctx),
                    watcher_tx,
                )));
            }
            // Drop the watcher's sender clone we still hold so the
            // `new_loops_rx.recv()` returns `None` once the watcher
            // task ends, breaking the outer loop cleanly.
            drop(new_loops_tx);

            // Drive the set. `biased;` keeps reconnect handling
            // (which churns rarely) polled ahead of the drain arm.
            loop {
                tokio::select! {
                    biased;

                    // Reconnect produced fresh halves — push new read +
                    // write futures into the set.
                    maybe_new = new_loops_rx.recv() => match maybe_new {
                        Some(NewLoops { write_sink, read_stream, write_rx }) => {
                            let router_for_read = router_for_future.clone();
                            let ctx_for_read = runtime_ctx_for_future.clone();
                            let state_for_write = state_for_future.clone();
                            tasks.push(Box::pin(async move {
                                Self::run_write_loop(write_sink, write_rx).await;

                                #[cfg(feature = "tracing")]
                                tracing::warn!("WS client: (reconnect) write loop ended");

                                state_for_write.lock().await.status = ConnectionStatus::Disconnected;
                            }));
                            tasks.push(Box::pin(async move {
                                Self::run_read_loop(
                                    read_stream,
                                    &router_for_read,
                                    Some(&ctx_for_read),
                                )
                                .await;

                                #[cfg(feature = "tracing")]
                                tracing::warn!("WS client: (reconnect) read loop ended");
                            }));
                        }
                        None => {
                            // Watcher gone (auto_reconnect disabled or
                            // it gave up after max_attempts). Stop
                            // listening for new loops; tasks continue
                            // draining until empty.
                            break;
                        }
                    },

                    // Drain finished child futures. `Some(_) = next()`
                    // (rather than `select_next_some()`) is the safe form:
                    // an empty `FuturesUnordered` reports
                    // `is_terminated() == true`, and `select_next_some`
                    // panics in that state. With the pattern guard, the
                    // arm is simply disabled when `next()` resolves to
                    // `None`; the always-active reconnect arm keeps the
                    // select alive.
                    Some(_) = tasks.next() => {}
                }
            }

            // After the watcher exited: drain remaining children to
            // completion so resources release cleanly.
            while tasks.next().await.is_some() {}
        });

        Ok((Self { state, router }, connector_future))
    }

    /// Collect one outbound publisher future per route.
    ///
    /// Each future subscribes to a local record, serializes values, and sends
    /// `ClientMessage::Write` to the remote server. Returned futures are appended
    /// to the `AimDbRunner` accumulator.
    pub(crate) fn collect_outbound_futures<R>(
        &self,
        db: &aimdb_core::builder::AimDb<R>,
        outbound_routes: Vec<OutboundRoute>,
    ) -> Vec<Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>>>
    where
        R: aimdb_executor::RuntimeAdapter + 'static,
    {
        let runtime_ctx: Arc<dyn core::any::Any + Send + Sync> = db.runtime_any();
        let mut futures: Vec<Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>>> =
            Vec::with_capacity(outbound_routes.len());

        for (default_topic, consumer, serializer, _config, topic_provider) in outbound_routes {
            let state = self.state.clone();
            let default_topic_clone = default_topic.clone();
            let runtime_ctx = runtime_ctx.clone();

            futures.push(Box::pin(async move {
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "WS client outbound: subscribe failed for '{}': {:?}",
                            default_topic_clone,
                            _e
                        );
                        return;
                    }
                };

                #[cfg(feature = "tracing")]
                tracing::info!(
                    "WS client outbound publisher started for topic: {}",
                    default_topic_clone
                );

                while let Ok(value_any) = reader.recv_any().await {
                    // Resolve topic (dynamic or static)
                    let topic = topic_provider
                        .as_ref()
                        .and_then(|p| p.topic_any(&*value_any))
                        .unwrap_or_else(|| default_topic_clone.clone());

                    // Serialize
                    let bytes = match &serializer {
                        aimdb_core::connector::SerializerKind::Raw(ser) => match ser(&*value_any) {
                            Ok(b) => b,
                            Err(_e) => {
                                #[cfg(feature = "tracing")]
                                tracing::error!(
                                    "WS client outbound: serialize error for '{}': {:?}",
                                    topic,
                                    _e
                                );
                                continue;
                            }
                        },
                        aimdb_core::connector::SerializerKind::Context(ser) => {
                            match ser(runtime_ctx.clone(), &*value_any) {
                                Ok(b) => b,
                                Err(_e) => {
                                    #[cfg(feature = "tracing")]
                                    tracing::error!(
                                        "WS client outbound: serialize error for '{}': {:?}",
                                        topic,
                                        _e
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    // Build Write message
                    let payload: serde_json::Value = match serde_json::from_slice(&bytes) {
                        Ok(v) => v,
                        Err(_e) => {
                            // Fallback: wrap raw bytes as a JSON string
                            serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
                        }
                    };

                    let msg = ClientMessage::Write {
                        topic: topic.clone(),
                        payload,
                    };

                    if let Ok(json) = serde_json::to_string(&msg) {
                        let mut s = state.lock().await;
                        if s.status == ConnectionStatus::Connected {
                            let _ = s.write_tx.send(json);
                        } else if s.pending_writes.len() < s.max_offline_queue {
                            s.pending_writes.push_back(json);
                        }
                        // else: drop (overflow policy)
                    }
                }

                #[cfg(feature = "tracing")]
                tracing::info!(
                    "WS client outbound publisher stopped for topic: {}",
                    default_topic_clone
                );
            }));
        }

        futures
    }

    // ════════════════════════════════════════════════════════════════
    // Background task implementations
    // ════════════════════════════════════════════════════════════════

    /// Write loop: drains the mpsc channel and sends text frames.
    async fn run_write_loop(
        mut ws_write: futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tokio_tungstenite::tungstenite::Message,
        >,
        mut write_rx: mpsc::UnboundedReceiver<String>,
    ) {
        while let Some(text) = write_rx.recv().await {
            let msg = tokio_tungstenite::tungstenite::Message::Text(text.into());
            if ws_write.send(msg).await.is_err() {
                #[cfg(feature = "tracing")]
                tracing::warn!("WS client: write failed, closing write loop");
                break;
            }
        }
    }

    /// Read loop: receives server messages and routes them via the Router.
    async fn run_read_loop(
        mut ws_read: futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
        router: &Router,
        runtime_ctx: Option<&Arc<dyn core::any::Any + Send + Sync>>,
    ) {
        while let Some(Ok(msg)) = ws_read.next().await {
            let text = match msg {
                tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
                tokio_tungstenite::tungstenite::Message::Close(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::info!("WS client: received close frame");
                    break;
                }
                _ => continue,
            };

            let server_msg: ServerMessage = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("WS client: failed to parse server message: {}", _e);
                    continue;
                }
            };

            match server_msg {
                ServerMessage::Data { topic, payload, .. }
                | ServerMessage::Snapshot { topic, payload } => {
                    if let Some(payload) = payload {
                        let bytes = match serde_json::to_vec(&payload) {
                            Ok(b) => b,
                            Err(_e) => {
                                #[cfg(feature = "tracing")]
                                tracing::warn!(
                                    "WS client: failed to serialize payload for '{}': {}",
                                    topic,
                                    _e
                                );
                                continue;
                            }
                        };
                        if let Err(_e) = router.route(&topic, &bytes, runtime_ctx).await {
                            #[cfg(feature = "tracing")]
                            tracing::warn!(
                                "WS client: route failed for topic '{}': {:?}",
                                topic,
                                _e
                            );
                        }
                    }
                }
                ServerMessage::Subscribed { .. } => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("WS client: subscription acknowledged");
                }
                ServerMessage::Error { message, topic, .. } => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        "WS client: server error{}: {}",
                        topic
                            .as_ref()
                            .map(|t| format!(" on '{}'", t))
                            .unwrap_or_default(),
                        message
                    );
                    let _ = (&message, &topic);
                }
                ServerMessage::Pong => {
                    // Keepalive ACK — nothing to do.
                }
                ServerMessage::QueryResult { .. } => {
                    // Query results are handled by the WASM bridge; the native
                    // client connector does not issue queries (yet).
                }
                ServerMessage::TopicList { .. } => {
                    // Topic list responses are not used by the native client connector.
                }
            }
        }
    }

    /// Keepalive loop: sends periodic Ping messages via the shared state sender.
    async fn run_keepalive(state: Arc<Mutex<SharedState>>, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // skip first immediate tick

        loop {
            ticker.tick().await;
            let ping = ClientMessage::Ping;
            if let Ok(json) = serde_json::to_string(&ping) {
                let s = state.lock().await;
                if s.status != ConnectionStatus::Connected {
                    continue;
                }
                if s.write_tx.send(json).is_err() {
                    break; // channel closed, connection gone
                }
            }
        }
    }

    /// Reconnect watcher: monitors connection status and reconnects when needed.
    ///
    /// Uses exponential backoff: 500ms, 1s, 2s, 4s, 8s (capped). On a
    /// successful reconnect it sends a [`NewLoops`] to the outer
    /// connector future, which translates it into a fresh write- and
    /// read-loop future pushed onto the connector's `FuturesUnordered`.
    /// The watcher itself never calls `tokio::spawn`.
    ///
    /// `_router` and `_runtime_ctx` are retained in the signature for
    /// symmetry with the outer connector future — they are not used by
    /// the watcher itself in the new design (the outer future supplies
    /// its own clones to the spawned read-loop future).
    async fn run_reconnect_watcher(
        state: Arc<Mutex<SharedState>>,
        url: String,
        subscribe_topics: Vec<String>,
        _router: Arc<Router>,
        max_attempts: usize,
        _runtime_ctx: Option<Arc<dyn core::any::Any + Send + Sync>>,
        new_loops_tx: mpsc::UnboundedSender<NewLoops>,
    ) {
        let backoff = [500u64, 1_000, 2_000, 4_000, 8_000];
        let mut attempt = 0usize;

        loop {
            // Wait a bit before checking
            tokio::time::sleep(Duration::from_millis(1_000)).await;

            let status = state.lock().await.status;
            if status == ConnectionStatus::Connected || status == ConnectionStatus::Connecting {
                attempt = 0;
                continue;
            }

            // Disconnected — try to reconnect
            if max_attempts > 0 && attempt >= max_attempts {
                #[cfg(feature = "tracing")]
                tracing::error!(
                    "WS client: max reconnect attempts ({}) reached, giving up",
                    max_attempts
                );
                break;
            }

            let delay_ms = backoff.get(attempt).copied().unwrap_or(8_000);
            attempt += 1;

            #[cfg(feature = "tracing")]
            tracing::info!(
                "WS client: reconnecting in {}ms (attempt {})",
                delay_ms,
                attempt
            );

            state.lock().await.status = ConnectionStatus::Reconnecting;
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;

            // Guard: status may have changed during sleep
            if state.lock().await.status != ConnectionStatus::Reconnecting {
                continue;
            }

            match tokio_tungstenite::connect_async(&url).await {
                Ok((ws_stream, _)) => {
                    #[cfg(feature = "tracing")]
                    tracing::info!("WS client: reconnected to {}", url);

                    let (ws_write, ws_read) = ws_stream.split();

                    // Create new channel; sender will be swapped into
                    // shared state; receiver travels to the outer future
                    // inside `NewLoops`.
                    let (new_write_tx, new_write_rx) = mpsc::unbounded_channel::<String>();

                    // Re-subscribe before swapping — the new sender is
                    // still local and other producers cannot reach it
                    // yet, so this `Subscribe` is guaranteed first in
                    // the new write channel.
                    if !subscribe_topics.is_empty() {
                        let sub = ClientMessage::Subscribe {
                            topics: subscribe_topics.clone(),
                        };
                        if let Ok(json) = serde_json::to_string(&sub) {
                            let _ = new_write_tx.send(json);
                        }
                    }

                    // Swap write_tx and flush pending writes in one
                    // critical section. All producers (outbound
                    // publishers, publish(), keepalive) pick up the new
                    // sender on their next lock acquisition.
                    {
                        let mut s = state.lock().await;
                        s.write_tx = new_write_tx;
                        while let Some(msg) = s.pending_writes.pop_front() {
                            let _ = s.write_tx.send(msg);
                        }
                        s.status = ConnectionStatus::Connected;
                    }

                    // Hand the new halves to the outer connector future,
                    // which will push fresh read+write loop futures onto
                    // its `FuturesUnordered`.
                    if new_loops_tx
                        .send(NewLoops {
                            write_sink: ws_write,
                            read_stream: ws_read,
                            write_rx: new_write_rx,
                        })
                        .is_err()
                    {
                        // Outer future has gone away — nothing left to
                        // drive the loops; give up.
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            "WS client: outer future dropped, stopping reconnect watcher"
                        );
                        break;
                    }

                    attempt = 0;
                }
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("WS client: reconnect failed: {}", _e);
                    state.lock().await.status = ConnectionStatus::Disconnected;
                }
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Connector trait
// ════════════════════════════════════════════════════════════════════

impl aimdb_core::transport::Connector for WsClientConnectorImpl {
    /// Send a payload to the remote server as a `Write` message.
    ///
    /// This is the on-demand publish path used by the `ConnectorConfig` system.
    /// Most data flow happens via the outbound publisher tasks instead.
    fn publish(
        &self,
        destination: &str,
        _config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn core::future::Future<Output = Result<(), PublishError>> + Send + '_>> {
        let destination = destination.to_string();
        let payload_owned = payload.to_vec();

        Box::pin(async move {
            let json_payload: serde_json::Value = serde_json::from_slice(&payload_owned)
                .map_err(|_| PublishError::MessageTooLarge)?;

            let msg = ClientMessage::Write {
                topic: destination,
                payload: json_payload,
            };

            let json = serde_json::to_string(&msg).map_err(|_| PublishError::MessageTooLarge)?;

            let mut s = self.state.lock().await;
            if s.status == ConnectionStatus::Connected {
                s.write_tx
                    .send(json)
                    .map_err(|_| PublishError::ConnectionFailed)?;
            } else if s.pending_writes.len() < s.max_offline_queue {
                s.pending_writes.push_back(json);
            } else {
                return Err(PublishError::BufferFull);
            }

            Ok(())
        })
    }
}
