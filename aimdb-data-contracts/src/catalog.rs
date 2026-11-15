//! [`catalog!`](crate::catalog): a contracts crate's contracts, listed in one
//! place and locked in `aimdb.lock`.

/// One entry of a contracts crate's `CATALOG`: a contract's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractId {
    /// [`SchemaType::NAME`](crate::SchemaType::NAME)
    pub name: &'static str,
    /// [`SchemaType::VERSION`](crate::SchemaType::VERSION)
    pub version: u32,
    /// First eight bytes of the locked SHA-256, interpreted as big-endian.
    pub shape: Option<u64>,
}

impl ContractId {
    /// Stable FNV-1a 32 identifier of the contract name.
    pub const fn name_id(self) -> u32 {
        crate::name_id(self.name)
    }
    /// Locked hash prefix for this version, when present in the lock.
    pub const fn shape(self) -> Option<u64> {
        self.shape
    }
}

#[cfg(feature = "aimdb-core")]
impl From<ContractId> for aimdb_core::envelope::ContractRef {
    fn from(id: ContractId) -> Self {
        Self::new(id.name_id(), id.version, id.shape())
    }
}

/// List a contracts crate's contracts, and lock the wire shape of each
/// `NAME@VERSION` in `aimdb.lock` at the crate root.
///
/// ```rust,ignore
/// aimdb_data_contracts::catalog! {
///     contracts: [
///         TemperatureV1,       // mqtt_demo.temperature@1, kept for the migration chain
///         Temperature,         // mqtt_demo.temperature@2
///         TemperatureCommand,
///         ButtonPress,
///     ],
///     helpers: [Mode],         // enums nested in the contracts
/// }
/// ```
///
/// Without helpers, the contracts can be listed alone:
/// `catalog! { Temperature, ButtonPress }`.
///
/// Requires a committed `aimdb.lock` at the crate root. Bootstrap a new crate
/// with a file containing `version = 1`, then run `AIMDB_UPDATE_LOCK=1 cargo test`.
/// Run the lock test in every firmware and hub CI build: a transmitted shape
/// is read from the lock, not calculated from the compiled struct.
///
/// Generates `pub const CATALOG: &[ContractId]` (contracts only, `no_std`)
/// and a test in the crate's own unit tests that checks the catalog against
/// `aimdb.lock`. The test needs the `lock` feature, which belongs in
/// `[dev-dependencies]` only, so firmware builds never see it:
///
/// ```toml
/// [dev-dependencies]
/// aimdb-data-contracts = { version = "…", features = ["lock"] }
/// ```
///
/// Every enum nested in a contract (behind a field, `Option`, `Vec`, a map
/// or another enum) goes in `helpers`: it is traced first, has no
/// `NAME@VERSION` of its own, and its shape is part of every contract that
/// uses it. A missing one fails the check.
///
/// `AIMDB_UPDATE_LOCK=1 cargo test` writes new contracts and versions to the
/// lock. A shape changed under the same `NAME@VERSION` always fails. The
/// supported contracts and the rules are in the usage guide, "A contracts
/// crate as the source of truth".
#[macro_export]
macro_rules! catalog {
    (
        contracts: [$($ty:ty),+ $(,)?]
        $(, helpers: [$($helper:ty),* $(,)?])?
        $(,)?
    ) => {
        $crate::catalog! {
            @lock_dir ::core::env!("CARGO_MANIFEST_DIR"),
            contracts: [$($ty),+],
            helpers: [$($($helper),*)?],
        }
    };
    // Not an API: the directory of `aimdb.lock`, for this crate's own tests.
    (
        @lock_dir $dir:expr,
        contracts: [$($ty:ty),+ $(,)?]
        $(, helpers: [$($helper:ty),* $(,)?])?
        $(,)?
    ) => {
        $(impl $crate::ContractShape for $ty {
            const SHAPE: ::core::option::Option<u64> = $crate::locked_shape(
                ::core::include_str!(::core::concat!($dir, "/aimdb.lock")),
                <$ty as $crate::SchemaType>::NAME,
                <$ty as $crate::SchemaType>::VERSION,
            );
        })+

        /// This crate's contracts, as listed in `catalog!`.
        pub const CATALOG: &[$crate::ContractId] = &[
            $($crate::ContractId {
                name: <$ty as $crate::SchemaType>::NAME,
                version: <$ty as $crate::SchemaType>::VERSION,
                shape: <$ty as $crate::ContractShape>::SHAPE,
            }),+
        ];

        const _: () = ::core::assert!(
            !$crate::has_name_id_collision(CATALOG),
            "catalog!: distinct contract names have the same FNV-1a name_id"
        );

        // `cfg(test)` of the calling crate: the invocation is gone before it
        // expands, so only the crate's own tests see it. The test is
        // generated in the caller's module, where the listed paths resolve.
        #[cfg(test)]
        $crate::__catalog_lock_test!($dir; [$($ty),+] [$($($helper),*)?]);
    };
    ($($ty:ty),+ $(,)?) => {
        $crate::catalog! { contracts: [$($ty),+] }
    };
}

#[cfg(feature = "lock")]
#[doc(hidden)]
#[macro_export]
macro_rules! __catalog_lock_test {
    ($dir:expr; [$($ty:ty),+] [$($helper:ty),*]) => {
        #[test]
        fn aimdb_lock() {
            let helpers: &[$crate::lock::Helper] = &[$($crate::lock::helper::<$helper>()),*];
            $crate::lock::check(
                $dir,
                &[$($crate::lock::trace::<$ty>(helpers)),+],
            );
        }
    };
}

#[cfg(not(feature = "lock"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __catalog_lock_test {
    ($($tt:tt)*) => {
        ::core::compile_error!(
            "catalog!: the aimdb.lock test needs aimdb-data-contracts' `lock` feature; \
             enable it in [dev-dependencies]"
        );
    };
}
