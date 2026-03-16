//! Type-erased dispatch registry for [`Streamable`] types in the WASM adapter.
//!
//! Built via [`SchemaRegistry::new`] + [`register`](SchemaRegistry::register)
//! calls. Each entry stores `Arc`-wrapped closures that capture the concrete
//! type `T` through monomorphization, enabling runtime dispatch by schema name
//! without a central match macro.
//!
//! The registry is `Clone`-able (cheap `Arc` bumps) so it can be shared
//! between `WasmDb` and `WsBridge`.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use wasm_bindgen::prelude::*;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::builder::{AimDb, AimDbBuilder};
use aimdb_core::record_id::StringKey;

use aimdb_data_contracts::Streamable;

use crate::WasmAdapter;

// ─── Type-erased operations ───────────────────────────────────────────────

type ConfigureFn = Arc<dyn Fn(&mut AimDbBuilder<WasmAdapter>, StringKey, BufferCfg) + Send + Sync>;
type GetFn = Arc<dyn Fn(&AimDb<WasmAdapter>, &str) -> Result<JsValue, JsError> + Send + Sync>;
type SetFn = Arc<dyn Fn(&AimDb<WasmAdapter>, &str, JsValue) -> Result<(), JsError> + Send + Sync>;
type SubscribeFn = Arc<
    dyn Fn(&AimDb<WasmAdapter>, &str, &js_sys::Function) -> Result<JsValue, JsError> + Send + Sync,
>;
type ProduceFromJsonFn = Arc<dyn Fn(&AimDb<WasmAdapter>, &str, serde_json::Value) + Send + Sync>;

/// Type-erased operations for a single [`Streamable`] type.
#[derive(Clone)]
pub(crate) struct SchemaOps {
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
    /// Duplicate registrations (same `T::NAME`) silently overwrite the
    /// previous entry.
    pub fn register<T: Streamable>(&mut self) -> &mut Self {
        use crate::bindings::{get_typed, set_typed, subscribe_typed};
        use crate::ws_bridge::produce_from_json;

        let ops = SchemaOps {
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
