//! Service Macro Implementation - Simplified Version
//!
//! Generates runtime-appropriate service spawning implementations with automatic RuntimeContext injection:
//! - Embassy: Generates Embassy task with spawner integration  
//! - Tokio: Generates dynamic spawning shim using tokio::spawn
//!
//! **Simplified Approach**: All services automatically receive a RuntimeContext,
//! providing consistent access to timing and sleep capabilities across runtimes.

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

    Ok(quote! {
        // Original function (preserved for direct calls)
        #(#fn_attrs)*
        #fn_vis async fn #fn_name(#fn_inputs) #fn_output #fn_body

        // Service implementation struct for runtime-neutral spawning
        pub struct #service_struct_name;

        // Embassy task wrapper (only available with embassy-runtime feature)
        #[cfg(feature = "embassy-runtime")]
        #[embassy_executor::task]
        async fn #embassy_task_name(ctx: aimdb_core::RuntimeContext<aimdb_embassy_adapter::EmbassyAdapter>) {
            let _ = #fn_name(ctx).await;
        }

        // Implement the AimDbService trait for this service
        impl aimdb_executor::AimDbService for #service_struct_name {
            fn run() -> impl core::future::Future<Output = aimdb_executor::ExecutorResult<()>> + Send + 'static {
                async {
                    // Services should be spawned using spawn_on_* methods which provide RuntimeContext
                    Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                        #[cfg(feature = "std")]
                        message: "Use spawn_on_dynamic or spawn_on_static instead".to_string(),
                        #[cfg(not(feature = "std"))]
                        message: "Use spawn_on_dynamic or spawn_on_static instead"
                    })
                }
            }

            fn service_name() -> &'static str {
                stringify!(#fn_name)
            }

            // Dynamic spawning method (for Tokio and similar runtimes)
            fn spawn_on_dynamic<R: aimdb_executor::SpawnDynamically>(adapter: &R) -> aimdb_executor::ExecutorResult<()> {
                use aimdb_core::{RuntimeContext, time::{SleepCapable, TimestampProvider}};

                #[cfg(feature = "tokio-runtime")]
                {
                    // Create a TokioAdapter instance to provide the RuntimeContext
                    let tokio_adapter = match aimdb_tokio_adapter::TokioAdapter::new() {
                        Ok(adapter) => adapter,
                        Err(_) => return Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                            #[cfg(feature = "std")]
                            message: "Failed to create TokioAdapter".to_string(),
                            #[cfg(not(feature = "std"))]
                            message: "Failed to create TokioAdapter"
                        })
                    };
                    let runtime_ctx = RuntimeContext::from_runtime(tokio_adapter);

                    match adapter.spawn(#fn_name(runtime_ctx)) {
                        Ok(_handle) => Ok(()), // Ignore the JoinHandle, just return success
                        Err(_) => Err(aimdb_executor::ExecutorError::SpawnFailed {
                            #[cfg(feature = "std")]
                            message: format!("Failed to spawn service: {}", stringify!(#fn_name)),
                            #[cfg(not(feature = "std"))]
                            message: "Failed to spawn service"
                        })
                    }
                }
                #[cfg(not(feature = "tokio-runtime"))]
                {
                    Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                        #[cfg(feature = "std")]
                        message: "Tokio runtime not available".to_string(),
                        #[cfg(not(feature = "std"))]
                        message: "Tokio runtime not available"
                    })
                }
            }

            // Static spawning method (for Embassy and similar runtimes)
            fn spawn_on_static<R: aimdb_executor::SpawnStatically>(adapter: &R) -> aimdb_executor::ExecutorResult<()> {
                #[cfg(feature = "embassy-runtime")]
                {
                    use aimdb_core::RuntimeContext;

                    if let Some(spawner) = adapter.spawner() {
                        // Use defmt for embedded targets, tracing for std targets
                        #[cfg(all(feature = "tracing", not(feature = "embedded")))]
                        tracing::info!("Spawning Embassy service: {}", stringify!(#fn_name));
                        #[cfg(all(not(feature = "tracing"), feature = "embedded"))]
                        defmt::info!("Spawning Embassy service: {}", defmt::Debug2Format(&stringify!(#fn_name)));

                        // Create RuntimeContext for Embassy
                        let embassy_adapter = match aimdb_embassy_adapter::EmbassyAdapter::new() {
                            Ok(adapter) => adapter,
                            Err(_) => return Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                                #[cfg(feature = "std")]
                                message: "Failed to create EmbassyAdapter".to_string(),
                                #[cfg(not(feature = "std"))]
                                message: "Failed to create EmbassyAdapter"
                            })
                        };
                        let runtime_ctx = RuntimeContext::from_runtime(embassy_adapter);

                        match spawner.spawn(#embassy_task_name(runtime_ctx)) {
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
                                Err(aimdb_executor::ExecutorError::SpawnFailed {
                                    #[cfg(feature = "std")]
                                    message: format!("Failed to spawn Embassy service: {}", stringify!(#fn_name)),
                                    #[cfg(not(feature = "std"))]
                                    message: "Failed to spawn Embassy service"
                                })
                            }
                        }
                    } else {
                        #[cfg(all(feature = "tracing", not(feature = "embedded")))]
                        tracing::error!("No spawner available for Embassy service: {}", stringify!(#fn_name));
                        #[cfg(all(not(feature = "tracing"), feature = "embedded"))]
                        defmt::error!("No spawner available for Embassy service: {}", defmt::Debug2Format(&stringify!(#fn_name)));
                        Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                            #[cfg(feature = "std")]
                            message: "No spawner available for Embassy service".to_string(),
                            #[cfg(not(feature = "std"))]
                            message: "No spawner available for Embassy service"
                        })
                    }
                }
                #[cfg(not(feature = "embassy-runtime"))]
                {
                    Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                        #[cfg(feature = "std")]
                        message: "Embassy runtime not available".to_string(),
                        #[cfg(not(feature = "std"))]
                        message: "Embassy runtime not available"
                    })
                }
            }
        }
    })
}
