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
    /// Capacity of the shared client engine's command channel (default: 256);
    /// past the cap the oldest queued command is dropped.
    ///
    /// This is the whole command channel, not an offline-writes buffer: every
    /// `write`, `subscribe`, `query` and `listTopics` passes through it, so an
    /// evicted `query` rejects its promise and an evicted `subscribe` ends its
    /// stream. It only *fills* while the engine isn't draining it — during a
    /// pending dial or the backoff between reconnects.
    ///
    /// Clamped to `1..=8192`: the ring is preallocated, so `0` cannot deliver
    /// anything and is raised to `1`, and larger values are capped.
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
    /// Notified when a subscription reports a delivery gap.
    on_gap: RefCell<Option<js_sys::Function>>,
    /// Cumulative updates the server sent that never reached the local mirror.
    dropped_total: Cell<u64>,
    /// Set by `disconnect()` — stops redials and ends the pumps.
    stopped: Cell<bool>,
    /// Whether a connection ever succeeded (Connecting vs Reconnecting).
    ever_connected: Cell<bool>,
    auto_reconnect: bool,
    /// The current socket, for a prompt close on `disconnect()`. Published from
    /// the start of the dial (still `CONNECTING`, so a pending handshake is
    /// reachable) until the owner releases it via [`unpublish_socket`].
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

    /// Record a delivery gap on `topic` and notify JS.
    fn report_gap(&self, topic: &str, skipped: u64) {
        self.dropped_total
            .set(self.dropped_total.get().saturating_add(skipped));
        emit_gap(&self.on_gap, topic, skipped);
    }

    /// Adopt a completed handshake, moving to `Connected`; `false` when
    /// [`WsBridge::disconnect`] landed between `onopen` and this call, telling
    /// the caller to abandon the socket.
    fn accept_dial(&self) -> bool {
        if self.stopped.get() {
            return false;
        }
        self.ever_connected.set(true);
        self.set_status(ConnectionStatus::Connected);
        true
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

/// Depth of the funnel between the JS message callback and the engine's `recv`,
/// sized well above the engine's `SUBSCRIBE_CHANNEL_CAP` so an ordinary
/// late-join snapshot burst passes untouched.
const FRAME_QUEUE_CAP: usize = 1024;

/// Ceiling on a single inbound frame, in bytes. Sized well above any frame an
/// ordinary bridge sees — the largest is a `record.query` reply, which the caller
/// pages with `limit`.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Ceiling on the funnel's *total* queued bytes — the actual memory bound:
/// [`FRAME_QUEUE_CAP`] × [`MAX_FRAME_BYTES`] is ~1 GiB, far past any browser
/// tab's budget. Leaves headroom for several max-size frames.
const FRAME_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// Frames arriving from JS event callbacks, funneled to the engine's `recv`.
type FrameRx = futures_channel::mpsc::Receiver<Vec<u8>>;

/// Sending half of the frame funnel, held by the `onmessage` callback.
type FrameTx = futures_channel::mpsc::Sender<Vec<u8>>;

/// Funnel accounting shared by the `onmessage` callback and the engine's `recv`.
/// `Cell` suffices: wasm32 without atomics is single-threaded.
#[derive(Default)]
struct FunnelState {
    /// Bytes currently queued — raised on enqueue, lowered as `recv` drains.
    queued_bytes: Cell<usize>,
    /// Set when a limit is breached; `recv` then reports EOF instead of
    /// draining, freeing the backlog with the connection.
    overflowed: Cell<bool>,
}

/// What [`funnel_text`] / [`funnel_frame`] did with one inbound frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Funneled {
    /// Queued for the engine's `recv`.
    Accepted,
    /// The funnel was full; the stream is now closed.
    Overflowed,
    /// The frame was past [`MAX_FRAME_BYTES`]; the stream is now closed.
    Oversize,
    /// The funnel was already closed; the frame is discarded.
    Closed,
}

