//! Rust source code generator
//!
//! Converts an [`ArchitectureState`] into compilable Rust source that uses the
//! actual AimDB 0.5.x API: `#[derive(RecordKey)]`, `BufferCfg`, and
//! `AimDbBuilder::configure()`.
//!
//! Uses [`quote`] for quasi-quoting token streams and [`prettyplease`] for
//! formatting the output into idiomatic Rust.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::state::{ArchitectureState, ConnectorDirection, RecordDef, SerializationType};

// ── Public API ────────────────────────────────────────────────────────────────

/// Generate a complete Rust source file from architecture state.
///
/// The returned string can be written to `src/generated_schema.rs`.
/// It contains:
/// - One `<Name>Value` struct per record (with `Serialize` / `Deserialize`)
/// - One `<Name>Key` enum per record (with `#[derive(RecordKey)]`)
/// - A `configure_schema<R>()` function wiring all records into `AimDbBuilder`
pub fn generate_rust(state: &ArchitectureState) -> String {
    let formatted = generate_rust_inner(state);

    let header = "\
// @generated — do not edit manually.\n\
// Source: .aimdb/state.toml — edit via `aimdb generate` or the architecture agent.\n\
// Regenerate: `aimdb generate` or confirm a proposal in the architecture agent.\n\n";

    format!("{header}{formatted}")
}

/// Generate `schema.rs` for a common crate (no `@generated` header).
///
/// Emits only the portable data-contract layer: value structs, key enums,
/// `SchemaType` and `Linkable` impls. No `configure_schema`, no runtime deps.
/// This keeps the common crate platform-agnostic (`no_std`-compatible).
pub fn generate_schema_rs(state: &ArchitectureState) -> String {
    generate_types_inner(state)
}

/// Types-only inner — value structs + key enums + trait impls, no `configure_schema`.
fn generate_types_inner(state: &ArchitectureState) -> String {
    let imports = emit_imports_types_only(state);

    let record_items: Vec<TokenStream> = state
        .records
        .iter()
        .flat_map(|rec| {
            let mut items = vec![emit_value_struct(rec), emit_key_enum(rec)];
            items.push(emit_schema_type_impl(rec));
            let linkable = emit_linkable_impl(rec);
            if !linkable.is_empty() {
                items.push(linkable);
            }
            if let Some(obs) = emit_observable_impl(rec) {
                items.push(obs);
            }
            if let Some(set) = emit_settable_impl(rec) {
                items.push(set);
            }
            items
        })
        .collect();

    let file_tokens = quote! {
        #imports
        #(#record_items)*
    };

    let syntax_tree = syn::parse2(file_tokens).expect("generated tokens should be valid Rust");
    prettyplease::unparse(&syntax_tree)
}

fn generate_rust_inner(state: &ArchitectureState) -> String {
    let imports = emit_imports(state);

    let record_items: Vec<TokenStream> = state
        .records
        .iter()
        .flat_map(|rec| {
            let mut items = vec![emit_value_struct(rec), emit_key_enum(rec)];
            items.push(emit_schema_type_impl(rec));
            let linkable = emit_linkable_impl(rec);
            if !linkable.is_empty() {
                items.push(linkable);
            }
            if let Some(obs) = emit_observable_impl(rec) {
                items.push(obs);
            }
            if let Some(set) = emit_settable_impl(rec) {
                items.push(set);
            }
            items
        })
        .collect();

    let configure_fn = emit_configure_schema(state);

    let file_tokens = quote! {
        #imports
        #(#record_items)*
        #configure_fn
    };

    let syntax_tree = syn::parse2(file_tokens).expect("generated tokens should be valid Rust");
    prettyplease::unparse(&syntax_tree)
}

