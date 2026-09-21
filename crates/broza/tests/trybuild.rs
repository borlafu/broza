//! Compile-fail proof of the `Approved` invariant (`AGENTS.md` §7).
//!
//! The cases in `tests/compile_fail/` are tiny crates that must not compile; the
//! committed `.stderr` files are the expected diagnostics. Regenerate them with
//! `TRYBUILD=overwrite cargo test -p broza --test trybuild`.

#[test]
fn a_write_token_cannot_be_forged_outside_the_safety_kernel() {
    trybuild::TestCases::new().compile_fail("tests/compile_fail/*.rs");
}
