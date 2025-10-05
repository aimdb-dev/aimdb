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

        // Embassy task wrapper (only compiled when embassy-runtime feature is enabled)
        #[cfg(feature = "embassy-runtime")]
        #[embassy_executor::task]
        async fn #embassy_task_name(#fn_inputs) {
            // Forward to the original function, ignoring the return value
            let _ = #fn_name(#(#param_names),*).await;
        }

        // Service type for runtime-neutral spawning
        pub struct #service_struct_name;

        // Embassy ServiceSpawnable implementation
        #[cfg(feature = "embassy-runtime")]
        impl aimdb_core::runtime::ServiceSpawnable<aimdb_embassy_adapter::EmbassyAdapter> for #service_struct_name {
            fn spawn_with_adapter(
                adapter: &aimdb_embassy_adapter::EmbassyAdapter,
                service_params: aimdb_core::runtime::ServiceParams,
            ) -> aimdb_core::DbResult<()> {
                use aimdb_core::DbError;

                #[cfg(feature = "tracing")]
                tracing::info!("Spawning Embassy service: {}", service_params.service_name);

                // Get the spawner from the adapter
                if let Some(spawner) = adapter.spawner() {
                    // TODO: For now we'll spawn a simple task - this needs to be enhanced
                    // to pass the actual service parameters to the Embassy task
                    match spawner.spawn(#embassy_task_name()) {
                        Ok(_) => {
                            #[cfg(feature = "tracing")]
                            tracing::debug!("Successfully spawned Embassy service: {}", service_params.service_name);
                            Ok(())
                        }
                        Err(_) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!("Failed to spawn Embassy service: {}", service_params.service_name);
                            Err(DbError::internal(0x5001))
                        }
                    }
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("No Embassy spawner available for service: {}", service_params.service_name);
                    Err(DbError::internal(0x5000))
                }
            }
        }

        // Tokio ServiceSpawnable implementation
        #[cfg(feature = "tokio-runtime")]
        impl aimdb_core::runtime::ServiceSpawnable<aimdb_tokio_adapter::TokioAdapter> for #service_struct_name {
            fn spawn_with_adapter(
                _adapter: &aimdb_tokio_adapter::TokioAdapter,
                service_params: aimdb_core::runtime::ServiceParams,
            ) -> aimdb_core::DbResult<()> {
                use aimdb_core::DbError;

                #[cfg(feature = "tracing")]
                tracing::info!("Spawning Tokio service: {}", service_params.service_name);

                // For Tokio, we spawn the service dynamically using tokio::spawn
                let service_name = service_params.service_name.to_string();
                tokio::spawn(async move {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("Starting Tokio service: {}", service_name);

                    // TODO: For now we call with default parameters - this needs to be enhanced
                    // to pass the actual service parameters
                    let result = #fn_name().await;

                    #[cfg(feature = "tracing")]
                    match &result {
                        Ok(_) => tracing::info!("Tokio service completed successfully: {}", service_name),
                        Err(e) => tracing::error!("Tokio service failed: {} - {:?}", service_name, e),
                    }

                    result
                });

                #[cfg(feature = "tracing")]
                tracing::debug!("Successfully spawned Tokio service: {}", service_params.service_name);

                Ok(())
            }
        }
    })
}
