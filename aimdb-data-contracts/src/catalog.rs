//! [`catalog!`](crate::catalog): a contracts crate's contracts, listed in one
//! place and locked in `aimdb.lock`.

/// One entry of a contracts crate's `CATALOG`: a contract's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractId {
    /// [`SchemaType::NAME`](crate::SchemaType::NAME)
    pub name: &'static str,
    /// [`SchemaType::VERSION`](crate::SchemaType::VERSION)
    pub version: u32,
}

/// List a contracts crate's contracts, and lock the wire shape of each
/// `NAME@VERSION` in `aimdb.lock` at the crate root.
///
/// ```rust,ignore
/// aimdb_data_contracts::catalog! {
///     TemperatureV1,       // mqtt_demo.temperature@1, kept for the migration chain
///     Temperature,         // mqtt_demo.temperature@2
///     TemperatureCommand,
///     ButtonPress,
/// }
/// ```
///
/// Generates `pub const CATALOG: &[ContractId]`, `no_std`, and a test in the
/// crate's own unit tests that checks the catalog against `aimdb.lock`. The
/// test needs the `lock` feature, which belongs in `[dev-dependencies]`
/// only, so firmware builds never see it:
///
/// ```toml
/// [dev-dependencies]
/// aimdb-data-contracts = { version = "…", features = ["lock"] }
/// ```
///
/// `AIMDB_UPDATE_LOCK=1 cargo test` writes new contracts and versions to the
/// lock. A shape changed under the same `NAME@VERSION` always fails; the
/// rules are in the usage guide, "A contracts crate as the source of truth".
/// Each listed type needs `Serialize` and `Deserialize`: its shape is traced
/// through serde.
#[macro_export]
macro_rules! catalog {
    ($($ty:ty),+ $(,)?) => {
        /// This crate's contracts, as listed in `catalog!`.
        pub const CATALOG: &[$crate::ContractId] = &[
            $($crate::ContractId {
                name: <$ty as $crate::SchemaType>::NAME,
                version: <$ty as $crate::SchemaType>::VERSION,
            }),+
        ];

        // `cfg(test)` of the calling crate: the module is gone before the
        // inner macro expands, so only the crate's own tests see it.
        #[cfg(test)]
        mod __aimdb_catalog_lock {
            #[allow(unused_imports)]
            use super::*;
            $crate::__catalog_lock_test!($($ty),+);
        }
    };
}

#[cfg(feature = "lock")]
#[doc(hidden)]
#[macro_export]
macro_rules! __catalog_lock_test {
    ($($ty:ty),+) => {
        #[test]
        fn aimdb_lock() {
            $crate::lock::check(
                ::core::env!("CARGO_MANIFEST_DIR"),
                &[$($crate::lock::trace::<$ty>()),+],
            );
        }
    };
}

#[cfg(not(feature = "lock"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __catalog_lock_test {
    ($($ty:ty),+) => {
        ::core::compile_error!(
            "catalog!: the aimdb.lock test needs aimdb-data-contracts' `lock` feature; \
             enable it in [dev-dependencies]"
        );
    };
}
