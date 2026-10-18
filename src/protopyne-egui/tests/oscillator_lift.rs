use super::*;

const KINDS: [OscillatorKind; 10] = [
    OscillatorKind::Aroon,
    OscillatorKind::Cci,
    OscillatorKind::Cmo,
    OscillatorKind::Mfi,
    OscillatorKind::Rsi,
    OscillatorKind::Srsi,
    OscillatorKind::Stoch,
    OscillatorKind::TrendSi,
    OscillatorKind::Voss,
    OscillatorKind::Raw,
];

const PROBE: [f64; 9] = [-300.0, -100.0, -50.0, -1.0, 0.0, 1.0, 50.0, 100.0, 300.0];

#[test]
fn bounded_equals_squash_of_normalize_per_kind() {
    for kind in KINDS {
        for raw in PROBE {
            let bounded = normalize_oscillator_bounded(raw, kind).expect("evaluable kind");
            let composed = squash_to_100(normalize_oscillator(raw, kind).expect("evaluable kind"));
            assert_eq!(
                bounded.to_bits(),
                composed.to_bits(),
                "bounded == squash(normalize) for {kind:?} at {raw}"
            );
        }
    }
}

#[test]
fn lifts_agree_with_scalar_composition_slot_for_slot() {
    for kind in KINDS {
        let raw: Vec<Option<f64>> = PROBE.iter().copied().map(Some).collect();
        let series = normalize_series(&raw, kind).expect("evaluable kind");
        let bounded = normalize_series_bounded(&raw, kind).expect("evaluable kind");
        let via_squash = squash_series_to_100(&series);
        assert_eq!(
            bounded, via_squash,
            "series bounded composition for {kind:?}"
        );
        assert_eq!(bounded.len(), raw.len(), "length-preserving for {kind:?}");
    }
}

#[test]
fn lifts_on_degenerate_inputs() {
    for kind in KINDS {
        assert!(
            normalize_series(&[], kind)
                .expect("evaluable kind")
                .is_empty(),
            "empty in, empty out for {kind:?}"
        );
        assert!(
            squash_series_to_100(&[]).is_empty(),
            "empty in, empty out for {kind:?}"
        );
        assert_eq!(
            normalize_series(&[None], kind).expect("evaluable kind"),
            vec![None],
            "single hole for {kind:?}"
        );
        assert_eq!(
            normalize_series_bounded(&[None], kind).expect("evaluable kind"),
            vec![None],
            "single bounded hole for {kind:?}"
        );
    }
}

#[test]
fn lifts_refuse_the_cdv_row() {
    // The lift contract carries the catalog's refusal: `Cdv` never lifts,
    // so no bounded/unbounded path can silently substitute constants.
    let refused = OscillatorKind::Cdv;
    assert!(normalize_series(&[Some(1.0)], refused).is_err());
    assert!(normalize_series_bounded(&[Some(1.0)], refused).is_err());
    assert!(normalize_oscillator(1.0, refused).is_err());
    assert!(normalize_oscillator_bounded(1.0, refused).is_err());
}
