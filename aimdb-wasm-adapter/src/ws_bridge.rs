//! WebSocket bridge connecting browser AimDB to a server instance.
//!
//! `WsBridge` rides the shared session engine: a [`web_sys::WebSocket`]-backed
//! [`Connection`]/[`Dialer`] pair drives [`run_client`] with the AimX codec
//! ([`AimxCodec`]), so reply/subscription correlation, reconnect backoff,
//! keepalive, and the offline queue all live in
//! `aimdb-core/src/session/client.rs` — exactly once.
//! This module only adapts the JS-event-driven socket to the engine's async
//! `recv`/`send` interface and maps incoming updates to local buffer pushes.
//!
//! # Modes
//!
//! - **Synchronized** — browser instance mirrors server records.
//! - **Hybrid** — works offline with local records, syncs when connected.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use futures_util::StreamExt;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use aimdb_core::builder::AimDb;
use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{run_client, ClientConfig, ClientHandle};
use aimdb_core::{
    BoxFut, Connection, Dialer, PeerInfo, RpcError, RuntimeOps, TransportError, TransportResult,
};

use crate::schema_registry::SchemaRegistry;
use crate::time::SendFuture;
use crate::WasmAdapter;

// ─── Connection status ────────────────────────────────────────────────────

/// Observable connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
    Reconnecting,
}

impl ConnectionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Reconnecting => "reconnecting",
        }
    }
}

// ─── Bridge configuration ─────────────────────────────────────────────────

/// Configuration for `WasmDb.connectBridge()`.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BridgeOptions {
    /// Dot-separated topic patterns to subscribe to (e.g. `["sensors.#"]`).
    #[serde(default)]
    pub subscribe_topics: Vec<String>,
    /// Re-connect automatically on close (default: true).
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Maximum queued commands (writes/subscribes) while disconnected
    /// (default: 256).
    #[serde(default = "default_queue_size")]
    pub max_offline_queue: usize,
    /// Keepalive interval in milliseconds (default: 30 000).
    #[serde(default = "default_keepalive_ms")]
    pub keepalive_ms: u32,
    /// Timeout for query / listTopics requests in milliseconds (default: 30 000).
    /// Set to 0 to disable timeouts.
    #[serde(default = "default_query_timeout_ms")]
    pub query_timeout_ms: u32,
}

fn default_true() -> bool {
    true
}
fn default_queue_size() -> usize {
    256
}
fn default_keepalive_ms() -> u32 {
    30_000
}
fn default_query_timeout_ms() -> u32 {
    30_000
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            subscribe_topics: Vec::new(),
            auto_reconnect: true,
            max_offline_queue: 256,
            keepalive_ms: 30_000,
            query_timeout_ms: 30_000,
        }
    }
}

// ─── Shared bridge state ──────────────────────────────────────────────────

/// State shared between the JS-facing [`WsBridge`], the transport wrappers,
/// and the subscription pumps.
struct BridgeShared {
    status: Cell<ConnectionStatus>,
    on_status: RefCell<Option<js_sys::Function>>,
    /// Set by `disconnect()` — stops redials and ends the pumps.
    stopped: Cell<bool>,
    /// Whether a connection ever succeeded (Connecting vs Reconnecting).
    ever_connected: Cell<bool>,
    auto_reconnect: bool,
    /// The live socket, for a prompt close on `disconnect()`.
    ws: RefCell<Option<web_sys::WebSocket>>,
}

impl BridgeShared {
    /// Transition the observable status (deduplicated) and notify JS.
    fn set_status(&self, status: ConnectionStatus) {
        if self.status.get() == status {
            return;
        }
        self.status.set(status);
        emit_status(&self.on_status, status);
    }

    /// The status to report when a connection ends.
    fn drop_status(&self) -> ConnectionStatus {
        if self.stopped.get() || !self.auto_reconnect {
            ConnectionStatus::Disconnected
        } else {
            ConnectionStatus::Reconnecting
        }
    }
}

// ─── Transport: web_sys::WebSocket as Connection/Dialer ──────────────────

