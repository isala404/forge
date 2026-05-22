//! Trybuild macro tests.
//!
//! Compile-pass: handlers that should compile cleanly.
//! Compile-fail: handlers that should be rejected by macro-level validation.
//! When the macro's diagnostic changes, run with `TRYBUILD=overwrite cargo
//! test --test trybuild` to refresh the `.stderr` files, then review.

#[test]
fn pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
}

#[test]
fn fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/fail/*.rs");
}
