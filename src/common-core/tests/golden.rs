use common_core::golden::{compare_series, measure, MismatchKind, StageTimer};
use common_core::metrics::LatencyHistogram;

fn some(v: &[f64]) -> Vec<Option<f64>> {
    v.iter().copied().map(Some).collect()
}

#[test]
fn golden_placeholder() {
    // Scaffolding placeholder: replaced by the real golden suites below.
    let report = compare_series(&[], &[], 1e-9);
    assert!(report.matches);
}

#[test]
fn identical_series_match() {
    let report = compare_series(&some(&[1.0, 2.0, 3.0]), &some(&[1.0, 2.0, 3.0]), 1e-9);
    assert!(report.matches);
    assert!(report.mismatches.is_empty());
    assert_eq!(report.compared, 3);
}

#[test]
fn tolerance_boundary_just_inside_matches_just_outside_fails() {
    let expected = some(&[1.0]);
    let inside = some(&[1.0 + 0.9e-9]);
    let outside = some(&[1.0 + 1.1e-9]);
    assert!(compare_series(&inside, &expected, 1e-9).matches);
    let report = compare_series(&outside, &expected, 1e-9);
    assert!(!report.matches);
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].index, 0);
}

#[test]
fn mismatch_report_names_index_and_sides() {
    let report = compare_series(&some(&[1.0, 2.0, 5.0]), &some(&[1.0, 2.0, 3.0]), 1e-9);
    assert!(!report.matches);
    let m = &report.mismatches[0];
    assert_eq!(m.index, 2);
    assert_eq!(m.kind, MismatchKind::Value);
    let text = format!("{m}");
    assert!(text.contains('2'), "report names the index: {text}");
    assert!(report.max_abs_diff > 1.9 && report.max_abs_diff < 2.1);
}

#[test]
fn none_vs_value_and_length_mismatch() {
    let report = compare_series(&[None, Some(1.0)], &[Some(1.0), Some(1.0)], 1e-9);
    assert!(!report.matches);
    assert_eq!(report.mismatches[0].kind, MismatchKind::Missing);
    let report = compare_series(&some(&[1.0, 2.0]), &some(&[1.0]), 1e-9);
    assert!(!report.matches);
    assert_eq!(report.mismatches[0].kind, MismatchKind::Length);
}

#[test]
fn non_finite_actual_never_matches() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let report = compare_series(&[Some(bad)], &[Some(1.0)], 1e-9);
        assert!(!report.matches, "{bad} must not match 1.0");
    }
}

#[test]
fn stage_timer_records_exactly_one_observation() {
    let hist = LatencyHistogram::new();
    {
        let _timer = StageTimer::start(&hist);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(hist.count(), 1);
    let value = measure(&hist, || 40 + 2);
    assert_eq!(value, 42);
    assert_eq!(hist.count(), 2);
}
