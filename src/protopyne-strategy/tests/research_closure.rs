//! The research closure: what a default build carries.
//!
//! `FractionalKelly` (capped fractional-Kelly sizing) is the one sizing policy
//! that must never reach live money untuned, so it exists only under the
//! `research` feature — compile-time, not a runtime flag. The type is `pub`,
//! so its presence in a default build raises no dead-code warning; this pin
//! fails the ordinary suite the moment the gate is dropped.
//!
//! The build-matrix gate (`bin/protopyne-build-matrix.sh`) proves the same
//! property on the compiled artifact — the lean rlib's symbol table names no
//! `FractionalKelly`, while the research rlib does — so a gate that is
//! written but ineffective is caught too.

fn read_source(file: &str) -> String {
    let path = format!("{}/src/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{path} is readable"))
}

#[test]
fn fractional_kelly_is_gated_out_of_the_default_build() {
    let elements = read_source("elements.rs");
    assert!(
        elements.contains(
            "#[cfg(feature = \"research\")]\n#[derive(Debug, Clone, Copy, PartialEq)]\npub struct FractionalKelly {"
        ),
        "FractionalKelly must be declared behind the research feature"
    );
    assert!(
        elements.contains("#[cfg(feature = \"research\")]\nimpl FractionalKelly {"),
        "FractionalKelly's constructor must be behind the research feature"
    );
    let lib = read_source("lib.rs");
    assert!(
        lib.contains("#[cfg(feature = \"research\")]\npub use elements::FractionalKelly;"),
        "FractionalKelly's re-export must be behind the research feature"
    );
}
