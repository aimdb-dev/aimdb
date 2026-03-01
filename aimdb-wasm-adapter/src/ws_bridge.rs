//! WebSocket bridge connecting browser AimDB to a server instance.
//!
//! `WsBridge` wraps `web_sys::WebSocket` and speaks the shared wire protocol
//! from [`aimdb_ws_protocol`] (`ServerMessage` / `ClientMessage`). It maps
//! incoming server data to local buffer pushes and forwards local writes to
//! the server.
//!
//! # Modes
//!
//! - **Synchronized** — browser instance mirrors server records.
//! - **Hybrid** — works offline with local records, syncs when connected.
//!
//! See design doc §7 for full details.

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use aimdb_core::builder::AimDb;
use aimdb_data_contracts::dispatch_streamable;

use crate::WasmAdapter;

// ─── Connection status ────────────────────────────────────────────────────

/// Observable connection state (matches design doc §7.1).
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

// ─── Wire protocol (shared with aimdb-websocket-connector) ───────────────

use aimdb_ws_protocol::{ClientMessage, ServerMessage};

// ─── Bridge configuration ─────────────────────────────────────────────────

/// Configuration for `WasmDb.connectBridge()`.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BridgeOptions {
    /// MQTT-style topic patterns to subscribe to (e.g. `["sensors/#"]`).
    #[serde(default)]
    pub subscribe_topics: Vec<String>,
    /// Re-connect automatically on close (default: true).
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Request snapshots on (re)connect (default: true).
    #[serde(default = "default_true")]
    pub late_join: bool,
    /// Maximum queued writes while disconnected (default: 256).
    #[serde(default = "default_queue_size")]
    pub max_offline_queue: usize,
    /// Keepalive interval in milliseconds (default: 30 000).
    #[serde(default = "default_keepalive_ms")]
    pub keepalive_ms: u32,
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

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            subscribe_topics: Vec::new(),
            auto_reconnect: true,
            late_join: true,
            max_offline_queue: 256,
            keepalive_ms: 30_000,
        }
    }
}

// ─── Bridge state ─────────────────────────────────────────────────────────

struct BridgeState {
    status: ConnectionStatus,
    pending_writes: alloc::collections::VecDeque<ClientMessage>,
    backoff_index: usize,
    /// Active keepalive interval ID (cleared on close/disconnect).
    keepalive_id: Option<i32>,
    /// Closures retained to prevent GC.
    _on_open: Option<Closure<dyn FnMut()>>,
    _on_message: Option<Closure<dyn FnMut(web_sys::MessageEvent)>>,
    _on_close: Option<Closure<dyn FnMut()>>,
    _on_error: Option<Closure<dyn FnMut()>>,
}

// ─── Shared reconnect context ─────────────────────────────────────────────

/// Shared state needed by both the initial connect and reconnect paths.
///
/// Wrapped in `Rc` so closures can cheaply reference it without cloning
/// every field individually (reduces parameter explosion).
struct SharedCtx {
    db: AimDb<WasmAdapter>,
    schema_map: Vec<(String, String)>,
    state: Rc<RefCell<BridgeState>>,
    on_status: Rc<RefCell<Option<js_sys::Function>>>,
    config: BridgeOptions,
    backoff: Vec<u32>,
    url: String,
    ws_cell: Rc<RefCell<web_sys::WebSocket>>,
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
///   lateJoin: true,
/// });
/// bridge.onStatusChange((status) => updateIndicator(status));
/// // ...
/// bridge.disconnect();
/// ```
#[wasm_bindgen]
pub struct WsBridge {
    ctx: Rc<SharedCtx>,
}

// SAFETY: wasm32-unknown-unknown is single-threaded.
unsafe impl Send for WsBridge {}
unsafe impl Sync for WsBridge {}

#[wasm_bindgen]
impl WsBridge {
    /// Register a callback for connection status changes.
    ///
    /// ```ts
    /// bridge.onStatusChange((status: string) => { /* … */ });
    /// ```
    #[wasm_bindgen(js_name = "onStatusChange")]
    pub fn on_status_change(&self, callback: js_sys::Function) {
        *self.ctx.on_status.borrow_mut() = Some(callback);
    }

