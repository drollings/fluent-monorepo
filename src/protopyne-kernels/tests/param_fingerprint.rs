//! Canonical parameter fingerprint: `ParamSet::fingerprint` hashes sorted
//! `(name, tag, payload)` frames through the shared FNV-1a helper — never
//! the `Debug` repr, which is not a stability contract. This suite pins
//! insertion-order independence, the int/float distinction (they behave
//! distinctly in `period()`), and a golden stability vector whose values
//! are frozen: any change is a deliberate cache-invalidation event.

use crate::indicator::ParamSet;

#[test]
fn insertion_order_never_matters() {
    let first = ParamSet::new()
        .with_int("length", 14)
        .with_float("mult", 2.0)
        .with_bool("flag", true);
    let second = ParamSet::new()
        .with_bool("flag", true)
        .with_float("mult", 2.0)
        .with_int("length", 14);
    assert_eq!(first.fingerprint(), second.fingerprint());
}

#[test]
fn int_and_float_with_equal_numeric_value_stay_distinct() {
    // `Int(14)` and `Float(14.0)` behave distinctly in `period()`, so they
    // must key distinctly here — by rule, not by `Debug` accident.
    let int = ParamSet::new().with_int("length", 14);
    let float = ParamSet::new().with_float("length", 14.0);
    assert_ne!(int.fingerprint(), float.fingerprint());
}

#[test]
fn every_scalar_type_distinguishes_its_neighbors() {
    let base = ParamSet::new().with_int("length", 14);
    assert_ne!(
        base.fingerprint(),
        ParamSet::new().with_int("length", 15).fingerprint(),
        "int payload matters"
    );
    assert_ne!(
        base.fingerprint(),
        ParamSet::new().with_int("other", 14).fingerprint(),
        "name matters"
    );
    assert_ne!(
        ParamSet::new().with_float("mult", 2.0).fingerprint(),
        ParamSet::new().with_float("mult", 2.5).fingerprint(),
        "float payload matters"
    );
    assert_ne!(
        ParamSet::new().with_bool("flag", true).fingerprint(),
        ParamSet::new().with_bool("flag", false).fingerprint(),
        "bool payload matters"
    );
}

#[test]
fn float_encoding_is_bit_canonical() {
    // Distinct bit patterns hash distinctly, including signed zero.
    let zero = ParamSet::new().with_float("x", 0.0);
    let neg_zero = ParamSet::new().with_float("x", -0.0);
    assert_ne!(zero.fingerprint(), neg_zero.fingerprint());
}

// Golden stability vector: frozen hashes of representative sets. Any change
// invalidates every cache keyed on these, so it must be deliberate.
const GOLDEN_EMPTY: u64 = 14_695_981_039_346_656_037;
const GOLDEN_LEN14: u64 = 17_541_734_461_132_415_688;
const GOLDEN_LEN14_MULT2: u64 = 4_774_006_801_080_475_364;
const GOLDEN_BB_FULL: u64 = 14_372_929_455_755_839_043;

#[test]
fn golden_stability_vector() {
    assert_eq!(ParamSet::new().fingerprint(), GOLDEN_EMPTY, "empty set");
    assert_eq!(
        ParamSet::new().with_int("length", 14).fingerprint(),
        GOLDEN_LEN14,
        "single int"
    );
    assert_eq!(
        ParamSet::new()
            .with_float("mult", 2.0)
            .with_int("length", 14)
            .fingerprint(),
        GOLDEN_LEN14_MULT2,
        "int + float"
    );
    assert_eq!(
        ParamSet::new()
            .with_int("length", 14)
            .with_float("mult", 2.0)
            .with_bool("flag", true)
            .fingerprint(),
        GOLDEN_BB_FULL,
        "int + float + bool"
    );
}