/// Frames arriving from JS event callbacks, funneled to the engine's `recv`.
type FrameRx = futures_channel::mpsc::UnboundedReceiver<Vec<u8>>;

/// A dialed browser WebSocket serving the engine's [`Connection`] contract.
struct WasmWsConnection {
    ws: web_sys::WebSocket,
    frames: FrameRx,
    shared: Rc<BridgeShared>,
    peer: PeerInfo,
    /// JS callbacks kept alive for the socket's lifetime.
    _callbacks: Vec<Closure<dyn FnMut(web_sys::MessageEvent)>>,
    _plain_callbacks: Vec<Closure<dyn FnMut()>>,
}

// SAFETY: wasm32 without atomics is single-threaded; see [`SendFuture`].
// Gated to wasm32 — this crate's `wasm-runtime` feature can be enabled on a
// native host build (e.g. for testing), where the single-threaded argument
// does not hold.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for WasmWsConnection {}

impl Connection for WasmWsConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        // `Ok(None)` when the frame funnel closes (socket closed/errored).
        Box::pin(SendFuture(async move { Ok(self.frames.next().await) }))
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        // `send_with_str` is synchronous — resolve before the future so no JS
        // reference is held across an await.
        let result = core::str::from_utf8(frame)
            .map_err(|_| TransportError::Io)
            .and_then(|text| self.ws.send_with_str(text).map_err(|_| TransportError::Io));
        Box::pin(async move { result })
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

impl Drop for WasmWsConnection {
    fn drop(&mut self) {
        // Detach the JS callbacks before they drop, and close the socket if the
        // engine lets go of a still-open connection (graceful stop).
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        self.ws.set_onerror(None);
        let _ = self.ws.close();
        if self
            .shared
            .ws
            .borrow()
            .as_ref()
            .is_some_and(|current| current == &self.ws)
        {
            self.shared.ws.borrow_mut().take();
        }
    }
}

/// Dials `url` with a fresh `web_sys::WebSocket`, resolving once `onopen`
/// fires. `run_client` calls this on every (re)dial; the backoff between
/// attempts is the engine's.
struct WasmWsDialer {
    url: String,
    shared: Rc<BridgeShared>,
}

// SAFETY: wasm32 without atomics is single-threaded; see [`SendFuture`].
// Gated to wasm32 — see the identical note on `WasmWsConnection` above.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for WasmWsDialer {}

impl Dialer for WasmWsDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        let url = self.url.clone();
        let shared = self.shared.clone();
        Box::pin(SendFuture(async move {
            if shared.stopped.get() {
                return Err(TransportError::Closed);
            }
            if shared.ever_connected.get() {
                shared.set_status(ConnectionStatus::Reconnecting);
            }

            let ws = web_sys::WebSocket::new(&url).map_err(|_| TransportError::Io)?;

            // Frame funnel: onmessage pushes text frames; onclose closes it so
            // the engine's `recv` observes end-of-stream.
            let (frame_tx, frames) = futures_channel::mpsc::unbounded::<Vec<u8>>();
            // Open handshake: whichever of onopen/onclose fires first wins.
            let opened: Rc<RefCell<Option<futures_channel::oneshot::Sender<bool>>>> =
                Rc::new(RefCell::new(None));
            let (open_tx, open_rx) = futures_channel::oneshot::channel::<bool>();
            *opened.borrow_mut() = Some(open_tx);

            let mut msg_callbacks = Vec::new();
            let mut plain_callbacks = Vec::new();

            let on_open = {
                let opened = opened.clone();
                Closure::wrap(Box::new(move || {
                    if let Some(tx) = opened.borrow_mut().take() {
                        let _ = tx.send(true);
                    }
                }) as Box<dyn FnMut()>)
            };
            ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
            plain_callbacks.push(on_open);

            let on_message = {
                let frame_tx = frame_tx.clone();
                Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                    if let Some(text) = event.data().as_string() {
                        let _ = frame_tx.unbounded_send(text.into_bytes());
                    }
                }) as Box<dyn FnMut(web_sys::MessageEvent)>)
            };
            ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            msg_callbacks.push(on_message);

            let on_close = {
                let opened = opened.clone();
                let shared = shared.clone();
                Closure::wrap(Box::new(move || {
                    // Before open: fail the dial. After: end the frame stream.
                    if let Some(tx) = opened.borrow_mut().take() {
                        let _ = tx.send(false);
                    }
                    frame_tx.close_channel();
                    shared.set_status(shared.drop_status());
                }) as Box<dyn FnMut()>)
            };
            ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
            plain_callbacks.push(on_close);

            // The browser always follows `error` with `close`; nothing to do.
            let on_error = Closure::wrap(Box::new(move || {
                web_sys::console::warn_1(&"WsBridge: WebSocket error".into());
            }) as Box<dyn FnMut()>);
            ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            plain_callbacks.push(on_error);

            if open_rx.await != Ok(true) {
                return Err(TransportError::Io);
            }

            shared.ever_connected.set(true);
            *shared.ws.borrow_mut() = Some(ws.clone());
            shared.set_status(ConnectionStatus::Connected);

            Ok(Box::new(WasmWsConnection {
                ws,
                frames,
                shared,
                peer: PeerInfo::default(),
                _callbacks: msg_callbacks,
                _plain_callbacks: plain_callbacks,
            }) as Box<dyn Connection>)
        }))
    }
}

