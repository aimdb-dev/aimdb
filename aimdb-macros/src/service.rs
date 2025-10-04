//! Service Macro Implementation
//!
//! This module contains the core logic for the `#[service]` procedural macro,
//! which generates runtime-specific code for Embassy and Tokio environments.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Error, FnArg, ItemFn, Result, ReturnType, Type};

/// Expands the service macro for the given function
///
/// This function analyzes the input async function and generates appropriate
/// code for the detected runtime (Embassy vs Tokio vs generic).
pub fn expand_service_macro(input_fn: ItemFn) -> Result<TokenStream> {
    // Validate the function signature
    validate_service_function(&input_fn)?;

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_body = &input_fn.block;
    let fn_inputs = &input_fn.sig.inputs;
    let fn_output = &input_fn.sig.output;
    let fn_attrs = &input_fn.attrs;

    // Extract function parameters (excluding self)
    let param_names = extract_param_names(fn_inputs);

    // Generate runtime-specific code
    let expanded = quote! {
        // Original function (kept for direct calls if needed)
        #(#fn_attrs)*
        #fn_vis async fn #fn_name(#fn_inputs) #fn_output #fn_body

        // Embassy-specific implementation
        #[cfg(feature = "embassy-runtime")]
        mod #fn_name {
            use super::*;

            /// Embassy task wrapper for the service
            ///
            /// This function is decorated with `#[embassy_executor::task]` to enable
            /// proper spawning on Embassy executors. It calls the original service
            /// function and handles any errors appropriately for embedded environments.
            #[embassy_executor::task]
            pub async fn task(#fn_inputs) {
                use aimdb_core::DbError;

                match super::#fn_name(#(#param_names),*).await {
                    Ok(()) => {
                        #[cfg(feature = "tracing")]
                        tracing::info!("Service {} completed successfully", stringify!(#fn_name));
                    }
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!(?e, "Service {} failed", stringify!(#fn_name));

                        // In Embassy, we typically want to restart failed services
                        // This is a policy decision that can be customized
                        #[cfg(feature = "std")]
                        panic!("Service {} failed: {}", stringify!(#fn_name), e);

                        #[cfg(not(feature = "std"))]
                        {
                            // In no_std, we can't panic with formatted strings
                            // Consider implementing a restart mechanism here
                        }
                    }
                }
            }

            /// Spawns the service on an Embassy executor
            ///
            /// # Arguments
            /// * `spawner` - The Embassy spawner to use for task creation
            ///
            /// # Returns
            /// `Ok(())` if the task was successfully spawned, `Err(_)` if spawning failed
            pub fn spawn(
                spawner: &embassy_executor::Spawner,
                #fn_inputs
            ) -> Result<(), embassy_executor::SpawnError> {
                spawner.spawn(task(#(#param_names),*))
            }
        }

        // Tokio-specific implementation
        #[cfg(feature = "tokio-runtime")]
        mod #fn_name {
            use super::*;
            use std::future::Future;

            /// Spawns the service on the Tokio runtime
            ///
            /// This function spawns the service as a Tokio task and returns a handle
            /// that can be awaited or detached as needed.
            ///
            /// # Returns
            /// A `JoinHandle` that resolves to the service result
            pub fn spawn(#fn_inputs) -> tokio::task::JoinHandle<aimdb_core::DbResult<()>> {
                tokio::task::spawn(async move {
                    match super::#fn_name(#(#param_names),*).await {
                        Ok(result) => {
                            #[cfg(feature = "tracing")]
                            tracing::info!("Service {} completed successfully", stringify!(#fn_name));
                            Ok(result)
                        }
                        Err(e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(?e, "Service {} failed", stringify!(#fn_name));
                            Err(e)
                        }
                    }
                })
            }

            /// Spawns the service and awaits its completion
            ///
            /// This is a convenience function for services that should run to completion
            /// rather than being spawned and detached.
            pub async fn spawn_and_wait(#fn_inputs) -> aimdb_core::DbResult<()> {
                match spawn(#(#param_names),*).await {
                    Ok(result) => result,
                    Err(join_error) => {
                        use aimdb_tokio_adapter::TokioErrorSupport;
                        Err(aimdb_core::DbError::from_join_error(join_error))
                    }
                }
            }
        }

        // Generic implementation (when no runtime features are enabled)
        #[cfg(not(any(feature = "embassy-runtime", feature = "tokio-runtime")))]
        mod #fn_name {
            use super::*;

            /// Generic spawn function that directly calls the service
            ///
            /// This implementation is used when no specific runtime features are enabled.
            /// It provides a basic execution model that can work in test environments
            /// or when manual runtime management is desired.
            pub async fn spawn(#fn_inputs) -> aimdb_core::DbResult<()> {
                super::#fn_name(#(#param_names),*).await
            }
        }
    };

    Ok(expanded)
}

/// Validates that the function is suitable for use as a service
fn validate_service_function(func: &ItemFn) -> Result<()> {
    // Check that function is async
    if func.sig.asyncness.is_none() {
        return Err(Error::new_spanned(
            func.sig.fn_token,
            "Service functions must be async",
        ));
    }

    // Check return type is compatible
    match &func.sig.output {
        ReturnType::Default => {
            return Err(Error::new_spanned(
                &func.sig,
                "Service functions must return aimdb_core::DbResult<()> or compatible type",
            ));
        }
        ReturnType::Type(_, ty) => {
            // We could add more sophisticated return type checking here
            // For now, we assume the user knows what they're doing
            if !is_compatible_return_type(ty) {
                return Err(Error::new_spanned(
                    ty,
                    "Service functions should return aimdb_core::DbResult<()> or compatible type",
                ));
            }
        }
    }

    Ok(())
}

/// Checks if the return type is compatible with service requirements
fn is_compatible_return_type(_ty: &Type) -> bool {
    // For now, we accept any return type but warn about compatibility
    // A more sophisticated implementation could check for:
    // - aimdb_core::DbResult<T>
    // - Result<T, aimdb_core::DbError>
    // - () (unit type for fire-and-forget services)

    // TODO: Implement more sophisticated type checking
    true
}

/// Extracts parameter names from function inputs
fn extract_param_names(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::Token![,]>,
) -> Vec<TokenStream> {
    inputs
        .iter()
        .filter_map(|arg| {
            match arg {
                FnArg::Typed(pat_type) => {
                    if let syn::Pat::Ident(pat_ident) = &*pat_type.pat {
                        Some(quote! { #pat_ident })
                    } else {
                        None
                    }
                }
                FnArg::Receiver(_) => None, // Skip self parameters
            }
        })
        .collect()
}
