//! `#[wasm_bindgen]` TypeScript-facing API
//!
//! Exposes a high-level facade to JavaScript/TypeScript. Users do not interact
//! with `Arc`, `RecordRegistrar`, or feature flags — all of that is hidden
//! behind `WasmDb`, `configureRecord`, `get`, `set`, and `subscribe`.
//!
//! # Two-Phase Lifecycle
//!
//! 1. **Configuration** — `new WasmDb()` + `configureRecord(…)` calls collect
//!    record definitions without building the database.
//! 2. **Build** — `await db.build()` compiles the configuration into a live
//!    AimDB instance (buffers, records, typed storage).
//! 3. **Operation** — `get` / `set` / `subscribe` interact with the live
//!    database. Contract enforcement (Rust serde) happens at the WASM boundary.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::builder::{AimDb, AimDbBuilder};
use aimdb_core::record_id::StringKey;

use aimdb_ws_protocol::{ClientMessage, ServerMessage};

use crate::schema_registry::{SchemaOps, SchemaRegistry};
use crate::ws_bridge::WsBridge;
use crate::WasmAdapter;

// ─── Option parsing ───────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordOptions {
    schema_type: String,
    buffer: BufferOption,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BufferOption {
    /// Simple string: `"SingleLatest"`, `"Mailbox"`, `"SpmcRing"`
    Simple(String),
    /// Object: `{ type: "SpmcRing", capacity: 200 }`
    Config {
        r#type: String,
        capacity: Option<usize>,
    },
}

