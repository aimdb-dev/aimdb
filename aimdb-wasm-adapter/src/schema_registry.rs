//! Type-erased dispatch registry for [`Streamable`] types in the WASM adapter.
//!
//! Built once via [`SchemaRegistry::build`] using the visitor pattern from
//! `aimdb-data-contracts`. Each entry stores boxed closures that capture the
//! concrete type `T` through monomorphization, enabling runtime dispatch by
//! schema name without a central match macro.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;

use wasm_bindgen::prelude::*;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::builder::{AimDb, AimDbBuilder};
use aimdb_core::record_id::StringKey;

use aimdb_data_contracts::{for_each_streamable, Streamable, StreamableVisitor};

use crate::WasmAdapter;

// ─── Type-erased operations ───────────────────────────────────────────────

type ConfigureFn = Box<dyn Fn(&mut AimDbBuilder<WasmAdapter>, StringKey, BufferCfg) + Send + Sync>;
type GetFn = Box<dyn Fn(&AimDb<WasmAdapter>, &str) -> Result<JsValue, JsError> + Send + Sync>;
type SetFn = Box<dyn Fn(&AimDb<WasmAdapter>, &str, JsValue) -> Result<(), JsError> + Send + Sync>;
type SubscribeFn = Box<
    dyn Fn(&AimDb<WasmAdapter>, &str, &js_sys::Function) -> Result<JsValue, JsError> + Send + Sync,
>;
type ProduceFromJsonFn = Box<dyn Fn(&AimDb<WasmAdapter>, &str, serde_json::Value) + Send + Sync>;

/// Type-erased operations for a single [`Streamable`] type.
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
/// Built once at startup via [`SchemaRegistry::build`], then shared across
/// the `WasmDb` and `WsBridge`.
pub(crate) struct SchemaRegistry {
    entries: BTreeMap<&'static str, SchemaOps>,
}

impl SchemaRegistry {
    /// Build the registry by visiting all [`Streamable`] types.
    pub fn build() -> Self {
        let mut builder = RegistryBuilder {
            entries: BTreeMap::new(),
        };
        for_each_streamable(&mut builder);
        SchemaRegistry {
            entries: builder.entries,
        }
    }

    /// Look up operations for a schema name.
    pub fn get(&self, schema_name: &str) -> Option<&SchemaOps> {
        self.entries.get(schema_name)
    }

    /// Returns `true` if the schema name is known.
    pub fn is_known(&self, schema_name: &str) -> bool {
        self.entries.contains_key(schema_name)
    }
}

// ─── Visitor that builds the registry ─────────────────────────────────────

struct RegistryBuilder {
    entries: BTreeMap<&'static str, SchemaOps>,
}

impl StreamableVisitor for RegistryBuilder {
    fn visit<T: Streamable>(&mut self) {
        use crate::bindings::{get_typed, set_typed, subscribe_typed};
        use crate::ws_bridge::produce_from_json;

        let ops = SchemaOps {
            configure: Box::new(|builder, key, cfg| {
                use crate::WasmRecordRegistrarExt;
                builder.configure::<T>(key, |reg| {
                    reg.buffer(cfg);
                });
            }),
            get: Box::new(get_typed::<T>),
            set: Box::new(set_typed::<T>),
            subscribe: Box::new(|db, key, cb| subscribe_typed::<T>(db, key, cb)),
            produce_from_json: Box::new(produce_from_json::<T>),
        };
        self.entries.insert(T::NAME, ops);
    }
}
