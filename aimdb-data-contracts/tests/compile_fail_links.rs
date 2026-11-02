//! Compile-fail coverage for `reg.input(..)` / `reg.output(..)`: a link for
//! the wrong record type, or in the wrong direction, fails with the
//! `Input` / `Output` diagnostics.

#[test]
fn compile_fail_links() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail-links/*.rs");
}