fn parse_buffer_cfg(opt: &BufferOption) -> Result<BufferCfg, JsError> {
    match opt {
        BufferOption::Simple(s) => match s.as_str() {
            "SingleLatest" => Ok(BufferCfg::SingleLatest),
            "Mailbox" => Ok(BufferCfg::Mailbox),
            "SpmcRing" => Ok(BufferCfg::SpmcRing { capacity: 1024 }),
            _ => Err(JsError::new(&format!("Unknown buffer type: {s}"))),
        },
        BufferOption::Config { r#type, capacity } => match r#type.as_str() {
            "SpmcRing" => Ok(BufferCfg::SpmcRing {
                capacity: capacity.unwrap_or(1024),
            }),
            "SingleLatest" => Ok(BufferCfg::SingleLatest),
            "Mailbox" => Ok(BufferCfg::Mailbox),
            _ => Err(JsError::new(&format!("Unknown buffer type: {}", r#type))),
        },
    }
}

fn is_known_schema(registry: &SchemaRegistry, name: &str) -> bool {
    registry.is_known(name)
}

// ─── Collected config (pre-build) ─────────────────────────────────────────

struct RecordConfig {
    key: String,
    schema_type: String,
    buffer_cfg: BufferCfg,
}

// ─── WasmDb ───────────────────────────────────────────────────────────────

/// AimDB instance compiled to WebAssembly.
///
/// # Example (TypeScript)
/// ```ts
/// const db = new WasmDb();
/// db.configureRecord('sensors.temperature.vienna', {
///   schemaType: 'temperature',
///   buffer: 'SingleLatest',
/// });
/// await db.build();
/// db.set('sensors.temperature.vienna', { celsius: 22.5, timestamp: Date.now() });
/// const t = db.get('sensors.temperature.vienna'); // validated by Rust serde
/// ```
#[wasm_bindgen]
pub struct WasmDb {
    /// Pre-build record configurations. `None` after `build()`.
    configs: Option<Vec<RecordConfig>>,
    /// Live database handle. `None` before `build()`.
    db: Option<AimDb<WasmAdapter>>,
    /// Maps record key → schema type name (always populated).
    schema_map: BTreeMap<String, String>,
    /// Type-erased dispatch registry built from the visitor pattern.
    registry: SchemaRegistry,
}

impl Default for WasmDb {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl WasmDb {
    /// Create a new (unconfigured) AimDB WASM instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmDb {
        WasmDb {
            configs: Some(Vec::new()),
            db: None,
            schema_map: BTreeMap::new(),
            registry: SchemaRegistry::build(),
        }
    }

    /// Register a record before building the database.
    ///
    /// `options` is a JS object:
    /// ```json
    /// {
    ///   "schemaType": "temperature",
    ///   "buffer": "SingleLatest"  // or { "type": "SpmcRing", "capacity": 100 }
    /// }
    /// ```
    #[wasm_bindgen(js_name = "configureRecord")]
    pub fn configure_record(&mut self, record_key: &str, options: JsValue) -> Result<(), JsError> {
        let configs = self
            .configs
            .as_mut()
            .ok_or_else(|| JsError::new("Cannot configure records after build()"))?;

        let opts: RecordOptions = serde_wasm_bindgen::from_value(options)
            .map_err(|e| JsError::new(&format!("Invalid options: {e}")))?;

        if !is_known_schema(&self.registry, &opts.schema_type) {
            return Err(JsError::new(&format!(
                "Unknown schema type: {}",
                opts.schema_type
            )));
        }

        let buffer_cfg = parse_buffer_cfg(&opts.buffer)?;

        self.schema_map
            .insert(record_key.to_string(), opts.schema_type.clone());

        configs.push(RecordConfig {
            key: record_key.to_string(),
            schema_type: opts.schema_type,
            buffer_cfg,
        });

        Ok(())
    }

    /// Build the database from the collected configuration.
    ///
    /// Must be called exactly once, after all `configureRecord()` calls and
    /// before any `get` / `set` / `subscribe`.
    pub async fn build(&mut self) -> Result<(), JsError> {
        let configs = self
            .configs
            .take()
            .ok_or_else(|| JsError::new("Database already built"))?;

        let rt = Arc::new(WasmAdapter);
        let mut builder = AimDbBuilder::new().runtime(rt);

        for config in &configs {
            apply_record_config(&self.registry, &mut builder, config)?;
        }

        let db = builder
            .build()
            .await
            .map_err(|e| JsError::new(&format!("Build failed: {e:?}")))?;

        self.db = Some(db);
        Ok(())
    }

    /// Get the current value of a record (returns JS object or `undefined`).
    ///
    /// The value is the latest snapshot — it does not wait for a new push.
    /// Returns `undefined` if no value has been produced yet.
    pub fn get(&self, record_key: &str) -> Result<JsValue, JsError> {
        let (db, ops) = self.resolve(record_key)?;
        (ops.get)(db, record_key)
    }

    /// Set a record value (validates via Rust serde deserialization).
    ///
    /// Throws `JsError` if the payload fails contract validation (e.g. missing
    /// required fields) or the record key is unknown.
    pub fn set(&mut self, record_key: &str, value: JsValue) -> Result<(), JsError> {
        let (db, ops) = self.resolve(record_key)?;
        (ops.set)(db, record_key, value)
    }

    /// Subscribe to record updates. Returns an unsubscribe function.
    ///
    /// `callback` is invoked on every buffer push with the validated value.
    pub fn subscribe(
        &self,
        record_key: &str,
        callback: &js_sys::Function,
    ) -> Result<JsValue, JsError> {
        let (db, ops) = self.resolve(record_key)?;
        (ops.subscribe)(db, record_key, callback)
    }

    /// Returns `true` if the database has been built.
    #[wasm_bindgen(js_name = "isBuilt")]
    pub fn is_built(&self) -> bool {
        self.db.is_some()
    }

    /// Discover topics served at `url` without building a full database.
    ///
    /// Opens a one-shot WebSocket, sends `ListTopics`, and resolves with
    /// `TopicInfo[]` once the server responds. Rejects after 30 s if no
    /// response arrives, or immediately on connection error.
    ///
    /// # Example (TypeScript)
    /// ```ts
    /// const wasm = await import("aimdb-wasm-adapter");
    /// await wasm.default();
    /// const topics = await wasm.WasmDb.discover("wss://api.example.com/ws");
    /// topics.forEach(t => db.configureRecord(t.entity, { schemaType: t.schemaType, buffer: "SingleLatest" }));
    /// ```
    pub async fn discover(url: &str) -> Result<JsValue, JsError> {
        wasm_bindgen_futures::JsFuture::from(discover_impl(url.to_string()))
            .await
            .map_err(|e| JsError::new(&format!("discover: {e:?}")))
    }

    /// Returns the list of schema type names known to this WASM adapter.
    ///
    /// Use this to filter discovered topics before calling `configureRecord` —
    /// topics whose `schemaType` is not in this list cannot be handled by the
    /// WASM runtime and should be skipped.
    #[wasm_bindgen(js_name = "knownSchemas")]
    pub fn known_schemas(&self) -> Vec<String> {
        self.registry.known_names().iter().map(|s| s.to_string()).collect()
    }

    /// Connect a WebSocket bridge to this database for server synchronization.
    ///
    /// The database remains usable for local `get()` / `set()` / `subscribe()`
    /// after the bridge is opened — the bridge gets a cheap clone of the
    /// internal `AimDb` handle (two `Arc` pointer copies).
    ///
    /// # Example (TypeScript)
    /// ```ts
    /// const bridge = db.connectBridge('wss://api.example.com/ws', {
    ///   subscribeTopics: ['sensors/#'],
    ///   autoReconnect: true,
    /// });
    /// bridge.onStatusChange((status) => console.log(status));
    /// ```
    #[wasm_bindgen(js_name = "connectBridge")]
    pub fn connect_bridge(&self, url: &str, options: JsValue) -> Result<WsBridge, JsError> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| JsError::new("Database not built. Call build() first."))?
            .clone(); // cheap: two Arc pointer copies

        let schema_map = self.schema_map.clone();
        let registry = SchemaRegistry::build();

        WsBridge::new_internal(db, schema_map, registry, url, options)
    }
}

// ─── discover_impl ────────────────────────────────────────────────────────

/// Build a one-shot WebSocket promise that resolves with `TopicInfo[]`.
///
/// Each callback pair (resolve, reject) is stored in an `Rc<RefCell<Option>>`
/// so that whichever event fires first wins and subsequent events are no-ops.
fn discover_impl(url: String) -> js_sys::Promise {
    js_sys::Promise::new(&mut move |resolve, reject| {
        let ws = match web_sys::WebSocket::new(&url) {
            Ok(ws) => ws,
            Err(e) => {
                let _ = reject.call1(
                    &JsValue::NULL,
                    &JsValue::from_str(&format!("WebSocket open failed: {e:?}")),
                );
                return;
            }
        };
        let ws = Rc::new(ws);
        let resolve_rc: Rc<RefCell<Option<js_sys::Function>>> =
            Rc::new(RefCell::new(Some(resolve)));
        let reject_rc: Rc<RefCell<Option<js_sys::Function>>> =
            Rc::new(RefCell::new(Some(reject)));

        // on_open: send ListTopics
        {
            let ws_clone = ws.clone();
            let on_open = Closure::wrap(Box::new(move || {
                let msg = ClientMessage::ListTopics {
                    id: "discover".to_string(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = ws_clone.send_with_str(&json);
                }
            }) as Box<dyn FnMut()>);
            ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
            on_open.forget();
        }

        // on_message: parse TopicList, resolve, close socket
        {
            let ws_clone = ws.clone();
            let resolve_clone = resolve_rc.clone();
            let reject_clone = reject_rc.clone();
            let on_message =
                Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                    let _ = ws_clone.close();
                    let Some(text) = event.data().as_string() else {
                        if let Some(rej) = reject_clone.borrow_mut().take() {
                            let _ = rej.call1(
                                &JsValue::NULL,
                                &JsValue::from_str("Non-text frame from server"),
                            );
                        }
                        return;
                    };
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(ServerMessage::TopicList { topics, .. }) => {
                            let serializer =
                                serde_wasm_bindgen::Serializer::json_compatible();
                            let arr = js_sys::Array::new();
                            for topic in &topics {
                                if let Ok(js_val) = topic.serialize(&serializer) {
                                    arr.push(&js_val);
                                }
                            }
                            if let Some(res) = resolve_clone.borrow_mut().take() {
                                let _ = res.call1(&JsValue::NULL, &arr);
                            }
                        }
                        _ => {
                            if let Some(rej) = reject_clone.borrow_mut().take() {
                                let _ = rej.call1(
                                    &JsValue::NULL,
                                    &JsValue::from_str("Unexpected server message"),
                                );
                            }
                        }
                    }
                }) as Box<dyn FnMut(web_sys::MessageEvent)>);
            ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            on_message.forget();
        }

        // on_error: reject
        {
            let reject_clone = reject_rc.clone();
            let on_error = Closure::wrap(Box::new(move || {
                if let Some(rej) = reject_clone.borrow_mut().take() {
                    let _ = rej.call1(
                        &JsValue::NULL,
                        &JsValue::from_str("WebSocket error during discover"),
                    );
                }
            }) as Box<dyn FnMut()>);
            ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            on_error.forget();
        }

        // on_close: reject if server closed before we got TopicList
        // (no-op if on_message already resolved)
        {
            let reject_clone = reject_rc.clone();
            let on_close = Closure::wrap(Box::new(move || {
                if let Some(rej) = reject_clone.borrow_mut().take() {
                    let _ = rej.call1(
                        &JsValue::NULL,
                        &JsValue::from_str("Connection closed before TopicList received"),
                    );
                }
            }) as Box<dyn FnMut()>);
            ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
            on_close.forget();
        }

        // Timeout: reject after 30 s
        {
            let reject_clone = reject_rc.clone();
            let timeout_cb = Closure::once(move || {
                if let Some(rej) = reject_clone.borrow_mut().take() {
                    let _ = rej.call1(
                        &JsValue::NULL,
                        &JsValue::from_str("discover timed out"),
                    );
                }
            });
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    timeout_cb.as_ref().unchecked_ref(),
                    30_000,
                );
            }
            timeout_cb.forget();
        }
    })
}

