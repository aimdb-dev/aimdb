//! WebSocket bridge connecting browser AimDB to a server instance.
//!
//! `WsBridge` wraps `web_sys::WebSocket` and speaks the same wire protocol as
//! `aimdb-websocket-connector` (`ServerMessage` / `ClientMessage`). It maps
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

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::bindings::WasmDb;

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

// ─── Wire protocol (mirrors aimdb-websocket-connector/src/protocol.rs) ───

/// Server → Client message.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Data {
        topic: String,
        payload: Option<serde_json::Value>,
        #[allow(dead_code)]
        ts: u64,
    },
    Snapshot {
        topic: String,
        payload: Option<serde_json::Value>,
    },
    Subscribed {
        #[allow(dead_code)]
        topics: Vec<String>,
    },
    Error {
        #[allow(dead_code)]
        code: String,
        #[allow(dead_code)]
        topic: Option<String>,
        #[allow(dead_code)]
        message: String,
    },
    Pong,
}

/// Client → Server message.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Subscribe {
        topics: Vec<String>,
    },
    #[allow(dead_code)]
    Unsubscribe {
        topics: Vec<String>,
    },
    Write {
        topic: String,
        payload: serde_json::Value,
    },
    #[allow(dead_code)]
    Ping,
}

// ─── Bridge configuration ─────────────────────────────────────────────────

/// Configuration for `WsBridge.connect()`.
#[derive(Deserialize)]
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
    /// Closures retained to prevent GC.
    _on_open: Option<Closure<dyn FnMut()>>,
    _on_message: Option<Closure<dyn FnMut(web_sys::MessageEvent)>>,
    _on_close: Option<Closure<dyn FnMut()>>,
    _on_error: Option<Closure<dyn FnMut()>>,
}

// ─── WsBridge ─────────────────────────────────────────────────────────────

/// WebSocket bridge connecting the in-browser AimDB to a remote server.
///
/// # Example (TypeScript)
/// ```ts
/// const bridge = WsBridge.connect(db, 'wss://api.example.com/ws', {
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
    ws: web_sys::WebSocket,
    db: Rc<RefCell<WasmDb>>,
    config: BridgeOptions,
    state: Rc<RefCell<BridgeState>>,
    /// User-provided status change callback.
    on_status_change: Rc<RefCell<Option<js_sys::Function>>>,
    /// Backoff schedule in milliseconds.
    backoff: Vec<u32>,
}

// SAFETY: wasm32-unknown-unknown is single-threaded.
unsafe impl Send for WsBridge {}
unsafe impl Sync for WsBridge {}

#[wasm_bindgen]
impl WsBridge {
    /// Open a WebSocket connection and begin synchronising records.
    ///
    /// - `db` — a built `WasmDb` instance.
    /// - `url` — WebSocket endpoint (e.g. `wss://api.example.com/ws`).
    /// - `options` — optional JS object with `subscribeTopics`, `autoReconnect`,
    ///   `lateJoin`, etc.
    pub fn connect(db: WasmDb, url: &str, options: JsValue) -> Result<WsBridge, JsError> {
        let config: BridgeOptions = if options.is_undefined() || options.is_null() {
            BridgeOptions::default()
        } else {
            serde_wasm_bindgen::from_value(options)
                .map_err(|e| JsError::new(&format!("Invalid bridge options: {e}")))?
        };

        let ws = web_sys::WebSocket::new(url)
            .map_err(|e| JsError::new(&format!("WebSocket open failed: {e:?}")))?;

        // We use text (JSON) frames — no binary type configuration needed.

        let db = Rc::new(RefCell::new(db));
        let state = Rc::new(RefCell::new(BridgeState {
            status: ConnectionStatus::Connecting,
            pending_writes: alloc::collections::VecDeque::new(),
            backoff_index: 0,
            _on_open: None,
            _on_message: None,
            _on_close: None,
            _on_error: None,
        }));
        let on_status_change: Rc<RefCell<Option<js_sys::Function>>> = Rc::new(RefCell::new(None));
        let backoff = alloc::vec![500, 1_000, 2_000, 4_000, 8_000];

        let mut bridge = WsBridge {
            ws,
            db,
            config,
            state,
            on_status_change,
            backoff,
        };

        bridge.install_callbacks();
        Ok(bridge)
    }

    /// Register a callback for connection status changes.
    ///
    /// ```ts
    /// bridge.onStatusChange((status: string) => { /* … */ });
    /// ```
    #[wasm_bindgen(js_name = "onStatusChange")]
    pub fn on_status_change(&self, callback: js_sys::Function) {
        *self.on_status_change.borrow_mut() = Some(callback);
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

        let state = self.state.borrow();
        if state.status == ConnectionStatus::Connected {
            self.send_json(&msg)?;
        } else {
            drop(state);
            let mut state = self.state.borrow_mut();
            if state.pending_writes.len() < self.config.max_offline_queue {
                state.pending_writes.push_back(msg);
            }
            // else: drop (overflow policy)
        }
        Ok(())
    }

    /// Close the WebSocket and stop reconnection attempts.
    pub fn disconnect(&self) {
        let mut state = self.state.borrow_mut();
        state.status = ConnectionStatus::Disconnected;
        state._on_open = None;
        state._on_message = None;
        state._on_close = None;
        state._on_error = None;
        drop(state);

        let _ = self.ws.close();
        self.emit_status(ConnectionStatus::Disconnected);
    }

