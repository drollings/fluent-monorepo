use super::*;

#[test]
fn ticks_cover_window_within_budget() {
    let ticks = nice_ticks(90.0, 110.0, 6);
    assert!(!ticks.is_empty());
    assert!(ticks.len() <= 12);
    assert!(ticks.iter().all(|t| t.value >= 90.0 - 1e-9 && t.value <= 110.0 + 1e-9));
    // Sorted ascending.
    let mut sorted = ticks.clone();
    sorted.sort_by(|a, b| a.value.partial_cmp(&b.value).expect("finite ticks"));
    assert_eq!(sorted.iter().map(|t| t.value.to_bits()).collect::<Vec<_>>(), ticks.iter().map(|t| t.value.to_bits()).collect::<Vec<_>>());
}

#[test]
fn steps_are_round_numbers() {
    let ticks = nice_ticks(0.0, 100.0, 5);
    assert!(ticks.len() >= 3);
    // Gaps between consecutive ticks are equal (uniform grid).
    let gaps: Vec<f64> =
        ticks.windows(2).map(|w| w[1].value - w[0].value).collect();
    for gap in &gaps {
        assert!((gap - gaps[0]).abs() < 1e-9);
    }
}

#[test]
fn degenerate_windows_yield_no_ticks() {
    assert!(nice_ticks(100.0, 100.0, 6).is_empty());
    assert!(nice_ticks(110.0, 90.0, 6).is_empty());
    assert!(nice_ticks(f64::NAN, 110.0, 6).is_empty());
    assert!(nice_ticks(90.0, 110.0, 0).is_empty());
}

#[test]
fn small_ranges_gain_precision() {
    let ticks = nice_ticks(100.0, 100.05, 5);
    assert!(!ticks.is_empty());
    assert!(ticks.iter().all(|t| !t.label.is_empty()));
}

#[test]
fn weights_mark_major_ticks() {
    let ticks = nice_ticks(0.0, 100.0, 10);
    assert!(ticks.iter().any(|t| t.weight == 2));
}
