//! AimDB Procedural Macros
//!
//! Provides unified runtime abstractions across Tokio and Embassy runtimes.
//!
//! ## `#[service]` Macro
//!
//! The `#[service]` macro provides a unified way to define service tasks that work
//! across both Tokio and Embassy runtimes:
//!
//! ```ignore
//! #[aimdb_macros::service]
//! async fn my_service(db: &'static AimDb) {
//!     // Service implementation
//! }
//! ```
//!
//! **Tokio Runtime**: The macro has no effect - functions can be spawned dynamically.
//!
//! **Embassy Runtime**: The macro injects `#[embassy_executor::task]` and enforces
//! static lifetimes required by Embassy's static task system.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

/// Marks a function as a runtime-agnostic service task.
///
/// This macro enables a unified workflow across Tokio and Embassy runtimes:
///
/// # For Embassy Runtime
///
/// When `embassy-runtime` feature is enabled, this macro:
/// - Injects `#[embassy_executor::task]` attribute
/// - Validates that parameters use `&'static` lifetimes (required by Embassy)
/// - Ensures the function is compatible with Embassy's static task system
///
/// # For Tokio Runtime  
///
/// When `tokio-runtime` feature is enabled (or by default), this macro:
/// - Acts as a passthrough (no transformation)
/// - Allows flexible lifetimes (no `'static` requirement)
/// - Works with dynamic task spawning
///
/// # Usage
///
/// ## Tokio - Arc Pattern (Recommended!)
///
/// ```ignore
/// use aimdb_core::{service, AimDb, Spawn};
/// use std::sync::Arc;
///
/// #[service]
/// async fn temperature_producer(db: Arc<AimDb>) {
///     loop {
///         let reading = read_sensor().await;
///         db.produce(reading).await.ok();
///         tokio::time::sleep(Duration::from_secs(1)).await;
///     }
/// }
///
/// // Spawn with Arc - no statics needed!
/// let db = Arc::new(builder.build()?);
/// runtime.spawn(temperature_producer(db)).unwrap();
/// ```
///
/// ## Embassy - Static Reference Pattern
///
/// ```ignore
/// use aimdb_core::{service, AimDb};
/// use static_cell::StaticCell;
///
/// #[service]
/// async fn temperature_producer(db: &'static AimDb) {
///     loop {
///         let reading = read_sensor().await;
///         db.produce(reading).await.ok();
///         embassy_time::Timer::after(Duration::from_secs(1)).await;
///     }
/// }
///
/// // Store in static cell
/// static DB: StaticCell<AimDb> = StaticCell::new();
/// let db = DB.init(builder.build().unwrap());
/// spawner.spawn(temperature_producer(db).expect("spawn failed"));
/// ```
///
/// # Cross-Platform Services
///
/// For services that need to work on both runtimes, you can use conditional compilation:
///
/// ```ignore
/// use aimdb_core::service;
///
/// #[cfg(feature = "tokio-runtime")]
/// #[service]
/// async fn universal_service(db: Arc<AimDb>) {
///     // Tokio implementation with Arc
/// }
///
/// #[cfg(feature = "embassy-runtime")]
/// #[service]
/// async fn universal_service(db: &'static AimDb) {
///     // Embassy implementation with static reference
/// }
/// ```
///
/// # Parameter Patterns Summary
///
/// | Runtime | Recommended Pattern | Reason |
/// |---------|---------------------|---------|
/// | Tokio   | `Arc<AimDb>` | Shared ownership, idiomatic Rust |
/// | Embassy | `&'static AimDb` | Static task system requirement |
///
/// # Spawning Services
///
/// ```ignore
/// // Tokio: Arc pattern
/// let db = Arc::new(builder.build()?);
/// runtime.spawn(temperature_producer(db)).unwrap();
///
/// // Embassy: static reference
/// static DB: StaticCell<AimDb> = StaticCell::new();
/// let db = DB.init(builder.build().unwrap());
/// spawner.spawn(temperature_producer(db).expect("spawn failed"));
/// ```
#[proc_macro_attribute]
pub fn service(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    // For Embassy runtime, inject #[embassy_executor::task] and validate lifetimes
    #[cfg(feature = "embassy-runtime")]
    {
        generate_embassy_service(input_fn)
    }

    // For Tokio runtime or default, pass through unchanged
    #[cfg(not(feature = "embassy-runtime"))]
    {
        let output = quote! {
            #input_fn
        };
        output.into()
    }
}

#[cfg(feature = "embassy-runtime")]
fn generate_embassy_service(input_fn: ItemFn) -> TokenStream {
    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_inputs = &input_fn.sig.inputs;
    let fn_output = &input_fn.sig.output;
    let fn_block = &input_fn.block;
    let fn_attrs = &input_fn.attrs;

    // Validate that all reference parameters have 'static lifetime
    for arg in fn_inputs.iter() {
        if let syn::FnArg::Typed(pat_type) = arg {
            if let syn::Type::Reference(type_ref) = &*pat_type.ty {
                if let Some(lifetime) = &type_ref.lifetime {
                    if lifetime.ident != "static" {
                        return syn::Error::new_spanned(
                            lifetime,
                            "Embassy services require 'static lifetime for all references. Use &'static instead."
                        )
                        .to_compile_error()
                        .into();
                    }
                } else {
                    return syn::Error::new_spanned(
                        type_ref,
                        "Embassy services require explicit 'static lifetime for all references. Use &'static instead."
                    )
                    .to_compile_error()
                    .into();
                }
            }
        }
    }

    let output = quote! {
        #(#fn_attrs)*
        #[embassy_executor::task]
        #fn_vis async fn #fn_name(#fn_inputs) #fn_output {
            #fn_block
        }
    };

    output.into()
}