    /// Send a value to the server for a given topic.
    ///
    /// If the WebSocket is disconnected, the message is queued (up to
    /// `maxOfflineQueue`). Queued messages are flushed on reconnect.
    pub fn write(&self, topic: &str, payload: JsValue) -> Result<(), JsError> {
        let json_payload: serde_json::Value = serde_wasm_bindgen::from_value(payload)
            .map_err(|e| JsError::new(&format!("Payload serialization failed: {e}")))?;

        let msg = ClientMessage::Write {
            topic: topic.to_string(),
            payload: json_payload,
        };

        let state = self.ctx.state.borrow();
        if state.status == ConnectionStatus::Connected {
            drop(state);
            send_json(&self.ctx.ws_cell.borrow(), &msg)?;
        } else {
            drop(state);
            let mut state = self.ctx.state.borrow_mut();
            if state.pending_writes.len() < self.ctx.config.max_offline_queue {
                state.pending_writes.push_back(msg);
            }
            // else: drop (overflow policy)
        }
        Ok(())
    }

    /// Close the WebSocket and stop reconnection attempts.
    pub fn disconnect(&self) {
        let mut state = self.ctx.state.borrow_mut();
        state.status = ConnectionStatus::Disconnected;
        // Clear keepalive timer
        if let Some(id) = state.keepalive_id.take() {
            if let Some(window) = web_sys::window() {
                window.clear_interval_with_handle(id);
            }
        }
        // Drop closures to break Rc cycles
        state._on_open = None;
        state._on_message = None;
        state._on_close = None;
        state._on_error = None;
        drop(state);

        let _ = self.ctx.ws_cell.borrow().close();
        emit_status(&self.ctx.on_status, ConnectionStatus::Disconnected);
    }

    /// Current connection status as a string.
    pub fn status(&self) -> String {
        self.ctx.state.borrow().status.as_str().to_string()
    }
}

// ─── Internal constructor ──────────────────────────────────────────────────

impl WsBridge {
    /// Create a new bridge (called from `WasmDb::connect_bridge`).
    pub(crate) fn new_internal(
        db: AimDb<WasmAdapter>,
        schema_map: Vec<(String, String)>,
        url: &str,
        options: JsValue,
    ) -> Result<WsBridge, JsError> {
        let config: BridgeOptions = if options.is_undefined() || options.is_null() {
            BridgeOptions::default()
        } else {
            serde_wasm_bindgen::from_value(options)
                .map_err(|e| JsError::new(&format!("Invalid bridge options: {e}")))?
        };

        let ws = web_sys::WebSocket::new(url)
            .map_err(|e| JsError::new(&format!("WebSocket open failed: {e:?}")))?;

        let ws_cell = Rc::new(RefCell::new(ws));
        let state = Rc::new(RefCell::new(BridgeState {
            status: ConnectionStatus::Connecting,
            pending_writes: alloc::collections::VecDeque::new(),
            backoff_index: 0,
            keepalive_id: None,
            _on_open: None,
            _on_message: None,
            _on_close: None,
            _on_error: None,
        }));
        let on_status: Rc<RefCell<Option<js_sys::Function>>> = Rc::new(RefCell::new(None));
        let backoff = alloc::vec![500, 1_000, 2_000, 4_000, 8_000];

        let ctx = Rc::new(SharedCtx {
            db,
            schema_map,
            state,
            on_status,
            config,
            backoff,
            url: url.to_string(),
            ws_cell,
        });

        install_ws_callbacks(&ctx);

        Ok(WsBridge { ctx })
    }
}

// ─── Callback installation (shared by connect + reconnect) ────────────────

/// Install WebSocket event callbacks on the current socket in `ctx.ws_cell`.
///
/// Extracted as a free function so both the initial `connect` and
/// `schedule_reconnect` paths can call it.
fn install_ws_callbacks(ctx: &Rc<SharedCtx>) {
    let ws = ctx.ws_cell.borrow();

    // on_open
    let on_open = {
        let ctx = ctx.clone();
        Closure::wrap(Box::new(move || {
            {
                let mut s = ctx.state.borrow_mut();
                s.status = ConnectionStatus::Connected;
                s.backoff_index = 0;

                // Flush pending writes
                let ws = ctx.ws_cell.borrow();
                while let Some(msg) = s.pending_writes.pop_front() {
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = ws.send_with_str(&json);
                    }
                }
            }

            // Subscribe to configured topics
            let topics = &ctx.config.subscribe_topics;
            if !topics.is_empty() {
                let sub = ClientMessage::Subscribe {
                    topics: topics.clone(),
                };
                if let Ok(json) = serde_json::to_string(&sub) {
                    let _ = ctx.ws_cell.borrow().send_with_str(&json);
                }
            }

            // Start keepalive ping timer
            if ctx.config.keepalive_ms > 0 {
                let ws_for_ping = ctx.ws_cell.clone();
                let ping_closure = Closure::wrap(Box::new(move || {
                    let ping = ClientMessage::Ping;
                    if let Ok(json) = serde_json::to_string(&ping) {
                        let _ = ws_for_ping.borrow().send_with_str(&json);
                    }
                }) as Box<dyn FnMut()>);

                if let Some(window) = web_sys::window() {
                    if let Ok(id) = window.set_interval_with_callback_and_timeout_and_arguments_0(
                        ping_closure.as_ref().unchecked_ref(),
                        ctx.config.keepalive_ms as i32,
                    ) {
                        ctx.state.borrow_mut().keepalive_id = Some(id);
                    }
                }
                ping_closure.forget();
            }

            emit_status(&ctx.on_status, ConnectionStatus::Connected);
        }) as Box<dyn FnMut()>)
    };
    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