/// Generate `Cargo.toml` content for a common crate.
///
/// Requires `state.project` to be `Some`. The caller should validate this
/// before calling.
pub fn generate_cargo_toml(state: &ArchitectureState) -> String {
    let project = state
        .project
        .as_ref()
        .expect("generate_cargo_toml requires [project] block in state.toml");
    let crate_name = format!("{}-common", project.name);
    let edition = project.edition.as_deref().unwrap_or("2024");

    let has_non_custom_ser = state.records.iter().any(|r| {
        r.serialization.as_ref().unwrap_or(&SerializationType::Json) != &SerializationType::Custom
    });
    let has_postcard = state
        .records
        .iter()
        .any(|r| r.serialization.as_ref() == Some(&SerializationType::Postcard));
    let has_observable = state.records.iter().any(|r| r.observable.is_some());

    let mut data_contracts_features = Vec::new();
    if has_non_custom_ser {
        data_contracts_features.push("\"linkable\"");
    }

    let dc_features_str = if data_contracts_features.is_empty() {
        String::new()
    } else {
        format!(", features = [{}]", data_contracts_features.join(", "))
    };

    // Build std feature deps
    let mut std_deps = vec!["\"aimdb-data-contracts/std\"".to_string()];
    if has_non_custom_ser && !has_postcard {
        std_deps.push("\"serde_json\"".to_string());
    }
    if has_observable {
        std_deps.push("\"aimdb-data-contracts/observable\"".to_string());
    }
    let std_features = std_deps.join(", ");

    let mut optional_deps = String::new();
    if has_non_custom_ser && !has_postcard {
        optional_deps.push_str("serde_json = { version = \"1.0\", optional = true }\n");
    }
    if has_postcard {
        optional_deps.push_str(
            "postcard = { version = \"1.0\", default-features = false, features = [\"alloc\"] }\n",
        );
    }

    format!(
        r#"# Regenerate with `aimdb generate --common-crate`
[package]
name = "{crate_name}"
version = "0.1.0"
edition = "{edition}"

[features]
default = ["std"]
std = [{std_features}]
alloc = []

[dependencies]
aimdb-core = {{ version = "0.5", default-features = false, features = ["derive", "alloc"] }}
aimdb-data-contracts = {{ version = "0.5", default-features = false{dc_features_str} }}
serde = {{ version = "1.0", default-features = false, features = ["derive", "alloc"] }}
{optional_deps}"#
    )
}

/// Generate `lib.rs` content for a common crate.
pub fn generate_lib_rs() -> String {
    "\
// Regenerate with `aimdb generate --common-crate`
#![cfg_attr(not(feature = \"std\"), no_std)]
extern crate alloc;

mod schema;

// Re-export all public types for downstream crates
pub use schema::*;
"
    .to_string()
}

// ── Imports ──────────────────────────────────────────────────────────────────

