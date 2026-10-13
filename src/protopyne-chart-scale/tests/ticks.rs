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

/// Calibration control (task value: legibility): narrow and wide windows
/// keep density, spacing, and coverage. Gaps floor at span/budget (labels
/// never crowd denser than the budget allows — the headless no-overlap
/// proxy; uniformity itself is pinned by the property test), the grid
/// reaches both window edges within one step, and the count stays within
/// the documented `budget + 1` lattice-edge bound.
#[test]
fn narrow_and_wide_windows_keep_density_spacing_and_coverage() {
    for (name, min, max, budget) in
        [("narrow", 99.5, 100.5, 8), ("wide", 0.0, 1_000_000.0, 8), ("unit", 0.0, 100.0, 5)]
    {
        let ticks = nice_ticks(min, max, budget);
        let span = max - min;
        assert!(ticks.len() >= 2, "{name}: a usable grid has at least two ticks");
        assert!(ticks.len() <= budget + 1, "{name}: count within the documented bound");
        let gaps: Vec<f64> =
            ticks.windows(2).map(|window| window[1].value - window[0].value).collect();
        let step = gaps[0];
        assert!(step > 0.0, "{name}: gaps run forward");
        assert!(ticks[0].value <= min + step, "{name}: grid reaches the lower edge");
        assert!(
            ticks.last().expect("non-empty grid").value >= max - step,
            "{name}: grid reaches the upper edge"
        );
        let floor = gaps.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(
            floor * budget as f64 + 1e-9 * span >= span,
            "{name}: gaps floor at span/budget, got {floor}"
        );
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

/// Deterministic xorshift so the property check is reproducible, not flaky.
fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Uniform `[0, 1)` from the top 53 bits (exact in `f64`).
fn unit(state: &mut u64) -> f64 {
    (xorshift(state) >> 11) as f64 / (1u64 << 53) as f64
}

#[test]
fn random_windows_keep_the_grid_invariants() {
    let mut state = 0x9E3779B97F4A7C15u64;
    for _ in 0..5_000 {
        // Off-centre, fractional windows so `range / max_ticks` is not itself
        // a nice number: the grid then sits strictly inside the budget rather
        // than aligning both endpoints.
        let scale = 10f64.powf(unit(&mut state) * 6.0 - 3.0);
        let min = (unit(&mut state) - 0.5) * scale;
        let span = (unit(&mut state) + 0.01) * scale;
        let max = min + span;
        if !min.is_finite() || !max.is_finite() || min >= max {
            continue;
        }
        let max_ticks = 1 + (xorshift(&mut state) % 64) as usize;
        let ticks = nice_ticks(min, max, max_ticks);
        let step = nice_step((max - min) / max_ticks as f64);
        if !step.is_finite() || step <= 0.0 {
            continue;
        }

        // Hard cap the implementation always honours.
        assert!(ticks.len() <= max_ticks * 2);
        // Tight budget: a window whose endpoints both land exactly on grid
        // points can hold `max_ticks + 1` points, but these off-centre
        // fractional windows never align, so the nominal budget holds here.
        assert!(
            ticks.len() <= max_ticks,
            "grid stays within budget for [{min}, {max}] budget {max_ticks}: {}",
            ticks.len()
        );
        let start = (min / step).floor() * step;
        for (i, tick) in ticks.iter().enumerate() {
            assert!(tick.value.is_finite(), "finite tick in [{min}, {max}]");
            assert!(
                tick.value >= min && tick.value <= max,
                "tick {} outside [{min}, {max}]",
                tick.value
            );
            if i > 0 {
                let prev = ticks[i - 1].value;
                assert!(
                    tick.value > prev,
                    "strictly increasing grid: {prev} then {}",
                    tick.value
                );
                let gap = tick.value - prev;
                assert!(
                    (gap - step).abs() <= 1e-9 * step.abs().max(1.0),
                    "gap {gap} must equal step {step}"
                );
            }
            // Exact closed form: every value is bit-for-bit `start + i*step`
            // (the accumulation bug left a rounding drift behind).
            let i = ((tick.value - start) / step).round();
            assert_eq!(
                tick.value.to_bits(),
                (start + i * step).to_bits(),
                "tick {} must equal start + i*step (start {start}, step {step}, i {i})",
                tick.value
            );
        }
    }
}