// ─── Private helpers ──────────────────────────────────────────────────────

impl WasmDb {
    /// Resolve a record key to the live DB handle and its type-erased ops.
    fn resolve(&self, record_key: &str) -> Result<(&AimDb<WasmAdapter>, &SchemaOps), JsError> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| JsError::new("Database not built. Call build() first."))?;

        let schema = self
            .schema_map
            .get(record_key)
            .map(|v| v.as_str())
            .ok_or_else(|| JsError::new(&format!("Unknown record key: {record_key}")))?;

        let ops = self
            .registry
            .get(schema)
            .ok_or_else(|| JsError::new(&format!("Unknown schema type: {schema}")))?;

        Ok((db, ops))
    }
}

// ─── Typed dispatch ───────────────────────────────────────────────────────

/// Apply a single `RecordConfig` to the builder, dispatching on schema type.
fn apply_record_config(
    registry: &SchemaRegistry,
    builder: &mut AimDbBuilder<WasmAdapter>,
    config: &RecordConfig,
) -> Result<(), JsError> {
    let key = StringKey::intern(config.key.clone());
    let cfg = config.buffer_cfg.clone();

    let ops = registry
        .get(&config.schema_type)
        .ok_or_else(|| JsError::new(&format!("Unknown schema type: {}", config.schema_type)))?;

    (ops.configure)(builder, key, cfg);
    Ok(())
}