// ─── WsBridge ─────────────────────────────────────────────────────────────

/// WebSocket bridge connecting the in-browser AimDB to a remote server.
///
/// Created via `db.connectBridge(url, options)`. The database remains usable
/// for local `get()` / `set()` / `subscribe()` after the bridge is opened.
///
/// # Example (TypeScript)
/// ```ts
/// const bridge = db.connectBridge('wss://api.example.com/ws', {
///   subscribeTopics: ['sensors/#'],
///   autoReconnect: true,
/// });
/// bridge.onStatusChange((status) => updateIndicator(status));
/// // ...
/// bridge.disconnect();
/// ```
#[wasm_bindgen]
pub struct WsBridge {
    shared: Rc<BridgeShared>,
    /// Dropped on `disconnect()` — stopping the engine gracefully.
    handle: Rc<RefCell<Option<ClientHandle>>>,
    query_timeout_ms: u32,
}

// SAFETY: wasm32-unknown-unknown is single-threaded.
// Gated to wasm32 — see the identical note on `WasmWsConnection` above.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for WsBridge {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for WsBridge {}

#[wasm_bindgen]
impl WsBridge {
    /// Register a callback for connection status changes.
    ///
    /// ```ts
    /// bridge.onStatusChange((status) => { console.log(status); });
    /// ```
    #[wasm_bindgen(js_name = "onStatusChange")]
    pub fn on_status_change(&self, callback: js_sys::Function) {
        // Immediately replay current status so late registrations don't
        // miss the "connected" transition that may have already fired.
        let current = self.shared.status.get();
        let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(current.as_str()));
        // Store for subsequent status changes
        *self.shared.on_status.borrow_mut() = Some(callback);
    }

    /// Send a value to the server for a given topic (AimX `write` frame).
    ///
    /// While disconnected the command is queued by the engine (up to
    /// `maxOfflineQueue`) and flushed on reconnect.
    pub fn write(&self, topic: &str, payload: JsValue) -> Result<(), JsError> {
        let json_payload: serde_json::Value = serde_wasm_bindgen::from_value(payload)
            .map_err(|e| JsError::new(&format!("Payload serialization failed: {e}")))?;
        let bytes = serde_json::to_vec(&json_payload)
            .map_err(|e| JsError::new(&format!("Payload serialization failed: {e}")))?;
        self.with_handle(|handle| {
            handle
                .write(topic, aimdb_core::Payload::from(bytes.as_slice()))
                .map_err(|e| JsError::new(&format!("write failed: {}", rpc_err_str(&e))))
        })
    }

    /// Close the WebSocket and stop reconnection attempts.
    pub fn disconnect(&self) {
        self.shared.stopped.set(true);
        // Setting `stopped` makes the dialer return a terminal `Closed` on its
        // next redial, which stops the engine — dropping this handle alone would
        // not, since the subscription pumps hold their own clones. Dropping it
        // still rejects pending calls once the engine drains its command channel.
        self.handle.borrow_mut().take();
        if let Some(ws) = self.shared.ws.borrow_mut().take() {
            let _ = ws.close();
        }
        self.shared.set_status(ConnectionStatus::Disconnected);
    }

    /// Current connection status as a string.
    pub fn status(&self) -> String {
        self.shared.status.get().as_str().to_string()
    }

    /// Query historical / persisted records (AimX `record.query`).
    ///
    /// Returns a `Promise<Object>` that resolves with `{ records, total }`.
    ///
    /// ```ts
    /// const result = await bridge.query('*', { from: 1700000000000, to: 1700003600000, limit: 500 });
    /// ```
    pub fn query(&self, pattern: &str, options: JsValue) -> js_sys::Promise {
        #[derive(Deserialize, Default)]
        struct QueryOpts {
            from: Option<u64>,
            to: Option<u64>,
            limit: Option<usize>,
        }
        let opts: QueryOpts = if options.is_undefined() || options.is_null() {
            QueryOpts::default()
        } else {
            serde_wasm_bindgen::from_value(options).unwrap_or_default()
        };

        let params = serde_json::json!({
            "name": pattern,
            "start": opts.from,
            "end": opts.to,
            "limit": opts.limit,
        });
        self.call_as_promise("record.query", params)
    }

    /// List all records served by the endpoint (AimX `record.list`).
    ///
    /// Returns a `Promise<Array>` that resolves with one record-metadata object
    /// per record.
    ///
    /// ```ts
    /// const topics = await bridge.listTopics();
    /// ```
    #[wasm_bindgen(js_name = "listTopics")]
    pub fn list_topics(&self) -> js_sys::Promise {
        self.call_as_promise("record.list", serde_json::Value::Null)
    }
}