/// Imports for the types-only common crate schema — no runtime deps.
fn emit_imports_types_only(state: &ArchitectureState) -> TokenStream {
    let has_non_custom_ser = state.records.iter().any(|r| {
        r.serialization.as_ref().unwrap_or(&SerializationType::Json) != &SerializationType::Custom
    });
    let has_observable = state.records.iter().any(|r| r.observable.is_some());
    let has_settable = state
        .records
        .iter()
        .any(|r| r.fields.iter().any(|f| f.settable));

    let mut contract_traits: Vec<TokenStream> = vec![quote! { SchemaType }];
    if has_non_custom_ser {
        contract_traits.push(quote! { Linkable });
    }
    if has_observable {
        contract_traits.push(quote! { Observable });
    }
    if has_settable {
        contract_traits.push(quote! { Settable });
    }

    quote! {
        use aimdb_core::RecordKey;
        use aimdb_data_contracts::{#(#contract_traits),*};
        use serde::{Deserialize, Serialize};
    }
}

/// Imports for the full flat schema — includes runtime registration deps.
fn emit_imports(state: &ArchitectureState) -> TokenStream {
    let has_non_custom_ser = state.records.iter().any(|r| {
        r.serialization.as_ref().unwrap_or(&SerializationType::Json) != &SerializationType::Custom
    });
    let has_observable = state.records.iter().any(|r| r.observable.is_some());
    let has_settable = state
        .records
        .iter()
        .any(|r| r.fields.iter().any(|f| f.settable));

    // Build aimdb_data_contracts trait imports
    let mut contract_traits: Vec<TokenStream> = vec![quote! { SchemaType }];
    if has_non_custom_ser {
        contract_traits.push(quote! { Linkable });
    }
    if has_observable {
        contract_traits.push(quote! { Observable });
    }
    if has_settable {
        contract_traits.push(quote! { Settable });
    }

    quote! {
        use aimdb_core::buffer::BufferCfg;
        use aimdb_core::builder::AimDbBuilder;
        use aimdb_core::RecordKey;
        use aimdb_data_contracts::{#(#contract_traits),*};
        use aimdb_executor::Spawn;
        use serde::{Deserialize, Serialize};
    }
}

// ── Value struct ──────────────────────────────────────────────────────────────

fn emit_value_struct(rec: &RecordDef) -> TokenStream {
    let struct_name = format_ident!("{}Value", rec.name);
    let doc = format!("Value type for `{}`.", rec.name);

    let fields: Vec<TokenStream> = if rec.fields.is_empty() {
        vec![emit_todo_field(
            "add fields — use `propose_record` to define them via the architecture agent",
        )]
    } else {
        rec.fields
            .iter()
            .map(|f| {
                let fname = format_ident!("{}", f.name);
                let ftype: syn::Type = syn::parse_str(&f.field_type).unwrap_or_else(|_| {
                    panic!("invalid type `{}` for field `{}`", f.field_type, f.name)
                });
                if f.description.is_empty() {
                    quote! { pub #fname: #ftype, }
                } else {
                    let desc = &f.description;
                    quote! {
                        #[doc = #desc]
                        pub #fname: #ftype,
                    }
                }
            })
            .collect()
    };

    quote! {
        #[doc = #doc]
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub struct #struct_name {
            #(#fields)*
        }
    }
}

/// Emit a dummy field with a TODO doc comment (for records with no fields yet).
fn emit_todo_field(msg: &str) -> TokenStream {
    let doc = format!("TODO: {msg}");
    quote! {
        #[doc = #doc]
        pub _placeholder: (),
    }
}

// ── Key enum ──────────────────────────────────────────────────────────────────

fn emit_key_enum(rec: &RecordDef) -> TokenStream {
    let enum_name = format_ident!("{}Key", rec.name);
    let connector = rec.connectors.first();

    let key_prefix_attr = if !rec.key_prefix.is_empty() {
        let prefix = &rec.key_prefix;
        quote! { #[key_prefix = #prefix] }
    } else {
        quote! {}
    };

    let variants: Vec<TokenStream> = if rec.key_variants.is_empty() {
        let doc = "TODO: add key variants — use the architecture agent to resolve them";
        vec![quote! {
            #[doc = #doc]
            _Placeholder,
        }]
    } else {
        rec.key_variants
            .iter()
            .map(|variant_str| {
                let variant_name = format_ident!("{}", to_pascal_case(variant_str));
                let link_attr = connector.map(|conn| {
                    let addr = conn.url.replace("{variant}", variant_str);
                    quote! { #[link_address = #addr] }
                });
                quote! {
                    #[key = #variant_str]
                    #link_attr
                    #variant_name,
                }
            })
            .collect()
    };

    quote! {
        #[derive(Debug, RecordKey, Clone, Copy, PartialEq, Eq)]
        #key_prefix_attr
        pub enum #enum_name {
            #(#variants)*
        }
    }
}

// ── configure_schema ──────────────────────────────────────────────────────────

fn emit_configure_schema(state: &ArchitectureState) -> TokenStream {
    let record_blocks: Vec<TokenStream> = state
        .records
        .iter()
        .map(emit_record_configure_block)
        .collect();

    quote! {
        /// Register all architecture-agent-defined records on the builder.
        ///
        /// Generated from `.aimdb/state.toml`. Configures buffer types and connector
        /// addresses. Producers, consumers, serializers, and deserializers contain
        /// business logic and must be provided by application code — they are not
        /// generated here.
        pub fn configure_schema<R: Spawn + 'static>(builder: &mut AimDbBuilder<R>) {
            #(#record_blocks)*
        }
    }
}

fn emit_record_configure_block(rec: &RecordDef) -> TokenStream {
    if rec.key_variants.is_empty() {
        let msg = format!("TODO: {}: no key variants defined yet", rec.name);
        return quote! {
            // #msg — placeholder
            let _ = (#msg,);
        };
    }

    let value_type = format_ident!("{}Value", rec.name);
    let key_type = format_ident!("{}Key", rec.name);
    let buffer_tokens = rec.buffer.to_tokens(rec.capacity);

    let variant_idents: Vec<syn::Ident> = rec
        .key_variants
        .iter()
        .map(|v| format_ident!("{}", to_pascal_case(v)))
        .collect();

    let is_custom = rec
        .serialization
        .as_ref()
        .map(|s| s == &SerializationType::Custom)
        .unwrap_or(false);

    let connector_block = rec.connectors.first().map(|conn| {
        if is_custom {
            // Custom serialization: keep TODO stubs
            let link_fn = match conn.direction {
                ConnectorDirection::Outbound => format_ident!("link_to"),
                ConnectorDirection::Inbound => format_ident!("link_from"),
            };
            let todo_comment = match conn.direction {
                ConnectorDirection::Outbound => {
                    "TODO: add .with_serializer(...) — serialization = \"custom\""
                }
                ConnectorDirection::Inbound => {
                    "TODO: add .with_deserializer(...) — serialization = \"custom\""
                }
            };
            quote! {
                if let Some(addr) = key.link_address() {
                    let _ = #todo_comment;
                    reg.#link_fn(addr);
                }
            }
        } else {
            // Non-custom: wire real serializers via Linkable trait
            match conn.direction {
                ConnectorDirection::Inbound => {
                    quote! {
                        if let Some(addr) = key.link_address() {
                            reg.link_from(addr)
                                .with_deserializer(#value_type::from_bytes)
                                .finish();
                        }
                    }
                }
                ConnectorDirection::Outbound => {
                    quote! {
                        if let Some(addr) = key.link_address() {
                            reg.link_to(addr)
                                .with_serializer(|v: &#value_type| {
                                    v.to_bytes()
                                        .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
                                })
                                .finish();
                        }
                    }
                }
            }
        }
    });

    quote! {
        for key in [
            #(#key_type::#variant_idents,)*
        ] {
            builder.configure::<#value_type>(key, |reg| {
                reg.buffer(#buffer_tokens);
                #connector_block
            });
        }
    }
}

// ── Trait implementations ────────────────────────────────────────────────────

fn emit_schema_type_impl(rec: &RecordDef) -> TokenStream {
    let struct_name = format_ident!("{}Value", rec.name);
    let schema_name = to_snake_case(&rec.name);
    let version = proc_macro2::Literal::u32_unsuffixed(rec.schema_version.unwrap_or(1));

    quote! {
        impl SchemaType for #struct_name {
            const NAME: &'static str = #schema_name;
            const VERSION: u32 = #version;
        }
    }
}

fn emit_linkable_impl(rec: &RecordDef) -> TokenStream {
    let ser = rec
        .serialization
        .as_ref()
        .unwrap_or(&SerializationType::Json);

    match ser {
        SerializationType::Custom => quote! {},
        SerializationType::Json => emit_linkable_json(rec),
        SerializationType::Postcard => emit_linkable_postcard(rec),
    }
}

fn emit_linkable_json(rec: &RecordDef) -> TokenStream {
    let struct_name = format_ident!("{}Value", rec.name);
    quote! {
        impl Linkable for #struct_name {
            fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
                #[cfg(feature = "std")]
                {
                    serde_json::to_vec(self)
                        .map_err(|e| alloc::format!("serialize {}: {e}", Self::NAME))
                }
                #[cfg(not(feature = "std"))]
                {
                    Err(alloc::string::String::from(
                        "no_std serialization not available — enable the std feature or use postcard",
                    ))
                }
            }

            fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
                #[cfg(feature = "std")]
                {
                    serde_json::from_slice(data)
                        .map_err(|e| alloc::format!("deserialize {}: {e}", Self::NAME))
                }
                #[cfg(not(feature = "std"))]
                {
                    let _ = data;
                    Err(alloc::string::String::from(
                        "no_std deserialization not available — enable the std feature or use postcard",
                    ))
                }
            }
        }
    }
}