    // on_message — route server data to local buffers (no JsValue hop)
    let on_message = {
        let ctx = ctx.clone();
        Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
            if let Some(text) = event.data().as_string() {
                if let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) {
                    handle_server_message(&ctx.db, &ctx.schema_map, msg);
                }
            }
        }) as Box<dyn FnMut(web_sys::MessageEvent)>)
    };
    ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

    // on_close — reconnect with exponential backoff
    let on_close = {
        let ctx = ctx.clone();
        Closure::wrap(Box::new(move || {
            let current = ctx.state.borrow().status;
            if current == ConnectionStatus::Disconnected {
                return; // user-initiated disconnect — don't reconnect
            }

            // Clear keepalive timer
            if let Some(id) = ctx.state.borrow_mut().keepalive_id.take() {
                if let Some(window) = web_sys::window() {
                    window.clear_interval_with_handle(id);
                }
            }

            if ctx.config.auto_reconnect {
                let delay = {
                    let mut s = ctx.state.borrow_mut();
                    s.status = ConnectionStatus::Reconnecting;
                    let d = ctx
                        .backoff
                        .get(s.backoff_index)
                        .copied()
                        .unwrap_or(*ctx.backoff.last().unwrap_or(&8_000));
                    s.backoff_index = (s.backoff_index + 1).min(ctx.backoff.len() - 1);
                    d
                };

                emit_status(&ctx.on_status, ConnectionStatus::Reconnecting);
                schedule_reconnect(ctx.clone(), delay);
            } else {
                ctx.state.borrow_mut().status = ConnectionStatus::Disconnected;
                emit_status(&ctx.on_status, ConnectionStatus::Disconnected);
            }
        }) as Box<dyn FnMut()>)
    };
    ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

    // on_error — WebSocket always fires `close` after `error`, so just log.
    let on_error = {
        Closure::wrap(Box::new(move || {
            web_sys::console::warn_1(&"WsBridge: WebSocket error".into());
        }) as Box<dyn FnMut()>)
    };
    ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    // Store closures to prevent GC
    let mut state = ctx.state.borrow_mut();
    state._on_open = Some(on_open);
    state._on_message = Some(on_message);
    state._on_close = Some(on_close);
    state._on_error = Some(on_error);
}

// ─── Reconnect ─────────────────────────────────────────────────────────────

