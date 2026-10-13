use super::*;

#[test]
fn wrap_and_reveal_round_trips_bit_exact() {
    for value in [0.0_f64, -0.0, 100.05, f64::MIN_POSITIVE, 1.0e300] {
        let fenced = DisplayOnly::new(value);
        assert_eq!(fenced.reveal().to_bits(), value.to_bits(), "the fence stores, never converts");
    }
}

#[test]
fn copies_and_clones_stay_fenced() {
    let fenced = DisplayOnly::new(100.05_f64);
    let copied = fenced;
    assert_eq!(copied.reveal().to_bits(), 100.05_f64.to_bits());
    assert_eq!(copied, fenced, "fenced values compare as fenced values");
    let owned = DisplayOnly::new("label".to_owned());
    let cloned = owned.clone();
    assert_eq!(cloned.reveal(), "label", "non-Copy payloads clone wrapped");
}

#[test]
fn money_crossing_refuses_unconditionally() {
    // Refusal is unconditional, not validation: even a clean integral
    // display value never becomes a decision denomination.
    for value in [0.0_f64, 100.0, 100.05] {
        let error = DisplayOnly::new(value).try_into_money().expect_err("display never mints money");
        assert_eq!(
            error,
            DisplayOnlyError::DecisionRefused,
            "the refusal names itself: {error}"
        );
    }
}