/// Read the latest snapshot for record `key` and convert to `JsValue`.
pub(crate) fn get_typed<T>(db: &AimDb<WasmAdapter>, key: &str) -> Result<JsValue, JsError>
where
    T: Send + Sync + 'static + Debug + Clone + Serialize,
{
    let inner = db.inner();
    let typed = inner
        .get_typed_record_by_key::<T, WasmAdapter>(key)
        .map_err(|e| JsError::new(&format!("{e:?}")))?;

    match typed.latest() {
        Some(val) => serde_wasm_bindgen::to_value(val.get())
            .map_err(|e| JsError::new(&format!("Serialization failed: {e}"))),
        None => Ok(JsValue::UNDEFINED),
    }
}

/// Deserialize `JsValue` → `T` (contract enforcement), then push to buffer.
pub(crate) fn set_typed<T>(
    db: &AimDb<WasmAdapter>,
    key: &str,
    value: JsValue,
) -> Result<(), JsError>
where
    T: Send + Sync + 'static + Debug + Clone + DeserializeOwned,
{
    let val: T = serde_wasm_bindgen::from_value(value)
        .map_err(|e| JsError::new(&format!("Contract violation: {e}")))?;

    let inner = db.inner();
    let typed = inner
        .get_typed_record_by_key::<T, WasmAdapter>(key)
        .map_err(|e| JsError::new(&format!("{e:?}")))?;

    // TypedRecord::produce() is declared `async` but its body is synchronous:
    // it updates `latest_snapshot` and calls `buf.push(val)` — both complete
    // immediately on WasmBuffer. We poll the future exactly once.
    poll_sync(typed.produce(val));
    Ok(())
}

