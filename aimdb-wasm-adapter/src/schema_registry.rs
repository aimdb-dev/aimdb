//! Type-erased dispatch registry for [`Streamable`] types in the WASM adapter.
//!
//! Built via [`SchemaRegistry::new`] + [`register`](SchemaRegistry::register)
//! calls. Each entry stores `Arc`-wrapped closures that capture the concrete
//! type `T` through monomorphization, enabling runtime dispatch by schema name
//! without a central match macro.
//!
//! The registry is `Clone`-able (cheap `Arc` bumps) so it can be shared
//! between `WasmDb` and `WsBridge`.
//!
//! # Schema versions are an ingest concern, not a browser one
//!
//! Inbound AimX payloads decode with `serde_json::from_value::<T>`, **not**
//! through `Linkable::from_bytes`, so `Migratable` chains do not run here — an
//! older-shaped payload is logged and dropped, not upgraded.
//!
//! Deliberate: AimX is a *normalized* plane. A server migrates at its ingest
//! boundary (an MQTT `link_from` decoding through `Linkable`) and serves one
//! current shape onward, so a browser mirror registers the newest type per name
//! and expects that shape. The consequence: **a browser's version tolerance
//! belongs to the server it mirrors, not to the contracts crate compiled in
//! beside it** — bridging raw device payloads to a browser is unsupported.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use wasm_bindgen::prelude::*;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::builder::{AimDb, AimDbBuilder};
use aimdb_core::record_id::StringKey;

use aimdb_data_contracts::Streamable;

// ─── Type-erased operations ───────────────────────────────────────────────

type ConfigureFn = Arc<dyn Fn(&mut AimDbBuilder, StringKey, BufferCfg) + Send + Sync>;
type GetFn = Arc<dyn Fn(&AimDb, &str) -> Result<JsValue, JsError> + Send + Sync>;
type SetFn = Arc<dyn Fn(&AimDb, &str, JsValue) -> Result<(), JsError> + Send + Sync>;
type SubscribeFn =
    Arc<dyn Fn(&AimDb, &str, &js_sys::Function) -> Result<JsValue, JsError> + Send + Sync>;
type ProduceFromJsonFn = Arc<dyn Fn(&AimDb, &str, serde_json::Value) + Send + Sync>;

/// Type-erased operations for a single [`Streamable`] type.
#[derive(Clone)]
pub(crate) struct SchemaOps {
    /// Which concrete type these closures were monomorphized for. Only read by
    /// the duplicate-registration check in [`SchemaRegistry::register`].
    pub type_id: core::any::TypeId,
    pub configure: ConfigureFn,
    pub get: GetFn,
    pub set: SetFn,
    pub subscribe: SubscribeFn,
    pub produce_from_json: ProduceFromJsonFn,
}

// ─── Registry ─────────────────────────────────────────────────────────────

/// Maps schema names to type-erased operations.
///
/// Built via [`SchemaRegistry::new`] + repeated [`register`](Self::register)
/// calls at startup, then shared between `WasmDb` and `WsBridge`.
///
/// Cloning is cheap — all closures are `Arc`-wrapped.
#[derive(Clone)]
pub(crate) struct SchemaRegistry {
    entries: BTreeMap<&'static str, SchemaOps>,
}

impl SchemaRegistry {
    /// Create an empty registry. Call [`register`](Self::register) to add types.
    pub fn new() -> Self {
        SchemaRegistry {
            entries: BTreeMap::new(),
        }
    }

    /// Register a [`Streamable`] type for runtime dispatch.
    ///
    /// Entries key on `T::NAME` with no version component: `TemperatureV1` and
    /// `TemperatureV2` both declare `NAME = "temperature"`, so registering both
    /// keeps only the last. **Register the newest type per name.** The failure
    /// is otherwise near-silent — a browser renders nothing, with no error on
    /// either side — so a *different* type claiming a registered name trips a
    /// `debug_assert!`. Re-registering the same type is idempotent, matching
    /// the WS connector's server-side `StreamableRegistry`.
    pub fn register<T: Streamable>(&mut self) -> &mut Self {
        use crate::bindings::{get_typed, set_typed, subscribe_typed};
        use crate::ws_bridge::produce_from_json;

        let ops = SchemaOps {
            type_id: core::any::TypeId::of::<T>(),
            configure: Arc::new(|builder, key, cfg| {
                use crate::WasmRecordRegistrarExt;
                builder.configure::<T>(key, |reg| {
                    reg.buffer(cfg);
                });
            }),
            get: Arc::new(get_typed::<T>),
            set: Arc::new(set_typed::<T>),
            subscribe: Arc::new(|db, key, cb| subscribe_typed::<T>(db, key, cb)),
            produce_from_json: Arc::new(produce_from_json::<T>),
        };
        debug_assert!(
            self.entries
                .get(T::NAME)
                .is_none_or(|prev| prev.type_id == ops.type_id),
            "schema name collision on {:?}: a different type already claims it \
             (e.g. a v1 and v2 of one contract). Register only the newest.",
            T::NAME
        );
        self.entries.insert(T::NAME, ops);
        self
    }

    /// Look up operations for a schema name.
    pub fn get(&self, schema_name: &str) -> Option<&SchemaOps> {
        self.entries.get(schema_name)
    }

    /// Returns `true` if the schema name is known.
    pub fn is_known(&self, schema_name: &str) -> bool {
        self.entries.contains_key(schema_name)
    }

    /// Returns all registered schema names.
    pub fn known_names(&self) -> alloc::vec::Vec<&'static str> {
        self.entries.keys().copied().collect()
    }
}