fn emit_linkable_postcard(rec: &RecordDef) -> TokenStream {
    let struct_name = format_ident!("{}Value", rec.name);
    quote! {
        impl Linkable for #struct_name {
            fn to_bytes(&self) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
                postcard::to_allocvec(self)
                    .map_err(|e| alloc::format!("serialize {}: {e}", Self::NAME))
            }

            fn from_bytes(data: &[u8]) -> Result<Self, alloc::string::String> {
                postcard::from_bytes(data)
                    .map_err(|e| alloc::format!("deserialize {}: {e}", Self::NAME))
            }
        }
    }
}

fn emit_observable_impl(rec: &RecordDef) -> Option<TokenStream> {
    let obs = rec.observable.as_ref()?;
    let struct_name = format_ident!("{}Value", rec.name);

    // Look up signal field type
    let signal_field = rec.fields.iter().find(|f| f.name == obs.signal_field)?;
    let signal_type: syn::Type = syn::parse_str(&signal_field.field_type).ok()?;
    let signal_ident = format_ident!("{}", obs.signal_field);

    let icon = &obs.icon;
    let unit = &obs.unit;

    // Timestamp heuristic: first u64 field named timestamp/computed_at/fetched_at
    let timestamp_names = ["timestamp", "computed_at", "fetched_at"];
    let timestamp_field = rec
        .fields
        .iter()
        .find(|f| f.field_type == "u64" && timestamp_names.contains(&f.name.as_str()));

    let format_log_body = if let Some(ts) = timestamp_field {
        let ts_ident = format_ident!("{}", ts.name);
        quote! {
            alloc::format!(
                "{} [{}] {}: {:.1}{} at {}",
                Self::ICON,
                node_id,
                Self::NAME,
                self.signal(),
                Self::UNIT,
                self.#ts_ident,
            )
        }
    } else {
        quote! {
            alloc::format!(
                "{} [{}] {}: {:.1}{}",
                Self::ICON,
                node_id,
                Self::NAME,
                self.signal(),
                Self::UNIT,
            )
        }
    };

    Some(quote! {
        impl Observable for #struct_name {
            type Signal = #signal_type;
            const ICON: &'static str = #icon;
            const UNIT: &'static str = #unit;

            fn signal(&self) -> #signal_type {
                self.#signal_ident
            }

            fn format_log(&self, node_id: &str) -> alloc::string::String {
                #format_log_body
            }
        }
    })
}