impl Drop for WsBridge {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ─── Internal constructor ──────────────────────────────────────────────────

impl WsBridge {
    /// Create a new bridge (called from `WasmDb::connect_bridge`).
    pub(crate) fn new_internal(
        db: AimDb,
        schema_map: BTreeMap<String, String>,
        registry: SchemaRegistry,
        url: &str,
        options: JsValue,
    ) -> Result<WsBridge, JsError> {
        let config: BridgeOptions = if options.is_undefined() || options.is_null() {
            BridgeOptions::default()
        } else {
            serde_wasm_bindgen::from_value(options)
                .map_err(|e| JsError::new(&format!("Invalid bridge options: {e}")))?
        };

        let shared = Rc::new(BridgeShared {
            status: Cell::new(ConnectionStatus::Connecting),
            on_status: RefCell::new(None),
            stopped: Cell::new(false),
            ever_connected: Cell::new(false),
            auto_reconnect: config.auto_reconnect,
            ws: RefCell::new(None),
        });

        // Reconnect/keepalive/offline-queue are engine concerns; the backoff
        // ladder mirrors the retired hand-rolled bridge (500 ms → 8 s).
        let engine_config = ClientConfig {
            reconnect: config.auto_reconnect,
            reconnect_delay: 500,
            max_reconnect_delay: 8_000,
            max_reconnect_attempts: 0,
            keepalive_interval: (config.keepalive_ms > 0).then_some(config.keepalive_ms as u64),
            max_offline_queue: config.max_offline_queue,
            sends_hello: false,
        };
        let dialer = WasmWsDialer {
            url: url.to_string(),
            shared: shared.clone(),
        };
        let (handle, engine_fut) =
            run_client(dialer, AimxCodec, engine_config, Arc::new(WasmAdapter));
        wasm_bindgen_futures::spawn_local(engine_fut);

        // One pump per configured pattern: mirror tagged updates into local
        // records, re-subscribing when a stream ends (disconnect) — the
        // subscribe command queues offline and replays after the redial.
        for pattern in &config.subscribe_topics {
            wasm_bindgen_futures::spawn_local(pump_pattern(
                shared.clone(),
                handle.clone(),
                db.clone(),
                schema_map.clone(),
                registry.clone(),
                pattern.clone(),
            ));
        }

        Ok(WsBridge {
            shared,
            handle: Rc::new(RefCell::new(Some(handle))),
            query_timeout_ms: config.query_timeout_ms,
        })
    }