/// Subscribe to a record's buffer and invoke `callback` on each new value.
/// Returns a JS function that cancels the subscription when called.
///
/// Uses `futures_util::future::select` to race `recv()` against a cancel
/// future so the unsubscribe closure can break the loop immediately — even
/// when `recv()` is blocked waiting for the next push.
pub(crate) fn subscribe_typed<T>(
    db: &AimDb<WasmAdapter>,
    key: &str,
    callback: &js_sys::Function,
) -> Result<JsValue, JsError>
where
    T: Send + Sync + 'static + Debug + Clone + Serialize,
{
    let mut reader = db
        .subscribe::<T>(key)
        .map_err(|e| JsError::new(&format!("{e:?}")))?;

    let callback = callback.clone();
    let (cancel_token, cancel_handle) = crate::buffer::cancel_pair();

    wasm_bindgen_futures::spawn_local(async move {
        use core::task::Poll;
        use futures_util::future::{select, Either};

        loop {
            // Future that resolves when cancel() is called.
            let cancel_fut = core::future::poll_fn(|cx| {
                if cancel_token.is_cancelled() {
                    Poll::Ready(())
                } else {
                    cancel_token.register_waker(cx.waker());
                    Poll::Pending
                }
            });

            let recv_fut = reader.recv();

            futures_util::pin_mut!(cancel_fut);
            futures_util::pin_mut!(recv_fut);

            match select(recv_fut, cancel_fut).await {
                Either::Left((Ok(val), _)) => {
                    if let Ok(js) = serde_wasm_bindgen::to_value(&val) {
                        let _ = callback.call1(&JsValue::NULL, &js);
                    }
                }
                Either::Left((Err(_), _)) => break, // buffer error
                Either::Right(((), _)) => break,    // cancelled
            }
        }
    });

    // Closure::wrap (not once_into_js) so it can be called multiple times
    // (React StrictMode calls cleanup twice).
    let unsub = Closure::wrap(Box::new(move || {
        cancel_handle.cancel();
    }) as Box<dyn FnMut()>);
    Ok(unsub.into_js_value())
}

// ─── Sync future polling ──────────────────────────────────────────────────

/// Poll a future that is known to resolve in a single poll (no real I/O).
///
/// Used for `TypedRecord::produce()` whose body is synchronous despite being
/// declared `async fn` — it just updates a snapshot and pushes to a buffer.
///
/// # Panics
///
/// Panics if the future returns `Pending`. This should never happen for
/// operations on `WasmBuffer` (which are single-threaded, non-blocking).
pub(crate) fn poll_sync<F: core::future::Future>(f: F) -> F::Output {
    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};

    // SAFETY: the future is stack-local and will not be moved after pinning.
    let mut f = f;
    let f = unsafe { Pin::new_unchecked(&mut f) };

    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    match f.poll(&mut cx) {
        Poll::Ready(val) => val,
        Poll::Pending => {
            panic!("poll_sync: future returned Pending (expected synchronous completion)")
        }
    }
}
