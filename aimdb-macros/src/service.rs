//! Service Macro Implementation - Clean Runtime-Agnostic Version
//!
//! Generates a simple struct implementing the AimDbService trait.
//! Services are generic over any Runtime implementation, enabling:
//! - Testing with MockRuntime
//! - Runtime flexibility (Tokio, Embassy, custom)
//! - Clean separation of service logic from spawning mechanism
//!
//! The macro only handles service definition. Spawning is delegated to
//! adapter-specific helper methods, keeping runtime concerns isolated.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ItemFn, Result};

/// Convert snake_case to PascalCase
fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

/// Expand the #[service] macro into a clean service implementation
///
/// This generates:
/// 1. Original function (for direct calls if needed)
/// 2. A zero-sized struct named after the service
/// 3. AimDbService trait implementation
///
/// The generated service is generic over any Runtime implementation.
pub fn expand_service_macro(input_fn: ItemFn) -> Result<TokenStream> {
    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_body = &input_fn.block;
    let fn_attrs = &input_fn.attrs;

    // Extract the RuntimeContext parameter to determine the Runtime type
    // We expect: ctx: RuntimeContext<SomeRuntime>
    let ctx_param = input_fn.sig.inputs.first().ok_or_else(|| {
        syn::Error::new_spanned(
            &input_fn.sig,
            "Service function must have a RuntimeContext parameter",
        )
    })?;

    // Convert function name to PascalCase for struct name
    let fn_name_str = fn_name.to_string();
    let pascal_case_name = to_pascal_case(&fn_name_str);
    let service_struct_name = syn::Ident::new(&pascal_case_name, fn_name.span());

    Ok(quote! {
        // Original function (preserved for direct calls if needed)
        #(#fn_attrs)*
        #fn_vis async fn #fn_name(#ctx_param) -> aimdb_core::DbResult<()> #fn_body

        // Service implementation struct
        #[derive(Debug, Clone, Copy)]
        pub struct #service_struct_name;

        impl #service_struct_name {
            /// Spawn this service on a runtime that supports dynamic spawning (e.g., Tokio)
            ///
            /// # Type Parameters
            /// * `R` - The runtime type (must implement Runtime + SpawnDynamically)
            ///
            /// # Arguments
            /// * `runtime` - The runtime instance to spawn on
            ///
            /// # Returns
            /// A join handle to the spawned service, or an error if spawning failed
            #[cfg(feature = "tokio-runtime")]
            pub fn spawn_tokio(
                runtime: &aimdb_tokio_adapter::TokioAdapter,
            ) -> aimdb_executor::ExecutorResult<tokio::task::JoinHandle<aimdb_core::DbResult<()>>> {
                use aimdb_executor::SpawnDynamically;
                let ctx = aimdb_core::RuntimeContext::from_runtime(runtime.clone());
                runtime.spawn(#fn_name(ctx))
            }

            /// Spawn this service on Embassy runtime (requires static task definition)
            ///
            /// This is a placeholder - actual Embassy spawning happens through
            /// macro-generated #[embassy_executor::task] functions.
            #[cfg(feature = "embassy-runtime")]
            pub fn spawn_embassy(
                adapter: &aimdb_embassy_adapter::EmbassyAdapter,
            ) -> aimdb_executor::ExecutorResult<()> {
                if let Some(spawner) = adapter.spawner() {
                    // The Embassy task wrapper is generated below
                    spawner.spawn(embassy_task_wrapper(adapter.clone()))
                        .map_err(|_| aimdb_executor::ExecutorError::SpawnFailed {
                            #[cfg(feature = "std")]
                            message: format!("Failed to spawn Embassy service: {}", stringify!(#fn_name)),
                            #[cfg(not(feature = "std"))]
                            message: "Failed to spawn Embassy service"
                        })
                } else {
                    Err(aimdb_executor::ExecutorError::RuntimeUnavailable {
                        #[cfg(feature = "std")]
                        message: "No Embassy spawner available".to_string(),
                        #[cfg(not(feature = "std"))]
                        message: "No Embassy spawner available"
                    })
                }
            }

            /// Get the service name for logging and debugging
            pub const fn service_name() -> &'static str {
                stringify!(#fn_name)
            }
        }

        // Embassy task wrapper (only with embassy-runtime feature)
        #[cfg(feature = "embassy-runtime")]
        #[embassy_executor::task]
        async fn embassy_task_wrapper(adapter: aimdb_embassy_adapter::EmbassyAdapter) {
            let ctx = aimdb_core::RuntimeContext::new(&adapter);
            let _ = #fn_name(ctx).await;
        }
    })
}