    /// Run `f` with the live engine handle, or fail if `disconnect()` ran.
    fn with_handle<T>(
        &self,
        f: impl FnOnce(&ClientHandle) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        match self.handle.borrow().as_ref() {
            Some(handle) => f(handle),
            None => Err(JsError::new("Bridge is disconnected")),
        }
    }

    /// Issue an AimX call and expose it as a JS Promise (with the configured
    /// timeout), resolving with the JSON result converted to a JS value.
    fn call_as_promise(&self, method: &'static str, params: serde_json::Value) -> js_sys::Promise {
        let handle = self.handle.borrow().clone();
        let timeout_ms = self.query_timeout_ms;
        wasm_bindgen_futures::future_to_promise(async move {
            let handle = handle.ok_or_else(|| JsValue::from_str("Bridge is disconnected"))?;
            let params = serde_json::to_vec(&params)
                .map_err(|e| JsValue::from_str(&format!("params serialization failed: {e}")))?;
            let call = handle.call(method, aimdb_core::Payload::from(params.as_slice()));

            let reply = if timeout_ms > 0 {
                let timeout =
                    WasmAdapter.sleep(core::time::Duration::from_millis(timeout_ms as u64));
                futures_util::pin_mut!(call);
                match futures_util::future::select(call, timeout).await {
                    futures_util::future::Either::Left((reply, _)) => reply,
                    futures_util::future::Either::Right(((), _)) => {
                        return Err(JsValue::from_str("Request timed out"))
                    }
                }
            } else {
                call.await
            };

            let payload =
                reply.map_err(|e| JsValue::from_str(&format!("{method}: {}", rpc_err_str(&e))))?;
            let value: serde_json::Value = serde_json::from_slice(&payload)
                .map_err(|e| JsValue::from_str(&format!("{method}: malformed reply: {e}")))?;
            let serializer = serde_wasm_bindgen::Serializer::json_compatible();
            serde::Serialize::serialize(&value, &serializer)
                .map_err(|e| JsValue::from_str(&format!("{method}: {e}")))
        })
    }
}

/// Human-readable form of the engine's 3-code error vocabulary.
fn rpc_err_str(e: &RpcError) -> &'static str {
    match e {
        RpcError::NotFound => "not_found",
        RpcError::Denied => "denied",
        _ => "internal (engine stopped, disconnected, or server error)",
    }
}

// ─── Subscription pump ─────────────────────────────────────────────────────

/// Subscribe to `pattern` and mirror every tagged update into the local
/// database; when the stream ends on a disconnect, re-subscribe until the
/// bridge is stopped. A server rejection (a terminal error item) is permanent,
/// so the pump stops rather than spinning re-subscribe attempts the server will
/// keep denying.
async fn pump_pattern(
    shared: Rc<BridgeShared>,
    handle: ClientHandle,
    db: AimDb,
    schema_map: BTreeMap<String, String>,
    registry: SchemaRegistry,
    pattern: String,
) {
    loop {
        if shared.stopped.get() {
            return;
        }
        let mut stream = match handle.subscribe(&pattern) {
            Ok(stream) => stream,
            Err(_) => return, // engine stopped
        };
        while let Some(update) = stream.next().await {
            let update = match update {
                Ok(update) => update,
                // Terminal rejection (e.g. the server denied the pattern):
                // re-subscribing would only be denied again — stop this pump.
                Err(_rejected) => return,
            };
            // Wildcard events carry the concrete record topic; an exact-topic
            // subscription may leave it implicit.
            let topic = update.topic.as_deref().unwrap_or(&pattern);
            route_update(&db, &schema_map, &registry, topic, &update.data);
        }
        // Stream ended without a rejection — a disconnect. Pace the re-subscribe
        // so we don't spin; the engine queues it while offline.
        WasmAdapter
            .sleep(core::time::Duration::from_millis(500))
            .await;
    }
}

