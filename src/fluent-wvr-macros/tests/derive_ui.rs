//! Compile-time expansion ratchet for the composition shapes and the closed
//! `#[field(...)]` vocabulary. `trybuild` compiles these as a real consumer
//! crate, so an expansion that only works in the macro crate's own tests is
//! caught here.

#[test]
fn composition_shapes_expand_and_unknown_attrs_stay_fenced() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/shapes.rs");
    cases.compile_fail("tests/ui/unknown_attr.rs");
}