/// Schedule a reconnect attempt after `delay_ms` using `setTimeout`.
///
/// On success: swaps the new socket into `ctx.ws_cell` and re-installs
/// callbacks. On failure: increments backoff and schedules the next attempt.
///
/// Uses `Closure::once` + `.forget()` — each attempt leaks a few bytes.
/// With 5 backoff steps capped at 8 s, worst case is ~5 closures in flight.
fn schedule_reconnect(ctx: Rc<SharedCtx>, delay_ms: u32) {
    let closure = Closure::once(move || {
        // Guard: user may have called disconnect() during the delay
        if ctx.state.borrow().status == ConnectionStatus::Disconnected {
            return;
        }

        match web_sys::WebSocket::new(&ctx.url) {
            Ok(new_ws) => {
                *ctx.ws_cell.borrow_mut() = new_ws;
                install_ws_callbacks(&ctx);
            }
            Err(e) => {
                web_sys::console::error_1(&format!("WsBridge: reconnect failed: {e:?}").into());
                // Try again with increased backoff
                let next_delay = {
                    let mut s = ctx.state.borrow_mut();
                    let d = ctx
                        .backoff
                        .get(s.backoff_index)
                        .copied()
                        .unwrap_or(*ctx.backoff.last().unwrap_or(&8_000));
                    s.backoff_index = (s.backoff_index + 1).min(ctx.backoff.len() - 1);
                    d
                };
                schedule_reconnect(ctx, next_delay);
            }
        }
    });

    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            delay_ms as i32,
        );
    }
    closure.forget(); // prevent GC — fires once, then drops itself
}

// ─── Server message handling ───────────────────────────────────────────────

/// Route a decoded server message to the local database.
///
/// Uses direct `serde_json::from_value::<T>()` → buffer push, bypassing the
/// `JsValue` intermediary that the old code path used.
fn handle_server_message(
    db: &AimDb<WasmAdapter>,
    schema_map: &[(String, String)],
    msg: ServerMessage,
) {
    match msg {
        ServerMessage::Data { topic, payload, .. } | ServerMessage::Snapshot { topic, payload } => {
            if let Some(payload) = payload {
                let schema = schema_map
                    .iter()
                    .find(|(k, _)| k == &topic)
                    .map(|(_, v)| v.as_str());

                if let Some(schema) = schema {
                    dispatch_streamable!(schema, |T| {
                        produce_from_json::<T>(db, &topic, payload.clone());
                    });
                }
            }
        }
        ServerMessage::Subscribed { .. } => {
            // ACK — no action needed beyond status change (already handled in on_open).
        }
        ServerMessage::Error { message, topic, .. } => {
            let detail = match topic {
                Some(t) => format!("WsBridge error on topic '{t}': {message}"),
                None => format!("WsBridge error: {message}"),
            };
            web_sys::console::error_1(&detail.into());
        }
        ServerMessage::Pong => {
            // Keepalive ACK — reset timer if needed.
        }
    }
}

/// Deserialize `serde_json::Value` → `T` and push to the record buffer.
///
/// This is the fast path for incoming server data — no `JsValue` hop.
fn produce_from_json<T>(db: &AimDb<WasmAdapter>, key: &str, json: serde_json::Value)
where
    T: Send + Sync + 'static + Debug + Clone + DeserializeOwned,
{
    if let Ok(val) = serde_json::from_value::<T>(json) {
        let inner = db.inner();
        if let Ok(typed) = inner.get_typed_record_by_key::<T, WasmAdapter>(key) {
            crate::bindings::poll_sync(typed.produce(val));
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────

/// Serialize a `ClientMessage` and send as text frame.
fn send_json(ws: &web_sys::WebSocket, msg: &ClientMessage) -> Result<(), JsError> {
    let json = serde_json::to_string(msg)
        .map_err(|e| JsError::new(&format!("JSON serialization failed: {e}")))?;
    ws.send_with_str(&json)
        .map_err(|e| JsError::new(&format!("WebSocket send failed: {e:?}")))?;
    Ok(())
}

/// Emit status to the registered callback.
fn emit_status(on_status: &Rc<RefCell<Option<js_sys::Function>>>, status: ConnectionStatus) {
    if let Some(cb) = on_status.borrow().as_ref() {
        let _ = cb.call1(&JsValue::NULL, &JsValue::from_str(status.as_str()));
    }
}