/// Route one serialized record value into the local database.
fn route_update(
    db: &AimDb,
    schema_map: &BTreeMap<String, String>,
    registry: &SchemaRegistry,
    topic: &str,
    data: &[u8],
) {
    let Some(schema) = schema_map.get(topic) else {
        web_sys::console::warn_1(
            &format!(
                "[WsBridge] No schema mapping for topic='{}' (schema_map has {} entries)",
                topic,
                schema_map.len()
            )
            .into(),
        );
        return;
    };
    let Some(ops) = registry.get(schema) else {
        web_sys::console::warn_1(
            &format!("[WsBridge] unknown schema='{schema}' topic='{topic}'").into(),
        );
        return;
    };
    match serde_json::from_slice::<serde_json::Value>(data) {
        Ok(payload) => (ops.produce_from_json)(db, topic, payload),
        Err(e) => {
            web_sys::console::warn_1(
                &format!("[WsBridge] malformed payload for topic='{topic}': {e}").into(),
            );
        }
    }
}

/// Deserialize `serde_json::Value` → `T` and push to the record buffer.
///
/// This is the fast path for incoming server data — no `JsValue` hop.
pub(crate) fn produce_from_json<T>(db: &AimDb, key: &str, json: serde_json::Value)
where
    T: Send + Sync + 'static + core::fmt::Debug + Clone + serde::de::DeserializeOwned,
{
    match serde_json::from_value::<T>(json) {
        Ok(val) => {
            // Single write path via Producer<T>.
            match db.producer::<T>(key) {
                Ok(producer) => {
                    producer.produce(val);
                }
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!(
                            "[WsBridge] producer lookup failed for key='{}': {:?}",
                            key, e
                        )
                        .into(),
                    );
                }
            }
        }
        Err(e) => {
            web_sys::console::warn_1(
                &format!(
                    "[WsBridge] JSON deserialize failed for key='{}': {}",
                    key, e
                )
                .into(),
            );
        }
    }
}

// ─── Status emission ───────────────────────────────────────────────────────

/// Emit status change to the registered JS callback **and** via DOM
/// `CustomEvent` on `window` (secondary channel for non-React consumers).
///
/// The callback is deferred to a microtask via `spawn_local` so that it
/// executes outside the re-entrant WASM↔JS call stack created by
/// WebSocket event handlers (on_open, on_close, on_message). Direct
/// `cb.call1()` from inside those handlers silently fails — the call
/// returns `Ok` but the JS function body never runs.  Yielding once via
/// `Promise.resolve().await` puts us in a clean microtask context (same
/// as `subscribe_typed`), where React state updates flush correctly.
fn emit_status(on_status: &RefCell<Option<js_sys::Function>>, status: ConnectionStatus) {
    // Primary: deferred callback via microtask
    let cb = on_status.borrow().as_ref().cloned();
    if let Some(cb) = cb {
        let status_str = JsValue::from_str(status.as_str());
        wasm_bindgen_futures::spawn_local(async move {
            // Yield once to escape the current WASM call stack
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
                .await;
            if let Err(e) = cb.call1(&JsValue::NULL, &status_str) {
                web_sys::console::error_1(
                    &format!("[WsBridge] emit_status callback threw: {:?}", e).into(),
                );
            }
        });
    }
    // Secondary: DOM CustomEvent for non-React listeners
    dispatch_status_event(status);
}

/// Dispatch a `CustomEvent("aimdb:status")` on `window` with the status
/// string as `event.detail`.
fn dispatch_status_event(status: ConnectionStatus) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let init = web_sys::CustomEventInit::new();
    init.set_detail(&JsValue::from_str(status.as_str()));
    init.set_bubbles(false);
    if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict("aimdb:status", &init) {
        let _ = web_sys::EventTarget::from(window).dispatch_event(&event);
    }
}
