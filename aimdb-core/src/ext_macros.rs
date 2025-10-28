//! Extension trait macros for runtime adapters
//!
//! This module provides macros to reduce boilerplate when implementing
//! extension traits for different runtime adapters. The pattern is nearly
//! identical across all adapters, differing only in:
//! - Trait name
//! - Runtime adapter type
//! - Buffer implementation type

/// Generate an extension trait for convenient record configuration
///
/// This macro generates both the trait definition and implementation for
/// a runtime adapter's extension methods (buffer, source, tap).
///
/// # Arguments
///
/// * `$trait_name` - Name of the extension trait (e.g., `TokioRecordRegistrarExt`)
/// * `$runtime` - Runtime adapter type (e.g., `TokioAdapter`)
/// * `$buffer` - Buffer implementation type (e.g., `TokioBuffer`)
/// * `$feature_gate` - Optional feature flag to gate the implementation
/// * `$buffer_new` - Expression to create the buffer (handles const generics for Embassy)
///
/// # Example
///
/// ```ignore
/// // In tokio adapter
/// impl_record_registrar_ext! {
///     TokioRecordRegistrarExt,
///     TokioAdapter,
///     TokioBuffer,
///     "tokio-runtime",
///     |cfg| TokioBuffer::<T>::new(cfg)
/// }
///
/// // In embassy adapter
/// impl_record_registrar_ext! {
///     EmbassyRecordRegistrarExt,
///     EmbassyAdapter,
///     EmbassyBuffer,
///     ["embassy-runtime", "embassy-sync"],
///     |cfg| EmbassyBuffer::<T, 16, 4, 4, 1>::new(cfg)
/// }
/// ```
#[macro_export]
macro_rules! impl_record_registrar_ext {
    // Version with single feature gate
    (
        $trait_name:ident,
        $runtime:ty,
        $buffer:ty,
        $feature:literal,
        $buffer_new:expr
    ) => {
        /// Extension trait for convenient configuration with this runtime
        ///
        /// This trait provides high-level convenience methods for configuring records,
        /// automatically handling buffer creation and runtime context extraction.
        pub trait $trait_name<'a, T>
        where
            T: Send + Sync + Clone + core::fmt::Debug + 'static,
        {
            /// Configures a buffer using inline configuration
            fn buffer(
                &'a mut self,
                cfg: $crate::buffer::BufferCfg,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>;

            /// Registers a producer with automatic runtime context injection
            fn source<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Producer<T, $runtime>) -> Fut
                    + Send
                    + Sync
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static;

            /// Registers a consumer with automatic runtime context injection
            fn tap<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Consumer<T, $runtime>) -> Fut
                    + Send
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static;
        }

        #[cfg(feature = $feature)]
        impl<'a, T> $trait_name<'a, T> for $crate::RecordRegistrar<'a, T, $runtime>
        where
            T: Send + Sync + Clone + core::fmt::Debug + 'static,
        {
            fn buffer(
                &'a mut self,
                cfg: $crate::buffer::BufferCfg,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime> {
                use $crate::buffer::Buffer;

                #[cfg(feature = "std")]
                let buffer = Box::new($buffer_new(&cfg));

                #[cfg(not(feature = "std"))]
                let buffer = {
                    extern crate alloc;
                    alloc::boxed::Box::new($buffer_new(&cfg))
                };

                self.buffer_raw(buffer)
            }

            fn source<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Producer<T, $runtime>) -> Fut
                    + Send
                    + Sync
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static,
            {
                self.source_raw(|producer, ctx_any| {
                    let ctx = $crate::RuntimeContext::extract_from_any(ctx_any);
                    f(ctx, producer)
                })
            }

            fn tap<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Consumer<T, $runtime>) -> Fut
                    + Send
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static,
            {
                self.tap_raw(|consumer, ctx_any| {
                    let ctx = $crate::RuntimeContext::extract_from_any(ctx_any);
                    f(ctx, consumer)
                })
            }
        }
    };

    // Version with multiple feature gates (all must be enabled)
    (
        $trait_name:ident,
        $runtime:ty,
        $buffer:ty,
        [$($feature:literal),+],
        $buffer_new:expr
    ) => {
        /// Extension trait for convenient configuration with this runtime
        ///
        /// This trait provides high-level convenience methods for configuring records,
        /// automatically handling buffer creation and runtime context extraction.
        pub trait $trait_name<'a, T>
        where
            T: Send + Sync + Clone + core::fmt::Debug + 'static,
        {
            /// Configures a buffer using inline configuration
            fn buffer(
                &'a mut self,
                cfg: $crate::buffer::BufferCfg,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>;

            /// Registers a producer with automatic runtime context injection
            fn source<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Producer<T, $runtime>) -> Fut
                    + Send
                    + Sync
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static;

            /// Registers a consumer with automatic runtime context injection
            fn tap<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Consumer<T, $runtime>) -> Fut
                    + Send
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static;
        }

        #[cfg(all($(feature = $feature),+))]
        impl<'a, T> $trait_name<'a, T> for $crate::RecordRegistrar<'a, T, $runtime>
        where
            T: Send + Sync + Clone + core::fmt::Debug + 'static,
        {
            fn buffer(
                &'a mut self,
                cfg: $crate::buffer::BufferCfg,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime> {
                use $crate::buffer::Buffer;

                #[cfg(feature = "std")]
                let buffer = Box::new($buffer_new(&cfg));

                #[cfg(not(feature = "std"))]
                let buffer = {
                    extern crate alloc;
                    alloc::boxed::Box::new($buffer_new(&cfg))
                };

                self.buffer_raw(buffer)
            }

            fn source<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Producer<T, $runtime>) -> Fut
                    + Send
                    + Sync
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static,
            {
                self.source_raw(|producer, ctx_any| {
                    let ctx = $crate::RuntimeContext::extract_from_any(ctx_any);
                    f(ctx, producer)
                })
            }

            fn tap<F, Fut>(
                &'a mut self,
                f: F,
            ) -> &'a mut $crate::RecordRegistrar<'a, T, $runtime>
            where
                F: FnOnce($crate::RuntimeContext<$runtime>, $crate::Consumer<T, $runtime>) -> Fut
                    + Send
                    + 'static,
                Fut: core::future::Future<Output = ()> + Send + 'static,
            {
                self.tap_raw(|consumer, ctx_any| {
                    let ctx = $crate::RuntimeContext::extract_from_any(ctx_any);
                    f(ctx, consumer)
                })
            }
        }
    };
}