    /// Current connection status as a string.
    pub fn status(&self) -> String {
        self.state.borrow().status.as_str().to_string()
    }
}

// ─── Internal ─────────────────────────────────────────────────────────────

impl WsBridge {
    fn install_callbacks(&mut self) {
        // on_open
        {
            let state = self.state.clone();
            let ws = self.ws.clone();
            let topics = self.config.subscribe_topics.clone();
            let on_status = self.on_status_change.clone();
            let closure = Closure::wrap(Box::new(move || {
                {
                    let mut s = state.borrow_mut();
                    s.status = ConnectionStatus::Connected;
                    s.backoff_index = 0;

                    // Flush pending writes
                    while let Some(msg) = s.pending_writes.pop_front() {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = ws.send_with_str(&json);
                        }
                    }
                }

                // Subscribe to configured topics
                if !topics.is_empty() {
                    let sub = ClientMessage::Subscribe {
                        topics: topics.clone(),
                    };
                    if let Ok(json) = serde_json::to_string(&sub) {
                        let _ = ws.send_with_str(&json);
                    }
                }

                // Notify status change
                if let Some(cb) = on_status.borrow().as_ref() {
                    let _ = cb.call1(
                        &JsValue::NULL,
                        &JsValue::from_str(ConnectionStatus::Connected.as_str()),
                    );
                }
            }) as Box<dyn FnMut()>);
            self.ws.set_onopen(Some(closure.as_ref().unchecked_ref()));
            self.state.borrow_mut()._on_open = Some(closure);
        }

        // on_message
        {
            let db = self.db.clone();
            let closure = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                if let Some(text) = event.data().as_string() {
                    if let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) {
                        Self::handle_server_message(&db, msg);
                    }
                }
            }) as Box<dyn FnMut(web_sys::MessageEvent)>);
            self.ws
                .set_onmessage(Some(closure.as_ref().unchecked_ref()));
            self.state.borrow_mut()._on_message = Some(closure);
        }

        // on_close
        {
            let state = self.state.clone();
            let on_status = self.on_status_change.clone();
            let auto_reconnect = self.config.auto_reconnect;
            let backoff = self.backoff.clone();
            let closure = Closure::wrap(Box::new(move || {
                let current = state.borrow().status;
                if current == ConnectionStatus::Disconnected {
                    return; // user-initiated disconnect
                }

                if auto_reconnect {
                    let mut s = state.borrow_mut();
                    s.status = ConnectionStatus::Reconnecting;
                    let delay = backoff
                        .get(s.backoff_index)
                        .copied()
                        .unwrap_or(*backoff.last().unwrap_or(&8_000));
                    s.backoff_index = (s.backoff_index + 1).min(backoff.len() - 1);
                    drop(s);

                    if let Some(cb) = on_status.borrow().as_ref() {
                        let _ = cb.call1(
                            &JsValue::NULL,
                            &JsValue::from_str(ConnectionStatus::Reconnecting.as_str()),
                        );
                    }

                    // Schedule reconnect (actual reconnect requires creating a new WS —
                    // this is a placeholder; full reconnect needs the URL + re-install).
                    let _delay = delay;
                    // TODO: implement setTimeout-based reconnect with new WebSocket
                } else {
                    state.borrow_mut().status = ConnectionStatus::Disconnected;
                    if let Some(cb) = on_status.borrow().as_ref() {
                        let _ = cb.call1(
                            &JsValue::NULL,
                            &JsValue::from_str(ConnectionStatus::Disconnected.as_str()),
                        );
                    }
                }
            }) as Box<dyn FnMut()>);
            self.ws.set_onclose(Some(closure.as_ref().unchecked_ref()));
            self.state.borrow_mut()._on_close = Some(closure);
        }

        // on_error — WebSocket always fires `close` after `error`, so just log.
        {
            let closure = Closure::wrap(Box::new(move || {
                web_sys::console::warn_1(&"WsBridge: WebSocket error".into());
            }) as Box<dyn FnMut()>);
            self.ws.set_onerror(Some(closure.as_ref().unchecked_ref()));
            self.state.borrow_mut()._on_error = Some(closure);
        }
    }

    /// Route a decoded server message to the local database.
    fn handle_server_message(db: &Rc<RefCell<WasmDb>>, msg: ServerMessage) {
        match msg {
            ServerMessage::Data { topic, payload, .. }
            | ServerMessage::Snapshot { topic, payload } => {
                if let Some(payload) = payload {
                    let js = serde_wasm_bindgen::to_value(&payload).unwrap_or(JsValue::UNDEFINED);
                    let mut db = db.borrow_mut();
                    // Push to local buffer via set() which does contract validation.
                    let _ = db.set(&topic, js);
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

    /// Serialize a `ClientMessage` and send as text frame.
    fn send_json(&self, msg: &ClientMessage) -> Result<(), JsError> {
        let json = serde_json::to_string(msg)
            .map_err(|e| JsError::new(&format!("JSON serialization failed: {e}")))?;
        self.ws
            .send_with_str(&json)
            .map_err(|e| JsError::new(&format!("WebSocket send failed: {e:?}")))?;
        Ok(())
    }

    /// Emit status to the registered callback.
    fn emit_status(&self, status: ConnectionStatus) {
        if let Some(cb) = self.on_status_change.borrow().as_ref() {
            let _ = cb.call1(&JsValue::NULL, &JsValue::from_str(status.as_str()));
        }
    }
}
