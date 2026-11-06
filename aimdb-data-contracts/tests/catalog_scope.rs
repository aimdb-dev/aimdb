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
    }
}
