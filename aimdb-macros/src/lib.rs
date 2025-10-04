//! AimDB Procedural Macros
//!
//! This crate provides procedural macros for AimDB, enabling runtime-agnostic
//! service declarations that work across both Embassy (no_std/embedded) and
//! Tokio (std) environments.
//!
//! # Features
//!
//! - **`#[service]`**: Declares long-running async services that compile for both runtimes
//! - **Runtime Detection**: Automatically generates appropriate code for Embassy vs Tokio
//! - **Zero Runtime Cost**: All macro expansion happens at compile time
//! - **Feature-Gated**: Different code generation based on enabled features
//!
//! # Usage
//!
//! ```rust,ignore
//! use aimdb_core::service;
//!
//! #[service]
//! async fn my_service() -> aimdb_core::DbResult<()> {
//!     loop {
//!         // Service implementation
//!         aimdb_core::sleep(Duration::from_secs(1)).await;
//!     }
//! }
//! ```

use proc_macro::TokenStream;
use syn::{parse_macro_input, ItemFn};

mod service;

/// Declares a long-running async service that works across Embassy and Tokio runtimes
///
/// This macro transforms an async function into a service that can be spawned
/// on either Embassy (embedded) or Tokio (std) runtimes, depending on the
/// enabled features.
///
/// # Code Generation
///
/// The macro generates different code depending on the target runtime:
///
/// ## Embassy (no_std) Target
/// - Adds `#[embassy_executor::task]` attribute for proper task spawning
/// - Generates Embassy-compatible spawn function
/// - Uses `no_std` compatible error handling
///
/// ## Tokio (std) Target  
/// - Generates Tokio spawn wrapper
/// - Uses `std::thread::spawn` compatible signatures
/// - Includes proper error propagation
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::service;
/// use aimdb_core::{DbResult, sleep};
///
/// #[service]
/// async fn data_processor() -> DbResult<()> {
///     loop {
///         // Process data every second
///         println!("Processing data...");
///         sleep(Duration::from_secs(1)).await;
///     }
/// }
///
/// // Generated code enables:
/// // Embassy: data_processor_task::spawn(&spawner);
/// // Tokio: data_processor_spawn().await?;
/// ```
///
/// # Requirements
///
/// The service function must:
/// - Be `async`
/// - Return `aimdb_core::DbResult<()>` or compatible type
/// - Use only runtime-agnostic APIs from `aimdb_core`
///
/// # Feature Detection
///
/// The macro detects the target runtime through feature flags:
/// - `embassy-runtime`: Generates Embassy task code
/// - `tokio-runtime`: Generates Tokio spawn code
/// - No features: Generates minimal compatibility shims
#[proc_macro_attribute]
pub fn service(_args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);

    match service::expand_service_macro(input_fn) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
