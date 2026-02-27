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

use crate::state::{
    ArchitectureState, ConnectorDef, ConnectorDirection, RecordDef, SerializationType, TaskDef,
    TaskType,
};

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

// ── Binary crate generators ───────────────────────────────────────────────────

/// Generate `src/main.rs` for the named binary crate.
///
/// Uses `quote!` + `prettyplease` for guaranteed idiomatic formatting.
/// Requires the binary to exist in `state.binaries`. Returns `None` if not found.
pub fn generate_main_rs(state: &ArchitectureState, binary_name: &str) -> Option<String> {
    let bin = state.binaries.iter().find(|b| b.name == binary_name)?;
    let project_name = state
        .project
        .as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("project");
    let common_crate = format_ident!("{}", format!("{}_common", project_name.replace('-', "_")));

    // Collect tasks belonging to this binary
    let tasks: Vec<&TaskDef> = bin
        .tasks
        .iter()
        .filter_map(|tname| state.tasks.iter().find(|t| &t.name == tname))
        .collect();

    let task_use_idents: Vec<syn::Ident> = bin
        .tasks
        .iter()
        .map(|name| format_ident!("{}", name))
        .collect();

    // ── Connector use statements ─────────────────────────────────────────
    let connector_use_stmts: Vec<TokenStream> = bin
        .external_connectors
        .iter()
        .filter_map(|c| {
            match c.protocol.as_str() {
                "mqtt" => Some(quote! { use aimdb_mqtt_connector::MqttConnector; }),
                "knx" => Some(quote! { use aimdb_knx_connector::KnxConnector; }),
                _ => None,
            }
        })
        .collect();

    // ── Connector env-var bindings + construction ────────────────────────
    let connector_let_stmts: Vec<TokenStream> = bin
        .external_connectors
        .iter()
        .map(|c| {
            let var_ident = format_ident!("{}", c.env_var.to_lowercase());
            let var_name = &c.env_var;
            let default = &c.default;
            let ctor: TokenStream = match c.protocol.as_str() {
                "mqtt" => quote! { MqttConnector::new(&#var_ident) },
                "knx" => quote! { KnxConnector::new(&#var_ident) },
                _ => {
                    let msg = format!("build connector for protocol '{}'", c.protocol);
                    quote! { todo!(#msg) }
                }
            };
            let connector_ident = format_ident!("{}_connector", c.protocol);
            quote! {
                let #var_ident = std::env::var(#var_name)
                    .unwrap_or_else(|_| #default.to_string());
                let #connector_ident = #ctor;
            }
        })
        .collect();

    // ── .with_connector(...) chain calls ─────────────────────────────────
    let with_connector_calls: Vec<TokenStream> = bin
        .external_connectors
        .iter()
        .map(|c| {
            let connector_ident = format_ident!("{}_connector", c.protocol);
            quote! { .with_connector(#connector_ident) }
        })
        .collect();

    // ── Task source registrations ────────────────────────────────────────
    let task_registrations: Vec<TokenStream> = tasks
        .iter()
        .flat_map(|task| {
            task.outputs.iter().flat_map(move |output| {
                let variants: Vec<String> = if output.variants.is_empty() {
                    state
                        .records
                        .iter()
                        .find(|r| r.name == output.record)
                        .map(|r| r.key_variants.clone())
                        .unwrap_or_default()
                } else {
                    output.variants.clone()
                };

                let value_type = format_ident!("{}Value", output.record);
                let key_type = format_ident!("{}Key", output.record);
                let task_fn = format_ident!("{}", task.name);

                variants.into_iter().map(move |variant| {
                    let variant_ident = format_ident!("{}", to_pascal_case(&variant));
                    quote! {
                        builder.configure::<#value_type>(#key_type::#variant_ident, |reg| {
                            reg.source(#task_fn);
                        });
                    }
                })
            })
        })
        .collect();

    // ── Assemble via quote! ──────────────────────────────────────────────
    let file_tokens = quote! {
        use aimdb_core::{AimDbBuilder, DbResult};
        use aimdb_tokio_adapter::TokioAdapter;
        #(#connector_use_stmts)*
        use std::sync::Arc;
        use #common_crate::configure_schema;

        mod tasks;
        use tasks::{#(#task_use_idents),*};

        #[tokio::main]
        async fn main() -> DbResult<()> {
            tracing_subscriber::fmt::init();

            #(#connector_let_stmts)*

            let runtime = Arc::new(TokioAdapter::new());

            let mut builder = AimDbBuilder::new()
                .runtime(runtime)
                #(#with_connector_calls)*
                ;

            configure_schema(&mut builder);

            #(#task_registrations)*

            builder.run().await
        }
    };

    let header = format!(
        "// @generated — do not edit manually.\n\
         // Source: .aimdb/state.toml\n\
         // Regenerate: `aimdb generate --binary {binary_name}`\n\n"
    );

    let syntax_tree =
        syn::parse2(file_tokens).expect("generate_main_rs: tokens should be valid Rust");
    Some(format!("{header}{}", prettyplease::unparse(&syntax_tree)))
}

/// Generate `src/tasks.rs` scaffold for the named binary crate.
///
/// Uses `quote!` + `prettyplease` for guaranteed idiomatic formatting.
/// This file is generated **once** — it has no `@generated` header and is
/// then owned by the developer. Signatures must not be changed.
/// Returns `None` if the binary is not found.
pub fn generate_tasks_rs(state: &ArchitectureState, binary_name: &str) -> Option<String> {
    let bin = state.binaries.iter().find(|b| b.name == binary_name)?;
    let project_name = state
        .project
        .as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("project");
    let common_crate = format_ident!("{}", format!("{}_common", project_name.replace('-', "_")));

    // Collect tasks belonging to this binary
    let tasks: Vec<&TaskDef> = bin
        .tasks
        .iter()
        .filter_map(|tname| state.tasks.iter().find(|t| &t.name == tname))
        .collect();

    let task_fns: Vec<TokenStream> = tasks
        .iter()
        .map(|task| {
            let fn_name = format_ident!("{}", task.name);

            // Build parameter list
            let mut params: Vec<TokenStream> = vec![
                quote! { ctx: RuntimeContext<TokioAdapter> },
            ];
            for input in &task.inputs {
                let arg_name = format_ident!("{}", to_snake_case(&input.record));
                let value_type = format_ident!("{}Value", input.record);
                params.push(quote! { #arg_name: Consumer<#value_type, TokioAdapter> });
            }
            for output in &task.outputs {
                let arg_name = format_ident!("{}", to_snake_case(&output.record));
                let value_type = format_ident!("{}Value", output.record);
                params.push(quote! { #arg_name: Producer<#value_type, TokioAdapter> });
            }

            let todo_msg = match &task.task_type {
                TaskType::Agent => "LLM agent stub — implement reasoning loop".to_string(),
                _ => format!("implement: {}", task.description),
            };

            let doc_attr = if task.description.is_empty() {
                quote! {}
            } else {
                let desc = &task.description;
                quote! { #[doc = #desc] }
            };

            quote! {
                #doc_attr
                pub async fn #fn_name(#(#params),*) -> DbResult<()> {
                    todo!(#todo_msg)
                }
            }
        })
        .collect();

    let file_tokens = quote! {
        use aimdb_core::{Consumer, DbResult, Producer, RuntimeContext};
        use aimdb_tokio_adapter::TokioAdapter;
        use #common_crate::*;

        #(#task_fns)*
    };

    let header = format!(
        "// Implement the task bodies; signatures must not change.\n\
         // Regenerate with `aimdb generate --binary {binary_name} --tasks-scaffold`\n\
         // (only writes this file if it does not already exist)\n\n"
    );

    let syntax_tree =
        syn::parse2(file_tokens).expect("generate_tasks_rs: tokens should be valid Rust");
    Some(format!("{header}{}", prettyplease::unparse(&syntax_tree)))
}

/// Generate `Cargo.toml` content for a binary crate.
///
/// Derives dependencies from the binary's tasks and external connectors.
/// Returns `None` if the binary is not found.
pub fn generate_binary_cargo_toml(state: &ArchitectureState, binary_name: &str) -> Option<String> {
    let bin = state.binaries.iter().find(|b| b.name == binary_name)?;
    let project_name = state
        .project
        .as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("project");
    let common_crate_name = format!("{project_name}-common");
    let common_crate_dep = common_crate_name.replace('-', "_");
    let edition = state
        .project
        .as_ref()
        .and_then(|p| p.edition.as_deref())
        .unwrap_or("2024");

    let has_mqtt = bin.external_connectors.iter().any(|c| c.protocol == "mqtt");
    let has_knx = bin.external_connectors.iter().any(|c| c.protocol == "knx");

    let mut optional_connector_deps = String::new();
    if has_mqtt {
        optional_connector_deps.push_str(
            "aimdb-mqtt-connector = { version = \"0.5\", features = [\"tokio-runtime\"] }\n",
        );
    }
    if has_knx {
        optional_connector_deps.push_str(
            "aimdb-knx-connector = { version = \"0.5\", features = [\"tokio-runtime\"] }\n",
        );
    }

    let out = format!(
        "# @generated — do not edit manually.\n\
# Source: .aimdb/state.toml — regenerate with `aimdb generate --binary {binary_name}`\n\
[package]\n\
name = \"{binary_name}\"\n\
version = \"0.1.0\"\n\
edition = \"{edition}\"\n\
\n\
[[bin]]\n\
name = \"{binary_name}\"\n\
path = \"src/main.rs\"\n\
\n\
[dependencies]\n\
{common_crate_dep} = {{ path = \"../{common_crate_name}\" }}\n\
aimdb-core = {{ version = \"0.5\" }}\n\
aimdb-tokio-adapter = {{ version = \"0.5\", features = [\"tokio-runtime\"] }}\n\
{optional_connector_deps}\
tokio = {{ version = \"1\", features = [\"full\"] }}\n\
tracing = \"0.1\"\n\
tracing-subscriber = {{ version = \"0.3\", features = [\"env-filter\"] }}\n"
    );

    Some(out)
}

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
    // The RecordKey derive macro supports a single #[link_address] attribute.
    // We use the first connector for that; additional connectors are resolved
    // via standalone helper functions emitted by `emit_connector_address_fns`.
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

    let address_fns = emit_connector_address_fns(rec);

    quote! {
        #[derive(Debug, RecordKey, Clone, Copy, PartialEq, Eq)]
        #key_prefix_attr
        pub enum #enum_name {
            #(#variants)*
        }

        #address_fns
    }
}

/// Emit standalone address-resolver functions for connectors beyond the first.
///
/// The first connector's addresses are baked into `#[link_address]` on the key
/// enum and exposed via the `RecordKey::link_address()` trait method.  Additional
/// connectors get a `fn {record_snake}_{protocol}_address(key: &{Record}Key) -> Option<&'static str>`
/// function that the configure block can call.
fn emit_connector_address_fns(rec: &RecordDef) -> TokenStream {
    if rec.connectors.len() <= 1 || rec.key_variants.is_empty() {
        return quote! {};
    }

    let key_type = format_ident!("{}Key", rec.name);
    let record_snake = to_snake_case(&rec.name);

    let fns: Vec<TokenStream> = rec
        .connectors
        .iter()
        .skip(1) // first connector uses link_address()
        .map(|conn| {
            let fn_name = format_ident!("{}_{}_address", record_snake, conn.protocol);
            let doc = format!(
                "Link address for `{}` — {} connector (`{}`).",
                rec.name, conn.protocol, conn.direction_label(),
            );

            let arms: Vec<TokenStream> = rec
                .key_variants
                .iter()
                .map(|variant_str| {
                    let variant_ident = format_ident!("{}", to_pascal_case(variant_str));
                    let addr = conn.url.replace("{variant}", variant_str);
                    quote! { #key_type::#variant_ident => Some(#addr), }
                })
                .collect();

            quote! {
                #[doc = #doc]
                pub fn #fn_name(key: &#key_type) -> Option<&'static str> {
                    match key {
                        #(#arms)*
                    }
                }
            }
        })
        .collect();

    quote! { #(#fns)* }
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

    if rec.connectors.is_empty() {
        // No connectors: just buffer
        return quote! {
            for key in [
                #(#key_type::#variant_idents,)*
            ] {
                builder.configure::<#value_type>(key, |reg| {
                    reg.buffer(#buffer_tokens);
                });
            }
        };
    }

    // ── Pre-extract addresses ────────────────────────────────────────────
    // First connector uses `key.link_address()` (from RecordKey derive).
    // Additional connectors use generated helper functions.
    let record_snake = to_snake_case(&rec.name);

    let addr_extractions: Vec<TokenStream> = rec
        .connectors
        .iter()
        .enumerate()
        .map(|(i, conn)| {
            let addr_var = format_ident!("addr_{}", i);
            if i == 0 {
                quote! {
                    let #addr_var = key.link_address().map(|s| s.to_string());
                }
            } else {
                let resolver_fn = format_ident!("{}_{}_address", record_snake, conn.protocol);
                quote! {
                    let #addr_var = #resolver_fn(&key).map(|s| s.to_string());
                }
            }
        })
        .collect();

    // ── Build the configure closure body ─────────────────────────────────
    //
    // `reg.buffer()` consumes the `&mut` borrow and returns a builder, so
    // everything must be a single fluent chain starting from `reg.buffer(...)`.
    // We build two branches: one with connectors wired (when all addresses
    // resolve), one plain buffer fallback.
    let linked_chain = emit_connector_chain(&rec.connectors, &value_type, &buffer_tokens, is_custom);
    let addr_conditions: Vec<TokenStream> = (0..rec.connectors.len())
        .map(|i| {
            let addr_var = format_ident!("addr_{}", i);
            quote! { #addr_var.as_deref() }
        })
        .collect();

    // For a single connector: `if let Some(addr) = addr_0.as_deref() { chain } else { buffer }`
    // For multiple connectors: nest or tuple-match the conditions.
    let body = if rec.connectors.len() == 1 {
        let cond = &addr_conditions[0];
        quote! {
            if let Some(addr_0) = #cond {
                #linked_chain
            } else {
                reg.buffer(#buffer_tokens);
            }
        }
    } else {
        // Multiple connectors: match a tuple of Options.
        // When ALL addresses are present, wire the full chain.
        // Otherwise fall back to buffer-only.
        let some_bindings: Vec<TokenStream> = (0..rec.connectors.len())
            .map(|i| {
                let binding = format_ident!("addr_{}", i);
                quote! { Some(#binding) }
            })
            .collect();
        quote! {
            match (#(#addr_conditions),*) {
                (#(#some_bindings),*) => {
                    #linked_chain
                }
                _ => {
                    reg.buffer(#buffer_tokens);
                }
            }
        }
    };

    quote! {
        for key in [
            #(#key_type::#variant_idents,)*
        ] {
            #(#addr_extractions)*
            builder.configure::<#value_type>(key, |reg| {
                #body
            });
        }
    }
}

/// Build the full fluent chain: `reg.buffer(...).link_X(addr_0)...link_Y(addr_1)...`
///
/// All connector links are chained off a single `reg.buffer()` call so there
/// is only one mutable borrow of `reg`.  Address variables `addr_0`, `addr_1`,
/// etc. are assumed to be in scope as `&str`.
fn emit_connector_chain(
    connectors: &[ConnectorDef],
    value_type: &syn::Ident,
    buffer_tokens: &TokenStream,
    is_custom: bool,
) -> TokenStream {
    // Start the chain with reg.buffer(...)
    let mut chain = quote! { reg.buffer(#buffer_tokens) };

    for (i, conn) in connectors.iter().enumerate() {
        let addr_var = format_ident!("addr_{}", i);

        if is_custom {
            let todo_comment = match conn.direction {
                ConnectorDirection::Outbound => {
                    "TODO: chain .link_to(...).with_serializer(...) — serialization = \"custom\""
                }
                ConnectorDirection::Inbound => {
                    "TODO: chain .link_from(...).with_deserializer(...) — serialization = \"custom\""
                }
            };
            // Can't chain a TODO into the builder, so just emit a let-binding comment
            // after the chain. We'll terminate the chain with `;` below.
            chain = quote! {
                #chain;
                let _ = (#todo_comment, #addr_var)
            };
        } else {
            match conn.direction {
                ConnectorDirection::Inbound => {
                    chain = quote! {
                        #chain
                            .link_from(#addr_var)
                            .with_deserializer(#value_type::from_bytes)
                            .finish()
                    };
                }
                ConnectorDirection::Outbound => {
                    chain = quote! {
                        #chain
                            .link_to(#addr_var)
                            .with_serializer(|v: &#value_type| {
                                v.to_bytes()
                                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
                            })
                            .finish()
                    };
                }
            }
        }
    }

    // Terminate the chain
    quote! { #chain; }
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

// ── Hub crate generators ──────────────────────────────────────────────────────
//
// These functions derive a complete hub binary crate scaffold from state.toml
// without requiring `[[tasks]]` or `[[binaries]]` entries in state.
//
// Hub-internal tasks are identified automatically:
//   - Producers of records with INBOUND connectors → external (stations), skipped
//   - Producers of records with OUTBOUND or no connectors → hub-internal tasks

/// Returns the set of hub-internal task names derived from state.
///
/// A task is hub-internal if it appears as a producer of any record that has
/// no inbound connector (i.e. the hub itself writes that record).
fn hub_task_names(state: &ArchitectureState) -> Vec<String> {
    // Collect external producer names: those that produce records with inbound connectors
    use std::collections::HashSet;
    let external_producers: HashSet<&str> = state
        .records
        .iter()
        .filter(|r| {
            r.connectors
                .iter()
                .any(|c| matches!(c.direction, ConnectorDirection::Inbound))
        })
        .flat_map(|r| r.producers.iter().map(|p| p.as_str()))
        .collect();

    // Hub tasks: appear as producer of a non-inbound record
    let mut seen = HashSet::new();
    let mut tasks: Vec<String> = state
        .records
        .iter()
        .filter(|r| {
            !r.connectors
                .iter()
                .any(|c| matches!(c.direction, ConnectorDirection::Inbound))
        })
        .flat_map(|r| r.producers.iter().cloned())
        .filter(|p| !external_producers.contains(p.as_str()))
        .filter(|p| seen.insert(p.clone()))
        .collect();

    // Also include any consumer of a record that is not a known external producer
    for rec in &state.records {
        for consumer in &rec.consumers {
            let consumer = consumer.clone();
            if !external_producers.contains(consumer.as_str()) && seen.insert(consumer.clone()) {
                tasks.push(consumer);
            }
        }
    }

    tasks
}

/// Generate `src/schema.rs` for the hub binary crate.
///
/// Contains only the `configure_schema` function — no type definitions.
/// Types are imported from the project's common crate.
pub fn generate_hub_schema_rs(state: &ArchitectureState) -> String {
    let project = state
        .project
        .as_ref()
        .expect("generate_hub_schema_rs requires [project] block in state.toml");
    let common_crate = format_ident!("{}", project.name.replace('-', "_") + "_common");

    let configure_fn = emit_configure_schema(state);

    let file_tokens = quote! {
        use aimdb_core::buffer::BufferCfg;
        use aimdb_core::builder::AimDbBuilder;
        use aimdb_executor::Spawn;
        use #common_crate::*;

        #configure_fn
    };

    let header = "// @generated — do not edit manually.\n\
// Source: .aimdb/state.toml — regenerate with `aimdb generate --hub`.\n\n";

    let syntax_tree = syn::parse2(file_tokens).expect("generated tokens should be valid Rust");
    format!("{header}{}", prettyplease::unparse(&syntax_tree))
}

/// Generate `Cargo.toml` for the hub binary crate (`{project.name}-hub`).
pub fn generate_hub_cargo_toml(state: &ArchitectureState) -> String {
    let project = state
        .project
        .as_ref()
        .expect("generate_hub_cargo_toml requires [project] block in state.toml");
    let hub_crate = format!("{}-hub", project.name);
    let common_crate_name = format!("{}-common", project.name);
    let edition = project.edition.as_deref().unwrap_or("2024");

    let has_mqtt = state
        .records
        .iter()
        .any(|r| r.connectors.iter().any(|c| c.protocol == "mqtt"));
    let has_knx = state
        .records
        .iter()
        .any(|r| r.connectors.iter().any(|c| c.protocol == "knx"));
    let has_websocket = state
        .records
        .iter()
        .any(|r| r.connectors.iter().any(|c| c.protocol == "websocket"));

    let mut connector_deps = String::new();
    if has_mqtt {
        connector_deps.push_str(
            "aimdb-mqtt-connector = { version = \"0.5\", features = [\"tokio-runtime\"] }\n",
        );
    }
    if has_knx {
        connector_deps.push_str(
            "aimdb-knx-connector = { version = \"0.5\", features = [\"tokio-runtime\"] }\n",
        );
    }
    if has_websocket {
        connector_deps.push_str("# aimdb-websocket-connector is in aimdb-pro — add path dep here\n# aimdb-websocket-connector = { path = \"../../aimdb-pro/aimdb-websocket-connector\" }\n");
    }

    format!(
        "# @generated — do not edit manually.\n\
# Source: .aimdb/state.toml — regenerate with `aimdb generate --hub`\n\
[package]\n\
name = \"{hub_crate}\"\n\
version = \"0.1.0\"\n\
edition = \"{edition}\"\n\
description = \"Hub binary for {project_name}\"\n\
publish = false\n\
\n\
[[bin]]\n\
name = \"{hub_crate}\"\n\
path = \"src/main.rs\"\n\
\n\
[dependencies]\n\
{common_crate_name} = {{ path = \"../{common_crate_name}\" }}\n\
aimdb-core = {{ version = \"0.5\" }}\n\
aimdb-data-contracts = {{ version = \"0.5\", features = [\"linkable\"] }}\n\
aimdb-tokio-adapter = {{ version = \"0.5\", features = [\"tokio-runtime\"] }}\n\
{connector_deps}\
tokio = {{ version = \"1\", features = [\"full\"] }}\n\
tracing = \"0.1\"\n\
tracing-subscriber = {{ version = \"0.3\", features = [\"env-filter\"] }}\n",
        project_name = project.name
    )
}

/// Generate `src/main.rs` for the hub binary crate.
///
/// Uses `quote!` + `prettyplease` for guaranteed idiomatic formatting —
/// the same pipeline as the rest of the codegen (no raw format strings).
pub fn generate_hub_main_rs(state: &ArchitectureState) -> String {
    let project = state
        .project
        .as_ref()
        .expect("generate_hub_main_rs requires [project] block in state.toml");
    let common_crate = format_ident!("{}", project.name.replace('-', "_") + "_common");

    // ── Protocol detection ────────────────────────────────────────────────
    let has_mqtt = state
        .records
        .iter()
        .any(|r| r.connectors.iter().any(|c| c.protocol == "mqtt"));
    let has_knx = state
        .records
        .iter()
        .any(|r| r.connectors.iter().any(|c| c.protocol == "knx"));

    // ── Connector use statements ──────────────────────────────────────────
    let connector_use_stmts: Vec<TokenStream> = {
        let mut v = vec![];
        if has_mqtt {
            v.push(quote! { use aimdb_mqtt_connector::MqttConnector; });
        }
        if has_knx {
            v.push(quote! { use aimdb_knx_connector::KnxConnector; });
        }
        v
    };

    // ── Connector env-var bindings ────────────────────────────────────────
    let connector_let_stmts: TokenStream = {
        let mut ts = TokenStream::new();
        if has_mqtt {
            ts.extend(quote! {
                let mqtt_broker =
                    std::env::var("MQTT_BROKER").unwrap_or_else(|_| "localhost".to_string());
                let mqtt_url = format!("mqtt://{}", mqtt_broker);
            });
        }
        if has_knx {
            ts.extend(quote! {
                let knx_gateway = std::env::var("KNX_GATEWAY")
                    .unwrap_or_else(|_| "224.0.23.12:3671".to_string());
            });
        }
        ts
    };

    // ── .with_connector(...) chain entries ────────────────────────────────
    let with_connector_calls: Vec<TokenStream> = {
        let mut v = vec![];
        if has_mqtt {
            v.push(quote! { .with_connector(MqttConnector::new(&mqtt_url)) });
        }
        if has_knx {
            v.push(quote! { .with_connector(KnxConnector::new(&knx_gateway)) });
        }
        v
    };

    // ── Inline record configure blocks (the node graph) ───────────────────
    let record_blocks: Vec<TokenStream> = state
        .records
        .iter()
        .map(|r| emit_hub_record_configure_block(r, state))
        .collect();

    // ── String literals ───────────────────────────────────────────────────
    let log_filter = format!(
        "{}_hub=info,aimdb_core=info",
        project.name.replace('-', "_")
    );
    let startup_msg = format!("Starting {} hub", project.name);

    // ── Assemble via quote! — prettyplease formats the whole file ─────────
    let file_tokens = quote! {
        use aimdb_core::{buffer::BufferCfg, AimDbBuilder, DbResult, RecordKey};
        use aimdb_data_contracts::Linkable;
        use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
        #(#connector_use_stmts)*
        use std::sync::Arc;
        use #common_crate::*;

        mod tasks;
        use tasks::*;

        #[tokio::main]
        async fn main() -> DbResult<()> {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| #log_filter.into()),
                )
                .init();

            tracing::info!(#startup_msg);

            #connector_let_stmts

            let runtime = Arc::new(TokioAdapter::new()?);

            let mut builder = AimDbBuilder::new()
                .runtime(runtime)
                #(#with_connector_calls)*
                ;

            #(#record_blocks)*

            builder.run().await
        }
    };

    let header = "// @generated — do not edit manually.\n\
// Source: .aimdb/state.toml\n\
// Regenerate: `aimdb generate --hub`\n\n";

    let syntax_tree =
        syn::parse2(file_tokens).expect("generate_hub_main_rs: tokens should be valid Rust");
    format!("{header}{}", prettyplease::unparse(&syntax_tree))
}

/// Hub-specific record configure block.
///
/// For records produced by a `[[tasks]]`-defined hub task, emits per-variant
/// individual `builder.configure(...)` calls using `.transform()` or
/// `.transform_join()`. For all other records (inbound connector or external
/// source) falls back to the regular loop-based configure block.
fn emit_hub_record_configure_block(rec: &RecordDef, state: &ArchitectureState) -> TokenStream {
    if rec.key_variants.is_empty() {
        let msg = format!("TODO: {}: no key variants defined yet", rec.name);
        return quote! { let _ = (#msg,); };
    }

    // Find a task in [[tasks]] whose outputs include this record
    let producing_task = state
        .tasks
        .iter()
        .find(|t| t.outputs.iter().any(|o| o.record == rec.name));

    match producing_task {
        Some(task) => emit_transform_configure_block(rec, task),
        None => emit_record_configure_block(rec),
    }
}

/// Emit per-variant configure blocks for a hub-task-produced record.
///
/// Generates individual (non-loop) `builder.configure(...)` calls so that
/// each variant can reference its specific input keys for `.transform()` /
/// `.transform_join()`.
fn emit_transform_configure_block(rec: &RecordDef, task: &TaskDef) -> TokenStream {
    let value_type = format_ident!("{}Value", rec.name);
    let key_type = format_ident!("{}Key", rec.name);
    let buffer_tokens = rec.buffer.to_tokens(rec.capacity);

    // Only emit connector chain for mqtt/knx outbound (websocket is a pro feature)
    let has_outbound = rec.connectors.iter().any(|c| {
        matches!(c.direction, ConnectorDirection::Outbound)
            && matches!(c.protocol.as_str(), "mqtt" | "knx")
    });
    let outbound_chain = if has_outbound {
        quote! {
            .link_to(addr)
            .with_serializer(|v: &#value_type| {
                v.to_bytes()
                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
            })
            .finish()
        }
    } else {
        quote! {}
    };

    let variant_idents: Vec<syn::Ident> = rec
        .key_variants
        .iter()
        .map(|v| format_ident!("{}", to_pascal_case(v)))
        .collect();

    let per_variant: Vec<TokenStream> = variant_idents
        .iter()
        .map(|variant_ident| {
            let transform_call = build_transform_call(task, variant_ident);

            if has_outbound {
                let outbound = outbound_chain.clone();
                quote! {
                    {
                        let link_addr = #key_type::#variant_ident
                            .link_address()
                            .map(|s| s.to_string());
                        builder.configure::<#value_type>(#key_type::#variant_ident, |reg| {
                            if let Some(addr) = link_addr.as_deref() {
                                reg.buffer(#buffer_tokens)
                                    #transform_call
                                    #outbound;
                            } else {
                                reg.buffer(#buffer_tokens)
                                    #transform_call;
                            }
                        });
                    }
                }
            } else {
                quote! {
                    builder.configure::<#value_type>(#key_type::#variant_ident, |reg| {
                        reg.buffer(#buffer_tokens)
                            #transform_call;
                    });
                }
            }
        })
        .collect();

    quote! { #(#per_variant)* }
}

/// Build the `.transform(...)` or `.transform_join(...)` call for one variant.
///
/// - 1 input  → `.transform::<InputValue, _>(InputKey::Variant, |b| b.map(task_transform))`
/// - N inputs → `.transform_join(|j| j.input::<...>(Key::Variant)....on_trigger(task_handler))`
fn build_transform_call(task: &TaskDef, variant_ident: &syn::Ident) -> TokenStream {
    if task.inputs.len() != 1 {
        // Multi-input → transform_join
        let handler_ident = format_ident!("{}_handler", task.name);
        let input_calls: Vec<TokenStream> = task
            .inputs
            .iter()
            .map(|inp| {
                let in_val = format_ident!("{}Value", inp.record);
                let in_key = format_ident!("{}Key", inp.record);
                quote! { .input::<#in_val>(#in_key::#variant_ident) }
            })
            .collect();
        quote! {
            .transform_join(|j| {
                j #(#input_calls)*
                    .with_state(())
                    .on_trigger(#handler_ident)
            })
        }
    } else {
        // Single-input → transform + map
        let handler_ident = format_ident!("{}_transform", task.name);
        let inp = &task.inputs[0];
        let in_val = format_ident!("{}Value", inp.record);
        let in_key = format_ident!("{}Key", inp.record);
        quote! {
            .transform::<#in_val, _>(#in_key::#variant_ident, |b| b.map(#handler_ident))
        }
    }
}

/// Generate `src/tasks.rs` stub for the hub binary crate.
///
/// This file is generated **once** — it is not overwritten if it already exists.
/// Task handler signatures are derived from `[[tasks]]` in state.toml:
///
/// | Inputs | Outputs | API                   | Generated stub            |
/// |--------|---------|-----------------------|---------------------------|
/// | N > 1  | ≥ 1     | `.transform_join()`   | `fn task_handler(JoinTrigger, &mut (), &Producer<O, R>)` |
/// | 1      | ≥ 1     | `.transform().map()`  | `fn task_transform(&Input) -> Option<Output>` |
/// | 0      | ≥ 1     | `.source()`           | `async fn task(RuntimeContext, Producer<O, R>)` |
/// | ≥ 1    | 0       | `.tap()`              | `async fn task(RuntimeContext, Consumer<I, R>)` |
pub fn generate_hub_tasks_rs(state: &ArchitectureState) -> String {
    let project = state
        .project
        .as_ref()
        .expect("generate_hub_tasks_rs requires [project] block in state.toml");
    let common_crate = format!("{}_common", project.name.replace('-', "_"));

    let mut fns = String::new();
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();

    for task in &state.tasks {
        handled.insert(task.name.clone());
        let n_in = task.inputs.len();
        let n_out = task.outputs.len();

        let out_t = task
            .outputs
            .first()
            .map(|o| format!("{}Value", o.record))
            .unwrap_or_else(|| "()".to_string());
        let in_t = task
            .inputs
            .first()
            .map(|i| format!("{}Value", i.record))
            .unwrap_or_else(|| "()".to_string());

        if !task.description.is_empty() {
            fns.push_str(&format!("/// {}\n", task.description));
        }

        if n_in > 1 && n_out >= 1 {
            // Multi-input → join handler
            // Returns Pin<Box<dyn Future>> — the only concrete return type that satisfies
            // the for<'a,'b> HRTB on on_trigger. `-> impl Future` does NOT work here.
            let handler = format!("{}_handler", task.name);
            let inputs_doc = task
                .inputs
                .iter()
                .enumerate()
                .map(|(i, inp)| format!("  index {i} = {}", inp.record))
                .collect::<Vec<_>>()
                .join(", ");
            fns.push_str(&format!(
                "/// Join handler — match `trigger.index()` to identify which input fired:\n\
/// {inputs_doc}\n\
pub fn {handler}(\n\
    _trigger: aimdb_core::transform::JoinTrigger,\n\
    _state: &mut (),\n\
    _producer: &aimdb_core::Producer<{out_t}, TokioAdapter>,\n\
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {{\n\
    Box::pin(async move {{ todo!(\"implement {handler}\") }})\n\
}}\n\n"
            ));
        } else if n_in == 1 && n_out >= 1 {
            // Single-input → map transform
            let handler = format!("{}_transform", task.name);
            let input_rec = &task.inputs[0].record;
            let output_rec = task
                .outputs
                .first()
                .map(|o| o.record.as_str())
                .unwrap_or("?");
            fns.push_str(&format!(
                "/// Transform: {input_rec} → {output_rec}\n\
/// Return `Some(value)` to emit, `None` to skip this input.\n\
pub fn {handler}(input: &{in_t}) -> Option<{out_t}> {{\n\
    let _ = input;\n\
    todo!(\"implement {handler}\")\n\
}}\n\n"
            ));
        } else if n_in == 0 && n_out >= 1 {
            // Pure source
            fns.push_str(&format!(
                "pub async fn {}(\n\
    _ctx: aimdb_core::RuntimeContext<TokioAdapter>,\n\
    _producer: aimdb_core::Producer<{out_t}, TokioAdapter>,\n\
) {{\n\
    todo!(\"implement {}\")\n\
}}\n\n",
                task.name, task.name
            ));
        } else if n_in >= 1 && n_out == 0 {
            // Pure sink / tap
            fns.push_str(&format!(
                "pub async fn {}(\n\
    _ctx: aimdb_core::RuntimeContext<TokioAdapter>,\n\
    _consumer: aimdb_core::Consumer<{in_t}, TokioAdapter>,\n\
) {{\n\
    todo!(\"implement {}\")\n\
}}\n\n",
                task.name, task.name
            ));
        }
    }

    // Fallback: any hub tasks NOT in [[tasks]] get a minimal stub
    for task_name in hub_task_names(state) {
        if handled.contains(&task_name) {
            continue;
        }
        fns.push_str(&format!(
            "/// Hub task: add a `[[tasks]]` entry in state.toml for a typed stub.\n\
pub async fn {task_name}() {{\n\
    todo!(\"implement {task_name}\")\n\
}}\n\n"
        ));
    }


    format!(
        "// Implement task bodies; signatures are derived from state.toml [[tasks]].\n\
// This file is scaffolded once — it will not be overwritten on subsequent runs.\n\
// Regenerate signatures: delete this file, then run `aimdb generate --hub`.\n\
\n\
use aimdb_tokio_adapter::TokioAdapter;\n\
use {common_crate}::*;\n\
\n\
{fns}"
    )
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
            out.contains("link_to(addr_0)"),
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
            out.contains("link_from(addr_0)"),
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
