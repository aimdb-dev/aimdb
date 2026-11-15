//! `catalog!` resolves contract and helper paths in the caller's scope, in
//! `CATALOG` and in the generated lock test alike.

#![cfg(feature = "lock")]

use aimdb_data_contracts::{ContractId, SchemaType};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum Mode {
    Off,
    On(u8),
}

#[derive(Serialize, Deserialize)]
pub struct Fixture {
    mode: Mode,
}

impl SchemaType for Fixture {
    const NAME: &'static str = "catalog_scope.fixture";
}

const EXPECTED: &[ContractId] = &[ContractId {
    name: "catalog_scope.fixture",
    version: 1,
    shape: Some(0x1724a7a065257cfb),
}];

/// The explicit form, with `super::` paths for contracts and helpers.
mod listed {
    aimdb_data_contracts::catalog! {
        @lock_dir concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/catalog_scope"),
        contracts: [super::Fixture],
        helpers: [super::Mode],
    }

    #[test]
    fn catalog_lists_the_contract() {
        assert_eq!(CATALOG, super::EXPECTED);
        assert_eq!(
            <super::Fixture as aimdb_data_contracts::ContractShape>::SHAPE,
            CATALOG[0].shape()
        );
        #[cfg(feature = "aimdb-core")]
        {
            let identity: aimdb_core::envelope::ContractRef = CATALOG[0].into();
            assert_eq!(
                identity.name_id(),
                aimdb_core::envelope::name_id("catalog_scope.fixture")
            );
            assert_eq!(identity.shape(), Some(0x1724a7a065257cfb));
        }
    }
}