fn emit_settable_impl(rec: &RecordDef) -> Option<TokenStream> {
    let settable_fields: Vec<_> = rec.fields.iter().filter(|f| f.settable).collect();
    if settable_fields.is_empty() {
        return None;
    }

    let struct_name = format_ident!("{}Value", rec.name);

    // Build the Value type
    let settable_types: Vec<syn::Type> = settable_fields
        .iter()
        .map(|f| syn::parse_str(&f.field_type).unwrap())
        .collect();

    let value_type: TokenStream = if settable_types.len() == 1 {
        let t = &settable_types[0];
        quote! { #t }
    } else {
        quote! { (#(#settable_types),*) }
    };

    // Timestamp heuristic: first u64 field named timestamp/computed_at/fetched_at
    let timestamp_names = ["timestamp", "computed_at", "fetched_at"];
    let timestamp_field = rec
        .fields
        .iter()
        .find(|f| f.field_type == "u64" && timestamp_names.contains(&f.name.as_str()));

    // Build field assignments for `set()`
    let mut settable_idx = 0usize;
    let field_assignments: Vec<TokenStream> = rec
        .fields
        .iter()
        .map(|f| {
            let fname = format_ident!("{}", f.name);
            if timestamp_field.map(|tf| tf.name == f.name).unwrap_or(false) && !f.settable {
                // This is the timestamp field — fill from parameter
                quote! { #fname: timestamp, }
            } else if f.settable {
                let assignment = if settable_fields.len() == 1 {
                    quote! { value }
                } else {
                    let idx = syn::Index::from(settable_idx);
                    quote! { value.#idx }
                };
                settable_idx += 1;
                quote! { #fname: #assignment, }
            } else {
                // Non-settable, non-timestamp field: use Default
                quote! { #fname: Default::default(), }
            }
        })
        .collect();

    Some(quote! {
        impl Settable for #struct_name {
            type Value = #value_type;

            fn set(value: Self::Value, timestamp: u64) -> Self {
                Self {
                    #(#field_assignments)*
                }
            }
        }
    })
}

// ── Utilities ─────────────────────────────────────────────────────────────────

/// Convert a PascalCase string to snake_case.
///
/// # Examples
/// ```
/// # use aimdb_codegen::rust::to_snake_case;
/// assert_eq!(to_snake_case("WeatherObservation"), "weather_observation");
/// assert_eq!(to_snake_case("OtaCommand"), "ota_command");
/// assert_eq!(to_snake_case("Temperature"), "temperature");
/// ```
pub fn to_snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        for lc in c.to_lowercase() {
            result.push(lc);
        }
    }
    result
}

/// Convert a kebab-case or snake_case string to PascalCase.
///
/// # Examples
/// ```
/// # use aimdb_codegen::rust::to_pascal_case;
/// assert_eq!(to_pascal_case("indoor"), "Indoor");
/// assert_eq!(to_pascal_case("gateway-01"), "Gateway01");
/// assert_eq!(to_pascal_case("sensor-hub-01"), "SensorHub01");
/// assert_eq!(to_pascal_case("sensor_hub_01"), "SensorHub01");
/// ```
pub fn to_pascal_case(s: &str) -> String {
    s.split(['-', '_'])
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    upper + chars.as_str()
                }
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ArchitectureState;

    const SAMPLE_TOML: &str = r#"
[meta]
aimdb_version = "0.5.0"
created_at = "2026-02-22T14:00:00Z"
last_modified = "2026-02-22T14:33:00Z"

[[records]]
name = "TemperatureReading"
buffer = "SpmcRing"
capacity = 256
key_prefix = "sensors.temp."
key_variants = ["indoor", "outdoor", "garage"]
producers = ["sensor_task"]
consumers = ["dashboard", "anomaly_detector"]

[[records.fields]]
name = "celsius"
type = "f64"
description = "Temperature in degrees Celsius"

[[records.fields]]
name = "humidity_percent"
type = "f64"
description = "Relative humidity 0-100"

[[records.fields]]
name = "timestamp"
type = "u64"
description = "Unix timestamp in milliseconds"

[[records.connectors]]
protocol = "mqtt"
direction = "outbound"
url = "mqtt://sensors/temp/{variant}"

[[records]]
name = "OtaCommand"
buffer = "Mailbox"
key_prefix = "device.ota."
key_variants = ["gateway-01", "sensor-hub-01"]
producers = ["cloud_ota"]
consumers = ["updater"]

[[records.fields]]
name = "action"
type = "String"
description = "Command: update, rollback, reboot"

[[records.fields]]
name = "target_version"
type = "String"
description = "Target firmware version"

[[records.connectors]]
protocol = "mqtt"
direction = "inbound"
url = "mqtt://ota/cmd/{variant}"
"#;

    fn state() -> ArchitectureState {
        ArchitectureState::from_toml(SAMPLE_TOML).unwrap()
    }

    fn generated() -> String {
        generate_rust(&state())
    }

    #[test]
    fn has_generated_header() {
        let out = generated();
        assert!(
            out.contains("@generated"),
            "Missing @generated header:\n{out}"
        );
    }

    #[test]
    fn has_imports() {
        let out = generated();
        assert!(
            out.contains("use aimdb_core::buffer::BufferCfg;"),
            "Missing BufferCfg import:\n{out}"
        );
        assert!(
            out.contains("use aimdb_core::builder::AimDbBuilder;"),
            "Missing AimDbBuilder import:\n{out}"
        );
        assert!(
            out.contains("use aimdb_core::RecordKey;"),
            "Missing RecordKey import:\n{out}"
        );
        assert!(
            out.contains("use aimdb_executor::Spawn;"),
            "Missing Spawn import:\n{out}"
        );
        assert!(
            out.contains("use serde::{Deserialize, Serialize};"),
            "Missing serde import:\n{out}"
        );
    }

    #[test]
    fn value_struct_generated() {
        let out = generated();
        assert!(
            out.contains("pub struct TemperatureReadingValue"),
            "Missing TemperatureReadingValue struct:\n{out}"
        );
        assert!(
            out.contains("pub celsius: f64,"),
            "Missing celsius field:\n{out}"
        );
        assert!(
            out.contains("pub humidity_percent: f64,"),
            "Missing humidity_percent field:\n{out}"
        );
        assert!(
            out.contains("pub timestamp: u64,"),
            "Missing timestamp field:\n{out}"
        );
        assert!(
            out.contains("#[derive(Debug, Clone, Serialize, Deserialize)]"),
            "Missing derives:\n{out}"
        );
    }

    #[test]
    fn key_enum_generated() {
        let out = generated();
        assert!(
            out.contains("pub enum TemperatureReadingKey"),
            "Missing key enum:\n{out}"
        );
        assert!(
            out.contains("#[derive(Debug, RecordKey, Clone, Copy, PartialEq, Eq)]"),
            "Missing RecordKey derive:\n{out}"
        );
        assert!(
            out.contains("#[key_prefix = \"sensors.temp.\"]"),
            "Missing key_prefix:\n{out}"
        );
        assert!(
            out.contains("#[key = \"indoor\"]"),
            "Missing indoor key attr:\n{out}"
        );
        assert!(
            out.contains("#[key = \"outdoor\"]"),
            "Missing outdoor key attr:\n{out}"
        );
        assert!(
            out.contains("#[key = \"garage\"]"),
            "Missing garage key attr:\n{out}"
        );
        assert!(out.contains("Indoor,"), "Missing Indoor variant:\n{out}");
        assert!(out.contains("Outdoor,"), "Missing Outdoor variant:\n{out}");
        assert!(out.contains("Garage,"), "Missing Garage variant:\n{out}");
    }

    #[test]
    fn link_address_substituted_per_variant() {
        let out = generated();
        assert!(
            out.contains("#[link_address = \"mqtt://sensors/temp/indoor\"]"),
            "link_address not substituted for indoor:\n{out}"
        );
        assert!(
            out.contains("#[link_address = \"mqtt://sensors/temp/outdoor\"]"),
            "link_address not substituted for outdoor:\n{out}"
        );
        assert!(
            out.contains("#[link_address = \"mqtt://sensors/temp/garage\"]"),
            "link_address not substituted for garage:\n{out}"
        );
    }

    #[test]
    fn kebab_variants_to_pascal_case() {
        let out = generated();
        assert!(
            out.contains("pub enum OtaCommandKey"),
            "Missing OtaCommandKey enum:\n{out}"
        );
        assert!(
            out.contains("Gateway01,"),
            "gateway-01 should become Gateway01:\n{out}"
        );
        assert!(
            out.contains("SensorHub01,"),
            "sensor-hub-01 should become SensorHub01:\n{out}"
        );
    }

    #[test]
    fn configure_schema_function_present() {
        let out = generated();
        assert!(
            out.contains(
                "pub fn configure_schema<R: Spawn + 'static>(builder: &mut AimDbBuilder<R>)"
            ),
            "Missing configure_schema function:\n{out}"
        );
    }

    #[test]
    fn configure_schema_spmc_buffer() {
        let out = generated();
        // prettyplease may split struct literals across lines
        assert!(
            out.contains("BufferCfg::SpmcRing"),
            "Missing SpmcRing buffer call:\n{out}"
        );
        assert!(
            out.contains("capacity: 256"),
            "Missing capacity value:\n{out}"
        );
    }

    #[test]
    fn configure_schema_mailbox_buffer() {
        let out = generated();
        assert!(
            out.contains("BufferCfg::Mailbox"),
            "Missing Mailbox buffer call:\n{out}"
        );
    }

    #[test]
    fn configure_schema_outbound_link_to_with_serializer() {
        let out = generated();
        assert!(
            out.contains("link_to(addr)"),
            "Missing link_to call:\n{out}"
        );
        assert!(
            out.contains("with_serializer"),
            "Missing with_serializer call:\n{out}"
        );
        assert!(out.contains(".finish()"), "Missing .finish() call:\n{out}");
    }

    #[test]
    fn configure_schema_inbound_link_from_with_deserializer() {
        let out = generated();
        assert!(
            out.contains("link_from(addr)"),
            "Missing link_from call:\n{out}"
        );
        assert!(
            out.contains("with_deserializer(OtaCommandValue::from_bytes)"),
            "Missing with_deserializer call:\n{out}"
        );
    }

    #[test]
    fn configure_schema_key_variants_iterated() {
        let out = generated();
        assert!(
            out.contains("TemperatureReadingKey::Indoor"),
            "Missing Indoor in configure_schema:\n{out}"
        );
        assert!(
            out.contains("TemperatureReadingKey::Outdoor"),
            "Missing Outdoor in configure_schema:\n{out}"
        );
        assert!(
            out.contains("TemperatureReadingKey::Garage"),
            "Missing Garage in configure_schema:\n{out}"
        );
        assert!(
            out.contains("OtaCommandKey::Gateway01"),
            "Missing Gateway01 in configure_schema:\n{out}"
        );
    }

    // ── to_pascal_case ───────────────────────────────────────────────────────

    #[test]
    fn pascal_case_simple() {
        assert_eq!(to_pascal_case("indoor"), "Indoor");
        assert_eq!(to_pascal_case("outdoor"), "Outdoor");
    }

    #[test]
    fn pascal_case_kebab() {
        assert_eq!(to_pascal_case("gateway-01"), "Gateway01");
        assert_eq!(to_pascal_case("sensor-hub-01"), "SensorHub01");
    }

    #[test]
    fn pascal_case_snake() {
        assert_eq!(to_pascal_case("sensor_hub_01"), "SensorHub01");
    }

    #[test]
    fn pascal_case_already_capitalized() {
        assert_eq!(to_pascal_case("Indoor"), "Indoor");
    }

    /// Snapshot: print the full generated output for manual review.
    #[test]
    fn snapshot_full_output() {
        let out = generated();
        // Uncomment to inspect:
        // eprintln!("{out}");
        assert!(!out.is_empty());
    }

    // ── to_snake_case ───────────────────────────────────────────────────────

    #[test]
    fn snake_case_basic() {
        assert_eq!(to_snake_case("WeatherObservation"), "weather_observation");
        assert_eq!(to_snake_case("Temperature"), "temperature");
    }

    #[test]
    fn snake_case_acronym() {
        assert_eq!(to_snake_case("OtaCommand"), "ota_command");
    }

    // ── Extended TOML with new fields ───────────────────────────────────────

    const EXTENDED_TOML: &str = r#"
[project]
name = "weather-sentinel"

[meta]
aimdb_version = "0.5.0"
created_at = "2026-02-24T21:39:15Z"
last_modified = "2026-02-25T10:00:00Z"

[[records]]
name = "WeatherObservation"
buffer = "SpmcRing"
capacity = 256
key_prefix = "weather.observation."
key_variants = ["Vienna", "Munich"]
schema_version = 2
serialization = "json"

[records.observable]
signal_field = "temperature_celsius"
icon = "🌡️"
unit = "°C"

[[records.fields]]
name = "timestamp"
type = "u64"
description = "Unix timestamp in milliseconds"

[[records.fields]]
name = "temperature_celsius"
type = "f32"
description = "Air temperature"
settable = true

[[records.fields]]
name = "humidity_percent"
type = "f32"
description = "Relative humidity"
settable = true

[[records.connectors]]
protocol = "mqtt"
direction = "inbound"
url = "sensors/{variant}/observation"
"#;

    fn extended_state() -> ArchitectureState {
        ArchitectureState::from_toml(EXTENDED_TOML).unwrap()
    }

    fn extended_generated() -> String {
        generate_rust(&extended_state())
    }

    #[test]
    fn schema_type_impl_generated() {
        let out = extended_generated();
        assert!(
            out.contains("impl SchemaType for WeatherObservationValue"),
            "Missing SchemaType impl:\n{out}"
        );
        assert!(
            out.contains("\"weather_observation\""),
            "Missing schema name:\n{out}"
        );
        assert!(
            out.contains("VERSION: u32 = 2"),
            "Missing schema version:\n{out}"
        );
    }

    #[test]
    fn linkable_impl_json_generated() {
        let out = extended_generated();
        assert!(
            out.contains("impl Linkable for WeatherObservationValue"),
            "Missing Linkable impl:\n{out}"
        );
        assert!(
            out.contains("serde_json::to_vec"),
            "Missing serde_json::to_vec call:\n{out}"
        );
        assert!(
            out.contains("serde_json::from_slice"),
            "Missing serde_json::from_slice call:\n{out}"
        );
    }

    #[test]
    fn observable_impl_generated() {
        let out = extended_generated();
        assert!(
            out.contains("impl Observable for WeatherObservationValue"),
            "Missing Observable impl:\n{out}"
        );
        assert!(
            out.contains("self.temperature_celsius"),
            "Missing signal field access:\n{out}"
        );
        assert!(out.contains("\"°C\""), "Missing unit:\n{out}");
    }

    #[test]
    fn settable_impl_generated() {
        let out = extended_generated();
        assert!(
            out.contains("impl Settable for WeatherObservationValue"),
            "Missing Settable impl:\n{out}"
        );
        assert!(
            out.contains("(f32, f32)"),
            "Missing tuple value type:\n{out}"
        );
    }

    #[test]
    fn configure_schema_with_real_deserializer() {
        let out = extended_generated();
        assert!(
            out.contains("with_deserializer(WeatherObservationValue::from_bytes)"),
            "Missing with_deserializer for inbound connector:\n{out}"
        );
    }

    #[test]
    fn data_contracts_import_present() {
        let out = extended_generated();
        assert!(
            out.contains("use aimdb_data_contracts"),
            "Missing aimdb_data_contracts import:\n{out}"
        );
        assert!(
            out.contains("SchemaType"),
            "Missing SchemaType in import:\n{out}"
        );
    }

    #[test]
    fn generate_cargo_toml_output() {
        let state = extended_state();
        let toml = generate_cargo_toml(&state);
        assert!(
            toml.contains("weather-sentinel-common"),
            "Missing crate name:\n{toml}"
        );
        assert!(
            toml.contains("serde_json"),
            "Missing serde_json dep:\n{toml}"
        );
        assert!(
            toml.contains("linkable"),
            "Missing linkable feature:\n{toml}"
        );
    }

    #[test]
    fn generate_lib_rs_output() {
        let lib = generate_lib_rs();
        assert!(lib.contains("no_std"), "Missing no_std attribute:\n{lib}");
        assert!(
            lib.contains("extern crate alloc"),
            "Missing alloc extern:\n{lib}"
        );
        assert!(lib.contains("mod schema"), "Missing schema module:\n{lib}");
        assert!(
            lib.contains("pub use schema::*"),
            "Missing re-export:\n{lib}"
        );
    }

    #[test]
    fn schema_rs_has_no_generated_header() {
        let out = generate_schema_rs(&extended_state());
        assert!(
            !out.contains("@generated"),
            "schema.rs should not have @generated header:\n{out}"
        );
    }
}
