//! Catalog identity invariants must fail during cargo check, not just codegen.
#![cfg(feature = "lock")]

#[test]
fn catalog_identity_diagnostics() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail-catalog/*.rs");
    t.pass("tests/fixtures/catalog_seed.rs");
}
