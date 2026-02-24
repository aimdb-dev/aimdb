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

use crate::state::{ArchitectureState, ConnectorDirection, RecordDef};

// ── Public API ────────────────────────────────────────────────────────────────

/// Generate a complete Rust source file from architecture state.
///
/// The returned string can be written to `src/generated_schema.rs`.
/// It contains:
/// - One `<Name>Value` struct per record (with `Serialize` / `Deserialize`)
/// - One `<Name>Key` enum per record (with `#[derive(RecordKey)]`)
/// - A `configure_schema<R>()` function wiring all records into `AimDbBuilder`
pub fn generate_rust(state: &ArchitectureState) -> String {
    let imports = emit_imports();

    let record_items: Vec<TokenStream> = state
        .records
        .iter()
        .flat_map(|rec| {
            let value_struct = emit_value_struct(rec);
            let key_enum = emit_key_enum(rec);
            [value_struct, key_enum]
        })
        .collect();

    let configure_fn = emit_configure_schema(state);

    let file_tokens = quote! {
        #imports
        #(#record_items)*
        #configure_fn
    };

    let syntax_tree = syn::parse2(file_tokens).expect("generated tokens should be valid Rust");
    let formatted = prettyplease::unparse(&syntax_tree);

    // Prepend the @generated header (outside the token stream since it's a comment)
    let header = "\
// @generated — do not edit manually.\n\
// Source: .aimdb/state.toml — edit via `aimdb generate` or the architecture agent.\n\
// Regenerate: `aimdb generate` or confirm a proposal in the architecture agent.\n\n";

    format!("{header}{formatted}")
}

// ── Imports ──────────────────────────────────────────────────────────────────

fn emit_imports() -> TokenStream {
    quote! {
        use aimdb_core::buffer::BufferCfg;
        use aimdb_core::builder::AimDbBuilder;
        use aimdb_derive::RecordKey;
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
        #[derive(RecordKey, Clone, Copy, PartialEq, Eq)]
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

    let connector_block = rec.connectors.first().map(|conn| {
        let link_fn = match conn.direction {
            ConnectorDirection::Outbound => format_ident!("link_to"),
            ConnectorDirection::Inbound => format_ident!("link_from"),
        };
        let todo_comment = match conn.direction {
            ConnectorDirection::Outbound => {
                "TODO: add .with_serializer(|v| serde_json::to_vec(v).map_err(Into::into))"
            }
            ConnectorDirection::Inbound => {
                "TODO: add .with_deserializer(|bytes| serde_json::from_slice(bytes).map_err(Into::into))"
            }
        };
        quote! {
            if let Some(addr) = key.link_address() {
                let _ = #todo_comment;
                reg.#link_fn(addr);
            }
        }
    });

    quote! {
        for key in [
            #(#key_type::#variant_idents,)*
        ] {
            builder.configure::<#value_type>(key, |reg| {
                reg.buffer_cfg(#buffer_tokens);
                #connector_block
            });
        }
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

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
            out.contains("use aimdb_derive::RecordKey;"),
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
            out.contains("#[derive(RecordKey, Clone, Copy, PartialEq, Eq)]"),
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
            "Missing SpmcRing buffer_cfg call:\n{out}"
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
            "Missing Mailbox buffer_cfg call:\n{out}"
        );
    }

    #[test]
    fn configure_schema_outbound_link_to() {
        let out = generated();
        assert!(
            out.contains("link_to(addr)"),
            "Missing link_to call:\n{out}"
        );
    }

    #[test]
    fn configure_schema_inbound_link_from() {
        let out = generated();
        assert!(
            out.contains("link_from(addr)"),
            "Missing link_from call:\n{out}"
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
}
