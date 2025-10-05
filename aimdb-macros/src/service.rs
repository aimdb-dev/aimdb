//! Service Macro Implementation
//!
//! Generates runtime-appropriate service spawning implementations:
//! - Embassy: Generates Embassy task with spawner integration
//! - Tokio: Generates dynamic spawning shim using tokio::spawn

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ItemFn, Result};

/// Convert snake_case to PascalCase
fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

pub fn expand_service_macro(input_fn: ItemFn) -> Result<TokenStream> {
    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_body = &input_fn.block;
    let fn_inputs = &input_fn.sig.inputs;
    let fn_output = &input_fn.sig.output;
    let fn_attrs = &input_fn.attrs;

    // Convert function name to PascalCase for struct name
    let fn_name_str = fn_name.to_string();
    let pascal_case_name = to_pascal_case(&fn_name_str);
    let service_struct_name =
        syn::Ident::new(&format!("{}Service", pascal_case_name), fn_name.span());
    let embassy_task_name = syn::Ident::new(&format!("{}_embassy_task", fn_name), fn_name.span());

    // Extract parameter names for forwarding (skip 'self' if present)
    let param_names: Vec<_> = fn_inputs
        .iter()
        .filter_map(|arg| {
            if let syn::FnArg::Typed(pat_type) = arg {
                if let syn::Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
                    Some(&pat_ident.ident)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    Ok(quote! {
        // Original function (preserved for direct calls)
        #(#fn_attrs)*
        #fn_vis async fn #fn_name(#fn_inputs) #fn_output #fn_body

        // Service implementation struct for runtime-neutral spawning
        pub struct #service_struct_name;

        // Embassy task wrapper (only available with embassy-runtime feature)
        #[cfg(feature = "embassy-runtime")]
        #[embassy_executor::task]
        async fn #embassy_task_name(#fn_inputs) {
            let _ = #fn_name(#(#param_names),*).await;
        }

        // Implement the AimDbService trait for this service
        impl aimdb_core::AimDbService for #service_struct_name {
            fn run() -> impl core::future::Future<Output = aimdb_core::DbResult<()>> + Send + 'static {
                #fn_name(#(#param_names),*)
            }

            fn service_name() -> &'static str {
                stringify!(#fn_name)
            }

            // Override the embassy spawning method when embassy-runtime is enabled
            #[cfg(feature = "embassy-runtime")]
            fn spawn_on_embassy(adapter: &impl aimdb_core::SpawnStatically) -> aimdb_core::DbResult<()> {
                if let Some(spawner) = adapter.spawner() {
                    // Use defmt for embedded targets, tracing for std targets
                    #[cfg(all(feature = "tracing", not(feature = "embedded")))]
                    tracing::info!("Spawning Embassy service: {}", stringify!(#fn_name));
                    #[cfg(all(not(feature = "tracing"), feature = "embedded"))]
                    defmt::info!("Spawning Embassy service: {}", defmt::Debug2Format(&stringify!(#fn_name)));

                    match spawner.spawn(#embassy_task_name(#(#param_names),*)) {
                        Ok(_) => {
                            #[cfg(all(feature = "tracing", not(feature = "embedded")))]
                            tracing::debug!("Successfully spawned Embassy service: {}", stringify!(#fn_name));
                            #[cfg(all(not(feature = "tracing"), feature = "embedded"))]
                            defmt::debug!("Successfully spawned Embassy service: {}", defmt::Debug2Format(&stringify!(#fn_name)));
                            Ok(())
                        }
                        Err(_) => {
                            #[cfg(all(feature = "tracing", not(feature = "embedded")))]
                            tracing::error!("Failed to spawn Embassy service: {}", stringify!(#fn_name));
                            #[cfg(all(not(feature = "tracing"), feature = "embedded"))]
                            defmt::error!("Failed to spawn Embassy service: {}", defmt::Debug2Format(&stringify!(#fn_name)));
                            Err(aimdb_core::DbError::internal(0x5003))
                        }
                    }
                } else {
                    #[cfg(all(feature = "tracing", not(feature = "embedded")))]
                    tracing::error!("No spawner available for Embassy service: {}", stringify!(#fn_name));
                    #[cfg(all(not(feature = "tracing"), feature = "embedded"))]
                    defmt::error!("No spawner available for Embassy service: {}", defmt::Debug2Format(&stringify!(#fn_name)));
                    Err(aimdb_core::DbError::internal(0x1001))
                }
            }
        }


    })
}