/// Hand one inbound frame to the engine, ending the stream if the funnel is
/// over a limit.
///
/// A full funnel is a loss of unknown shape (a reply, events, part of a
/// snapshot), so the stream ends rather than dropping frames: the engine reads
/// EOF as a disconnect, redials, and the pumps re-subscribe from scratch.
///
/// A free function so the overflow path is testable: the `onmessage` closure
/// is only installed by a completed handshake, which the test lane has no peer
/// for.
fn funnel_frame(tx: &mut FrameTx, state: &FunnelState, frame: Vec<u8>) -> Funneled {
    // Explicit closure check: a sender that reported full stays parked and
    // keeps reporting full, which would re-announce one overflow per late frame.
    if tx.is_closed() {
        return Funneled::Closed;
    }
    // Bytes before slots. `futures_channel` grants each sender a slot beyond the
    // requested capacity, so the frame count is the softer of the two bounds.
    let queued = state.queued_bytes.get();
    if queued.saturating_add(frame.len()) > FRAME_QUEUE_BYTES {
        return overflow(tx, state);
    }
    let len = frame.len();
    match tx.try_send(frame) {
        Ok(()) => {
            state.queued_bytes.set(queued + len);
            Funneled::Accepted
        }
        Err(e) if e.is_full() => overflow(tx, state),
        // Receiver gone: the engine dropped the connection.
        Err(_) => Funneled::Closed,
    }
}

/// End the stream over a limit, and mark it so `recv` drops the backlog rather
/// than delivering it first.
fn overflow(tx: &mut FrameTx, state: &FunnelState) -> Funneled {
    state.overflowed.set(true);
    tx.close_channel();
    Funneled::Overflowed
}

/// Size-check one inbound text frame, then hand it to [`funnel_frame`].
///
/// A frame past [`MAX_FRAME_BYTES`] ends the stream the way a full funnel does.
/// Its length is read JS-side first so an oversize frame is never copied into
/// wasm memory just to be refused; UTF-8 is never shorter than UTF-16, so over
/// the ceiling in code units is over it in bytes — the converse does not hold,
/// hence the exact re-check on the copy.
fn funnel_text(tx: &mut FrameTx, state: &FunnelState, text: &js_sys::JsString) -> Funneled {
    // Same explicit closure check as `funnel_frame`: one report per stream end.
    if tx.is_closed() {
        return Funneled::Closed;
    }
    match (text.length() as usize <= MAX_FRAME_BYTES).then(|| String::from(text)) {
        Some(frame) if frame.len() <= MAX_FRAME_BYTES => {
            funnel_frame(tx, state, frame.into_bytes())
        }
        _ => {
            state.overflowed.set(true);
            tx.close_channel();
            Funneled::Oversize
        }
    }
}

