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
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::builder::{AimDb, AimDbBuilder};
use aimdb_core::record_id::StringKey;

use aimdb_data_contracts::dispatch_streamable;

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

fn is_known_schema(name: &str) -> bool {
    aimdb_data_contracts::is_streamable(name)
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
    schema_map: Vec<(String, String)>,
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
            schema_map: Vec::new(),
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

        if !is_known_schema(&opts.schema_type) {
            return Err(JsError::new(&format!(
                "Unknown schema type: {}",
                opts.schema_type
            )));
        }

        let buffer_cfg = parse_buffer_cfg(&opts.buffer)?;

        self.schema_map
            .push((record_key.to_string(), opts.schema_type.clone()));

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
            apply_record_config(&mut builder, config)?;
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
        let (db, schema) = self.resolve(record_key)?;
        dispatch_streamable!(schema, |T| get_typed::<T>(db, record_key))
            .ok_or_else(|| JsError::new(&format!("Unknown schema type: {schema}")))?
    }

    /// Set a record value (validates via Rust serde deserialization).
    ///
    /// Throws `JsError` if the payload fails contract validation (e.g. missing
    /// required fields) or the record key is unknown.
    pub fn set(&mut self, record_key: &str, value: JsValue) -> Result<(), JsError> {
        let (db, schema) = self.resolve(record_key)?;
        dispatch_streamable!(schema, |T| set_typed::<T>(db, record_key, value))
            .ok_or_else(|| JsError::new(&format!("Unknown schema type: {schema}")))?
    }

    /// Subscribe to record updates. Returns an unsubscribe function.
    ///
    /// `callback` is invoked on every buffer push with the validated value.
    pub fn subscribe(
        &self,
        record_key: &str,
        callback: &js_sys::Function,
    ) -> Result<JsValue, JsError> {
        let (db, schema) = self.resolve(record_key)?;
        dispatch_streamable!(schema, |T| subscribe_typed::<T>(db, record_key, callback))
            .ok_or_else(|| JsError::new(&format!("Unknown schema type: {schema}")))?
    }

    /// Returns `true` if the database has been built.
    #[wasm_bindgen(js_name = "isBuilt")]
    pub fn is_built(&self) -> bool {
        self.db.is_some()
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

        WsBridge::new_internal(db, schema_map, url, options)
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────

impl WasmDb {
    /// Resolve a record key to the live DB handle and its schema type name.
    fn resolve(&self, record_key: &str) -> Result<(&AimDb<WasmAdapter>, &str), JsError> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| JsError::new("Database not built. Call build() first."))?;

        let schema = self
            .schema_map
            .iter()
            .find(|(k, _)| k == record_key)
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| JsError::new(&format!("Unknown record key: {record_key}")))?;

        Ok((db, schema))
    }
}

// ─── Typed dispatch ───────────────────────────────────────────────────────

/// Apply a single `RecordConfig` to the builder, dispatching on schema type.
fn apply_record_config(
    builder: &mut AimDbBuilder<WasmAdapter>,
    config: &RecordConfig,
) -> Result<(), JsError> {
    use crate::WasmRecordRegistrarExt;

    let key = StringKey::intern(config.key.clone());
    let cfg = config.buffer_cfg.clone();

    dispatch_streamable!(config.schema_type.as_str(), |T| {
        builder.configure::<T>(key, |reg| {
            reg.buffer(cfg);
        });
    })
    .ok_or_else(|| JsError::new(&format!("Unknown schema type: {}", config.schema_type)))?;
    Ok(())
}

/// Read the latest snapshot for record `key` and convert to `JsValue`.
fn get_typed<T>(db: &AimDb<WasmAdapter>, key: &str) -> Result<JsValue, JsError>
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
fn set_typed<T>(db: &AimDb<WasmAdapter>, key: &str, value: JsValue) -> Result<(), JsError>
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
fn subscribe_typed<T>(
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
    use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    // SAFETY: the future is stack-local and will not be moved after pinning.
    let mut f = f;
    let f = unsafe { Pin::new_unchecked(&mut f) };

    // No-op waker — produce() does not need to be woken.
    fn noop(_: *const ()) {}
    fn clone_noop(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    const VTABLE: RawWakerVTable = RawWakerVTable::new(clone_noop, noop, noop, noop);

    let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);

    match f.poll(&mut cx) {
        Poll::Ready(val) => val,
        Poll::Pending => {
            panic!("poll_sync: future returned Pending (expected synchronous completion)")
        }
    }
}
