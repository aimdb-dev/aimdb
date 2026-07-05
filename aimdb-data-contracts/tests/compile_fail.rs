//! Compile-fail coverage for `migration_chain!`'s validation: malformed
//! chains must still fail at compile time with the same assertion
//! messages the old `macro_rules!` produced, regardless of arity.

#![cfg(feature = "migratable")]

#[test]
fn compile_fail_chains() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail/*.rs");
}