/// A dialed browser WebSocket serving the engine's [`Connection`] contract.
struct WasmWsConnection {
    ws: web_sys::WebSocket,
    frames: FrameRx,
    funnel: Rc<FunnelState>,
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
        Box::pin(async move {
            // Overflow reports EOF without draining: `close_channel` leaves the
            // backlog queued, and dropping the connection is what frees it.
            if self.funnel.overflowed.get() {
                return Ok(None);
            }
            let frame = self.frames.next().await;
            if let Some(frame) = &frame {
                let queued = self.funnel.queued_bytes.get();
                self.funnel
                    .queued_bytes
                    .set(queued.saturating_sub(frame.len()));
            }
            Ok(frame)
        })
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

/// Detach every JS callback and close `ws`. Detaching first matters: the
/// `Closure`s are dropped right after, and a socket still holding them would
/// call into freed WASM closures on its final `close`/`error` event.
fn shutdown_socket(ws: &web_sys::WebSocket) {
    ws.set_onopen(None);
    ws.set_onmessage(None);
    ws.set_onclose(None);
    ws.set_onerror(None);
    let _ = ws.close();
}

/// Release `ws` from [`BridgeShared::ws`] if it is still the current socket,
/// reporting whether it was. Holds no borrow on return: callers answer `true`
/// by emitting a status, which re-enters JS, and a listener calling
/// [`WsBridge::disconnect`] borrows `shared.ws` mutably.
fn unpublish_socket(shared: &BridgeShared, ws: &web_sys::WebSocket) -> bool {
    let is_current = shared
        .ws
        .borrow()
        .as_ref()
        .is_some_and(|current| current == ws);
    if is_current {
        shared.ws.borrow_mut().take();
    }
    is_current
}

impl Drop for WasmWsConnection {
    fn drop(&mut self) {
        shutdown_socket(&self.ws);
        // With `onclose` detached, this is the only place left to report the
        // transition — skipped unless we still own the published socket, since
        // otherwise `disconnect()` or a newer dial owns the status.
        // `set_status` deduplicates for teardowns that did reach `onclose`.
        if unpublish_socket(&self.shared, &self.ws) {
            self.shared.set_status(self.shared.drop_status());
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
        // Declare our AimX version in the URL so the server's upgrade-time gate
        // admits us — a browser cannot set custom WebSocket handshake headers,
        // so the version rides the query string (`?v=3.0`).
        let url = aimdb_core::remote::ws_url_with_version(&self.url);
        let shared = self.shared.clone();
        Box::pin(SendFuture(async move {
            if shared.stopped.get() {
                return Err(TransportError::Closed);
            }

            // The constructor throws only on a URL the browser will never
            // accept, so fail terminally: as `Io` the engine would redial it
            // forever while the status stayed `Connecting`. Nothing is
            // published yet, so report the transition here.
            let ws = match web_sys::WebSocket::new(&url) {
                Ok(ws) => ws,
                Err(_) => {
                    web_sys::console::error_1(
                        &format!("WsBridge: rejected WebSocket URL '{url}'").into(),
                    );
                    shared.set_status(ConnectionStatus::Disconnected);
                    return Err(TransportError::Closed);
                }
            };

            // Publish *before* awaiting the handshake so `disconnect()` can
            // reach a dial in flight and the handshake below resolves `false`
            // instead of waiting out the browser's connect timeout.
            //
            // Nothing between the `stopped` check above and this line may
            // `.await` or re-enter JS — a `disconnect()` slipping in would be
            // undone by the publish. (`set_status` below does re-enter JS,
            // hence its position after the publish.)
            *shared.ws.borrow_mut() = Some(ws.clone());

            if shared.ever_connected.get() {
                shared.set_status(ConnectionStatus::Reconnecting);
            }

            // Frame funnel: onmessage pushes text frames; onclose closes it so
            // the engine's `recv` observes end-of-stream.
            let (mut frame_tx, frames) = futures_channel::mpsc::channel::<Vec<u8>>(FRAME_QUEUE_CAP);
            let funnel = Rc::new(FunnelState::default());
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
                let mut frame_tx = frame_tx.clone();
                let funnel = funnel.clone();
                Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                    // AimX is a text protocol; a binary frame is not ours.
                    let data = event.data();
                    let Some(text) = data.dyn_ref::<js_sys::JsString>() else {
                        return;
                    };
                    match funnel_text(&mut frame_tx, &funnel, text) {
                        Funneled::Overflowed => web_sys::console::warn_1(
                            &format!(
                                "WsBridge: frame funnel past {FRAME_QUEUE_CAP} frames or \
                                 {FRAME_QUEUE_BYTES} bytes — dropping the connection to resync"
                            )
                            .into(),
                        ),
                        Funneled::Oversize => web_sys::console::warn_1(
                            &format!(
                                "WsBridge: inbound frame past the {MAX_FRAME_BYTES}-byte ceiling \
                                 — dropping the connection to resync"
                            )
                            .into(),
                        ),
                        Funneled::Accepted | Funneled::Closed => {}
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

            // Handshake failed: `onclose` before `onopen`, or the sender
            // dropped. Detach the callbacks before this future frees them on
            // return, and unpublish so a dead socket never passes for the
            // live one.
            if open_rx.await != Ok(true) {
                shutdown_socket(&ws);
                unpublish_socket(&shared, &ws);
                // A stopped bridge never dials again: terminal `Closed` stops
                // the engine now, where transient `Io` would cost a reconnect
                // backoff first.
                return Err(if shared.stopped.get() {
                    TransportError::Closed
                } else {
                    TransportError::Io
                });
            }

            // `disconnect()` may have landed between `onopen` and here. Abandon
            // the socket rather than report `Connected` after a completed
            // disconnect; `Closed` is terminal, so the engine stops and the
            // subscription pumps release their `ClientHandle` clones.
            if !shared.accept_dial() {
                shutdown_socket(&ws);
                unpublish_socket(&shared, &ws);
                return Err(TransportError::Closed);
            }

            Ok(Box::new(WasmWsConnection {
                ws,
                frames,
                funnel,
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
///   subscribeTopics: ['sensors.#'],
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
        // Replay the current status so late registrations don't miss a
        // transition that already fired.
        let current = self.shared.status.get();
        let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(current.as_str()));
        *self.shared.on_status.borrow_mut() = Some(callback);
    }

    /// Register a callback for subscription delivery gaps.
    ///
    /// Fires with `(topic, skipped)` whenever the server-side buffer dropped
    /// updates before the one just mirrored — the local record jumped ahead and
    /// intermediate values are gone for good. Without this, a gap is
    /// indistinguishable from an idle producer.
    ///
    /// ```ts
    /// bridge.onGap((topic, skipped) => console.warn(`${topic}: lost ${skipped}`));
    /// ```
    #[wasm_bindgen(js_name = "onGap")]
    pub fn on_gap(&self, callback: js_sys::Function) {
        *self.shared.on_gap.borrow_mut() = Some(callback);
    }

    /// Total updates dropped before reaching the local mirror since the bridge
    /// was created (`0` while delivery has been lossless).
    #[wasm_bindgen(js_name = "droppedUpdates")]
    pub fn dropped_updates(&self) -> f64 {
        self.shared.dropped_total.get() as f64
    }

    /// Send a value to the server for a given topic (AimX `write` frame).
    ///
    /// While disconnected the command is queued by the engine and flushed on
    /// reconnect. The queue is shared with every other command and bounded by
    /// `maxOfflineQueue`; past the cap the oldest is dropped.
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
        // `stopped` fails the next redial terminally, which stops the engine —
        // dropping this handle alone would not, since the pumps hold clones.
        self.handle.borrow_mut().take();
        // The dialer publishes its socket before awaiting `onopen`, so this
        // also interrupts a pending handshake rather than waiting out the
        // browser's connect timeout.
        let ws = self.shared.ws.borrow_mut().take();
        if let Some(ws) = ws {
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
    /// `pattern` is MQTT-style over the record key: `*` matches exactly one
    /// dot-separated segment, `#` zero or more — so `#`, not `*`, is "every
    /// record".
    ///
    /// Returns a `Promise<Object>` that resolves with `{ records, total }`.
    ///
    /// ```ts
    /// const result = await bridge.query('#', { from: 1700000000000, to: 1700003600000, limit: 500 });
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
            on_gap: RefCell::new(None),
            dropped_total: Cell::new(0),
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

        // One pump per configured pattern; the subscribe command queues offline
        // and replays after a redial.
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
                // Losing the race drops `call`, which is cancel-safe: the
                // dropped future frees its pending-call entry in the engine.
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

/// Human-readable form of the engine's `RpcError` vocabulary.
fn rpc_err_str(e: &RpcError) -> &'static str {
    match e {
        RpcError::NotFound => "not_found",
        RpcError::Denied => "denied",
        RpcError::VersionMismatch => {
            "version_mismatch (server rejected the client protocol version)"
        }
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
            // Surface gaps: from JS a silent hole looks identical to an idle
            // producer.
            if update.skipped > 0 {
                shared.report_gap(topic, update.skipped);
            }
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
///
/// Decodes straight into `T`, not through `Linkable::from_bytes`, so a
/// `Migratable` chain does not run: an off-version payload warns and is
/// dropped. Deliberate — see [`crate::schema_registry`].
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

/// Emit a status change to the registered JS callback and as a DOM
/// `CustomEvent` on `window` (secondary channel for non-React consumers).
///
/// The callback is deferred to a microtask: called directly from inside a
/// WebSocket event handler it silently never runs, so a single
/// `Promise.resolve().await` escapes the re-entrant WASM↔JS stack first.
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

/// Notify JS that `skipped` updates were lost on `topic`: always warn on the
/// console, then call the registered handler — deferred to a microtask for the
/// same re-entrancy reason as [`emit_status`].
fn emit_gap(on_gap: &RefCell<Option<js_sys::Function>>, topic: &str, skipped: u64) {
    web_sys::console::warn_1(
        &format!("[WsBridge] delivery gap: {skipped} update(s) dropped for topic='{topic}'").into(),
    );
    let Some(cb) = on_gap.borrow().as_ref().cloned() else {
        return;
    };
    let topic = JsValue::from_str(topic);
    let skipped = JsValue::from_f64(skipped as f64);
    wasm_bindgen_futures::spawn_local(async move {
        let _ =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
        if let Err(e) = cb.call2(&JsValue::NULL, &topic, &skipped) {
            web_sys::console::error_1(
                &format!("[WsBridge] emit_gap callback threw: {:?}", e).into(),
            );
        }
    });
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

// ─── Tests ─────────────────────────────────────────────────────────────────

// Needs a real `web_sys::WebSocket`, so this is the browser lane only
// (`make wasm-test`); off-target the socket constructor is an unusable stub.
#[cfg(all(test, target_arch = "wasm32"))]
mod dial_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    /// TEST-NET-1 (RFC 5737) on an unblocked port: unroutable, so the handshake
    /// stays pending and a socket built from it never opens or closes on its
    /// own.
    const PENDING_URL: &str = "ws://192.0.2.1:8443/aimdb-test";

    /// A URL `WebSocket::new` throws on. An unparseable string would not do —
    /// it resolves against the test page's base into a *valid* `ws://` URL.
    const REJECTED_URL: &str = "ftp://192.0.2.1:8443/aimdb-test";

    fn shared() -> Rc<BridgeShared> {
        shared_with_reconnect(true)
    }

    fn shared_with_reconnect(auto_reconnect: bool) -> Rc<BridgeShared> {
        Rc::new(BridgeShared {
            status: Cell::new(ConnectionStatus::Connecting),
            on_status: RefCell::new(None),
            on_gap: RefCell::new(None),
            dropped_total: Cell::new(0),
            stopped: Cell::new(false),
            ever_connected: Cell::new(false),
            auto_reconnect,
            ws: RefCell::new(None),
        })
    }

    #[wasm_bindgen_test]
    fn accept_dial_reports_connected_while_running() {
        let shared = shared();

        assert!(shared.accept_dial());
        assert!(shared.ever_connected.get());
        assert_eq!(shared.status.get(), ConnectionStatus::Connected);
    }

    /// Regression: `disconnect()` landing between `onopen` and adoption must
    /// not flip the status back to `Connected`.
    #[wasm_bindgen_test]
    fn accept_dial_is_refused_after_disconnect_during_handshake() {
        let shared = shared();
        shared.stopped.set(true);
        shared.set_status(ConnectionStatus::Disconnected);

        assert!(
            !shared.accept_dial(),
            "a stopped bridge must not adopt a late handshake"
        );
        assert_eq!(
            shared.status.get(),
            ConnectionStatus::Disconnected,
            "a late onopen must not undo a completed disconnect"
        );
        assert!(!shared.ever_connected.get());
    }

    /// A published connection over a socket that never opens, plus the funnel's
    /// sending half — enough for the `Drop` seam.
    fn test_connection(shared: &Rc<BridgeShared>) -> (WasmWsConnection, FrameTx) {
        let ws = web_sys::WebSocket::new(PENDING_URL).unwrap();
        *shared.ws.borrow_mut() = Some(ws.clone());
        let (frame_tx, frames) = futures_channel::mpsc::channel::<Vec<u8>>(FRAME_QUEUE_CAP);
        let conn = WasmWsConnection {
            ws,
            frames,
            funnel: Rc::new(FunnelState::default()),
            shared: shared.clone(),
            peer: PeerInfo::default(),
            _callbacks: Vec::new(),
            _plain_callbacks: Vec::new(),
        };
        (conn, frame_tx)
    }

    /// Regression: a connection dropped without an `onclose` (funnel overflow,
    /// engine stop) still moves the observable status — nothing else can.
    #[wasm_bindgen_test]
    fn connection_drop_reports_disconnected_without_auto_reconnect() {
        let shared = shared_with_reconnect(false);
        shared.accept_dial();
        let (conn, _frame_tx) = test_connection(&shared);
        assert_eq!(shared.status.get(), ConnectionStatus::Connected);

        drop(conn);

        assert_eq!(
            shared.status.get(),
            ConnectionStatus::Disconnected,
            "a dropped connection must not leave JS reading \"connected\""
        );
        assert!(
            shared.ws.borrow().is_none(),
            "the dead socket must not stay published"
        );
    }

    /// Same teardown with reconnect on: the status becomes `Reconnecting`, which
    /// the next successful dial resolves back to `Connected`.
    #[wasm_bindgen_test]
    fn connection_drop_reports_reconnecting_with_auto_reconnect() {
        let shared = shared();
        shared.accept_dial();
        let (conn, _frame_tx) = test_connection(&shared);

        drop(conn);

        assert_eq!(shared.status.get(), ConnectionStatus::Reconnecting);
        assert!(shared.ws.borrow().is_none());
    }

    /// A drop that no longer owns the published socket (a `disconnect()` took
    /// it, or a newer dial replaced it) must leave the status alone.
    #[wasm_bindgen_test]
    fn connection_drop_after_disconnect_keeps_disconnected() {
        let shared = shared();
        shared.accept_dial();
        let (conn, _frame_tx) = test_connection(&shared);

        // What `disconnect()` does to the socket, before the engine unwinds.
        shared.stopped.set(true);
        let taken = shared.ws.borrow_mut().take().unwrap();
        let _ = taken.close();
        shared.set_status(ConnectionStatus::Disconnected);

        drop(conn);

        assert_eq!(shared.status.get(), ConnectionStatus::Disconnected);
    }

    /// Regression: a full funnel ends the frame stream, which the engine reads
    /// as a disconnect.
    #[wasm_bindgen_test]
    async fn funnel_overflow_ends_the_frame_stream() {
        let (mut tx, rx) = futures_channel::mpsc::channel::<Vec<u8>>(2);
        let funnel = Rc::new(FunnelState::default());

        // `futures_channel` grants each sender a guaranteed slot on top of the
        // requested capacity, so fill by outcome rather than by count.
        let mut accepted = 0u8;
        loop {
            match funnel_frame(&mut tx, &funnel, alloc::vec![accepted]) {
                Funneled::Accepted => accepted += 1,
                Funneled::Overflowed => break,
                Funneled::Closed => panic!("funnel closed before it ever filled"),
                // Only `funnel_text` weighs a frame; these are one byte each.
                Funneled::Oversize => unreachable!(),
            }
            assert!(accepted < 16, "funnel never filled");
        }
        assert!(accepted >= 2, "capacity must be honoured before overflow");

        // Frames still arriving before the socket closes are discarded, not
        // re-reported — one warning per overflow, not per frame.
        assert_eq!(
            funnel_frame(&mut tx, &funnel, alloc::vec![u8::MAX]),
            Funneled::Closed
        );

        // The engine's `recv` sees end-of-stream at once, not the backlog.
        let mut conn = funnel_connection(rx, funnel);
        assert!(
            conn.recv().await.unwrap().is_none(),
            "an overflowed funnel must end the stream, not drain it"
        );
    }

    /// A connection over `funnel`, for the `recv` seam alone.
    fn funnel_connection(frames: FrameRx, funnel: Rc<FunnelState>) -> WasmWsConnection {
        WasmWsConnection {
            ws: web_sys::WebSocket::new(PENDING_URL).unwrap(),
            frames,
            funnel,
            shared: shared(),
            peer: PeerInfo::default(),
            _callbacks: Vec::new(),
            _plain_callbacks: Vec::new(),
        }
    }

    /// Regression: the aggregate byte budget refuses max-size frames long
    /// before the frame cap would.
    #[wasm_bindgen_test]
    async fn the_funnel_bounds_total_queued_bytes() {
        let (mut tx, _rx) = futures_channel::mpsc::channel::<Vec<u8>>(FRAME_QUEUE_CAP);
        let funnel = Rc::new(FunnelState::default());

        // Frames at the per-frame ceiling, far fewer than the frame cap, so
        // only the aggregate can refuse them.
        let frame_len = MAX_FRAME_BYTES;
        let mut queued = 0usize;
        loop {
            match funnel_frame(&mut tx, &funnel, alloc::vec![0u8; frame_len]) {
                Funneled::Accepted => queued += frame_len,
                Funneled::Overflowed => break,
                other => panic!("unexpected outcome: {other:?}"),
            }
            assert!(
                queued <= FRAME_QUEUE_BYTES,
                "the byte budget must never be exceeded"
            );
        }

        assert!(
            queued + frame_len > FRAME_QUEUE_BYTES,
            "overflow must be the budget talking, not the frame count"
        );
        assert_eq!(funnel.queued_bytes.get(), queued);
    }

    /// The counter falls as the engine drains, so a funnel that stays level does
    /// not creep toward the budget over a long connection.
    #[wasm_bindgen_test]
    async fn draining_returns_the_budget() {
        let (mut tx, rx) = futures_channel::mpsc::channel::<Vec<u8>>(FRAME_QUEUE_CAP);
        let funnel = Rc::new(FunnelState::default());

        assert_eq!(
            funnel_frame(&mut tx, &funnel, alloc::vec![0u8; 4096]),
            Funneled::Accepted
        );
        assert_eq!(funnel.queued_bytes.get(), 4096);

        let mut conn = funnel_connection(rx, funnel.clone());
        assert_eq!(conn.recv().await.unwrap().map(|f| f.len()), Some(4096));
        assert_eq!(funnel.queued_bytes.get(), 0, "drained bytes are returned");
    }

    /// Regression: a frame past the per-frame ceiling ends the stream the way
    /// an overflow does — announced once, not per late frame.
    #[wasm_bindgen_test]
    async fn oversize_frame_ends_the_frame_stream() {
        let (mut tx, rx) = futures_channel::mpsc::channel::<Vec<u8>>(FRAME_QUEUE_CAP);
        let funnel = Rc::new(FunnelState::default());

        assert_eq!(
            funnel_text(&mut tx, &funnel, &js_sys::JsString::from("keep me")),
            Funneled::Accepted
        );
        // Built JS-side: the point of the ceiling is that this never becomes a
        // `String`, so the test must not make one either.
        let oversize = js_sys::JsString::from("x").repeat(MAX_FRAME_BYTES as i32 + 1);
        assert_eq!(funnel_text(&mut tx, &funnel, &oversize), Funneled::Oversize);
        assert_eq!(
            funnel_text(&mut tx, &funnel, &js_sys::JsString::from("late")),
            Funneled::Closed
        );

        // Same teardown as a full funnel: EOF at once, backlog dropped.
        let mut conn = funnel_connection(rx, funnel);
        assert!(
            conn.recv().await.unwrap().is_none(),
            "an oversize frame must end the stream, not stall it"
        );
    }

    /// The JS-side length is in UTF-16 code units and undercounts a multi-byte
    /// frame; the ceiling is bytes, so the copy is re-checked exactly.
    #[wasm_bindgen_test]
    fn multibyte_frame_over_the_byte_ceiling_is_refused() {
        let (mut tx, _rx) = futures_channel::mpsc::channel::<Vec<u8>>(FRAME_QUEUE_CAP);
        let funnel = Rc::new(FunnelState::default());

        // One code unit, three UTF-8 bytes: half the ceiling in units is 1.5×
        // the ceiling in bytes.
        let frame = js_sys::JsString::from("€").repeat((MAX_FRAME_BYTES / 2) as i32);
        assert!(
            frame.length() as usize <= MAX_FRAME_BYTES,
            "the JS-side gate must not be what refuses this"
        );

        assert_eq!(funnel_text(&mut tx, &funnel, &frame), Funneled::Oversize);
    }

    /// The ceiling is inclusive: a frame exactly at it still reaches the engine.
    #[wasm_bindgen_test]
    async fn frame_at_the_ceiling_is_accepted() {
        let (mut tx, mut rx) = futures_channel::mpsc::channel::<Vec<u8>>(FRAME_QUEUE_CAP);
        let funnel = Rc::new(FunnelState::default());

        let frame = js_sys::JsString::from("x").repeat(MAX_FRAME_BYTES as i32);
        assert_eq!(funnel_text(&mut tx, &funnel, &frame), Funneled::Accepted);
        assert_eq!(
            rx.next().await.map(|frame| frame.len()),
            Some(MAX_FRAME_BYTES)
        );
    }

    /// Regression: `disconnect()` reaches a still-pending handshake, and the
    /// dial reports a *terminal* failure — so the engine stops on this attempt
    /// rather than sleeping a reconnect backoff first.
    #[wasm_bindgen_test]
    async fn disconnect_interrupts_a_pending_dial() {
        let shared = shared();
        let dialer = WasmWsDialer {
            url: PENDING_URL.to_string(),
            shared: shared.clone(),
        };

        let dial = dialer.connect();
        futures_util::pin_mut!(dial);

        // Let the prologue run: it publishes the socket, then parks on the
        // handshake. PENDING_URL is unroutable, so nothing can resolve it.
        let settle = WasmAdapter.sleep(core::time::Duration::from_millis(50));
        let dial = match futures_util::future::select(dial, settle).await {
            futures_util::future::Either::Left(_) => {
                panic!("a dial to an unroutable address must not settle on its own")
            }
            futures_util::future::Either::Right(((), dial)) => dial,
        };

        let pending = shared
            .ws
            .borrow()
            .clone()
            .expect("the dialer must publish the socket before awaiting the handshake");
        assert_eq!(
            pending.ready_state(),
            web_sys::WebSocket::CONNECTING,
            "the published socket is still mid-handshake"
        );

        // What `disconnect()` does; reaching the in-flight socket is the point.
        shared.stopped.set(true);
        shared.ws.borrow_mut().take();
        let _ = pending.close();
        shared.set_status(ConnectionStatus::Disconnected);

        let timeout = WasmAdapter.sleep(core::time::Duration::from_millis(5_000));
        match futures_util::future::select(dial, timeout).await {
            futures_util::future::Either::Left((Err(e), _)) => assert_eq!(
                e,
                TransportError::Closed,
                "a stopped bridge must fail the dial terminally, not transiently"
            ),
            futures_util::future::Either::Left((Ok(_), _)) => {
                panic!("an interrupted dial must not yield a connection")
            }
            futures_util::future::Either::Right(_) => {
                panic!("closing the pending socket must settle the dial")
            }
        }
        assert!(shared.ws.borrow().is_none());
    }

    /// Regression: a rejected URL is permanent, so the dial fails *terminally*
    /// and reports it. As `Io` the engine redialed forever on `"connecting"`.
    #[wasm_bindgen_test]
    async fn rejected_url_fails_the_dial_terminally() {
        let shared = shared();
        let dialer = WasmWsDialer {
            url: REJECTED_URL.to_string(),
            shared: shared.clone(),
        };

        assert_eq!(
            dialer.connect().await.err(),
            Some(TransportError::Closed),
            "a URL no redial can fix must not be reported as transient"
        );
        assert_eq!(
            shared.status.get(),
            ConnectionStatus::Disconnected,
            "the dial that stops the engine is the only place left to report it"
        );
        assert!(
            shared.ws.borrow().is_none(),
            "a dial that never built a socket must publish nothing"
        );
    }

    /// The same failure end-to-end through the JS surface, with reconnect on:
    /// the bridge settles on `"disconnected"` instead of spinning redials.
    #[wasm_bindgen_test]
    async fn bridge_with_rejected_url_settles_disconnected() {
        let (db, _runner) = aimdb_core::AimDbBuilder::new()
            .runtime(Arc::new(WasmAdapter))
            .build()
            .await
            .unwrap();
        let bridge = WsBridge::new_internal(
            db,
            BTreeMap::new(),
            SchemaRegistry::new(),
            REJECTED_URL,
            JsValue::NULL,
        )
        .unwrap();

        // The engine dials from a spawned task; the failure is synchronous
        // once it runs, so a bounded number of microtask turns suffices.
        for _ in 0..32 {
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
                .await;
            if bridge.status() == "disconnected" {
                break;
            }
        }
        assert_eq!(
            bridge.status(),
            "disconnected",
            "a permanently unusable URL must not leave JS reading \"connecting\""
        );
    }

    /// The same interruption end-to-end through the JS surface: `disconnect()`
    /// while connecting closes the socket it published rather than leaving it to
    /// the browser's connect timeout.
    #[wasm_bindgen_test]
    async fn bridge_disconnect_closes_the_pending_socket() {
        let (db, _runner) = aimdb_core::AimDbBuilder::new()
            .runtime(Arc::new(WasmAdapter))
            .build()
            .await
            .unwrap();
        let bridge = WsBridge::new_internal(
            db,
            BTreeMap::new(),
            SchemaRegistry::new(),
            PENDING_URL,
            JsValue::NULL,
        )
        .unwrap();

        // The engine dials from a spawned task; give it a bounded number of
        // microtask turns to reach the publish.
        let mut pending = None;
        for _ in 0..32 {
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
                .await;
            pending = bridge.shared.ws.borrow().clone();
            if pending.is_some() {
                break;
            }
        }
        let pending = pending.expect("the engine's dial must publish its socket");
        assert_eq!(pending.ready_state(), web_sys::WebSocket::CONNECTING);

        bridge.disconnect();

        assert_eq!(bridge.status(), "disconnected");
        assert!(bridge.shared.ws.borrow().is_none());
        assert_ne!(
            pending.ready_state(),
            web_sys::WebSocket::CONNECTING,
            "disconnect() must interrupt the handshake, not wait out the browser"
        );
    }

    /// `disconnect()` while the bridge is still connecting settles on
    /// `Disconnected` and rejects further commands, with no socket left behind.
    #[wasm_bindgen_test]
    async fn bridge_disconnect_while_connecting_settles_disconnected() {
        // No records configured, so the runner owns no futures — dropping it is
        // the whole of "running" this database.
        let (db, _runner) = aimdb_core::AimDbBuilder::new()
            .runtime(Arc::new(WasmAdapter))
            .build()
            .await
            .unwrap();
        let bridge = WsBridge::new_internal(
            db,
            BTreeMap::new(),
            SchemaRegistry::new(),
            PENDING_URL,
            JsValue::NULL,
        )
        .unwrap();

        // Let the engine start its (never-completing) dial.
        let _ =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
        assert_eq!(bridge.status(), "connecting");

        bridge.disconnect();
        assert_eq!(bridge.status(), "disconnected");
        assert!(bridge.shared.stopped.get());
        assert!(bridge.shared.ws.borrow().is_none());
        assert!(bridge.write("test::topic", JsValue::NULL).is_err());
    }
}
