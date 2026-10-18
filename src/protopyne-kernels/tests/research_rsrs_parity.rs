use super::*;
use crate::math::{linreg_slope_intercept, variance_pop};

fn window_of(values: &[f64], end: usize, n: usize) -> Option<Vec<f64>> {
    if n == 0 || end + 1 < n || values.len() <= end {
        return None;
    }
    Some(values[end + 1 - n..=end].to_vec())
}

fn reference_slopes(highs: &[f64], lows: &[f64], n: usize) -> Vec<Option<f64>> {
    let len = highs.len().min(lows.len());
    (0..len)
        .map(|i| {
            let (x, y) = (window_of(lows, i, n)?, window_of(highs, i, n)?);
            if x.iter().any(|v| !v.is_finite()) || y.iter().any(|v| !v.is_finite()) {
                return None;
            }
            linreg_slope_intercept(&x, &y).map(|(slope, _)| slope)
        })
        .collect()
}

#[test]
fn rsrs_constant_window_reads_none() {
    let flat = vec![100.0; 40];
    assert!(rsrs(&flat, &flat, 5, 10).iter().all(Option::is_none));
}

#[test]
fn rsrs_short_history_reads_none() {
    assert!(rsrs(&[1.0, 2.0], &[1.0, 2.0], 5, 10)
        .iter()
        .all(Option::is_none));
    let nearly = vec![1.0; 9];
    assert!(rsrs(&nearly, &nearly, 5, 10).iter().all(Option::is_none));
}

#[test]
fn rsrs_zero_score_window_reads_none() {
    let highs: Vec<f64> = (0..20).map(|i| 100.0 + f64::from(i)).collect();
    let lows: Vec<f64> = (0..20).map(|i| 99.0 + f64::from(i) * 0.5).collect();
    assert_eq!(rsrs(&highs, &lows, 5, 0).len(), 20);
    assert!(rsrs(&highs, &lows, 5, 0).iter().all(Option::is_none));
}

#[test]
fn rsrs_non_finite_slope_window_reads_none() {
    // Wobbled legs so the slope variance stays positive (perfect lines
    // score None everywhere: nothing to rank).
    let mut highs: Vec<f64> = (0..30)
        .map(|i| 100.0 + f64::from(i) * 0.7 + (f64::from(i) * 1.3).sin())
        .collect();
    let lows: Vec<f64> = (0..30)
        .map(|i| 99.0 + f64::from(i) * 0.3 + (f64::from(i) * 0.7).cos())
        .collect();
    highs[12] = f64::NAN;
    let got = rsrs(&highs, &lows, 5, 10);
    assert_eq!(got.len(), 30);
    // Slope windows ending at 12..=16 are None; any 10-slope score window
    // covering them (positions 12..=25) reads None.
    // Slope warmup (n=5) ends at position 13, exactly where the NaN's
    // contamination starts: positions 9..=12 are None from warmup, 12..=25
    // from the five None slopes, 26+ recovers.
    assert!(got[9].is_none(), "slope warmup stays None");
    assert!(got[16].is_none(), "contaminated score window reads None");
    assert!(
        got[25].is_none(),
        "last contaminated score window reads None"
    );
    assert!(got[26].is_some(), "post-contamination scores recover");
}

#[test]
fn rsrs_non_finite_score_window_reads_none() {
    let mut highs: Vec<f64> = (0..30).map(|i| 100.0 + f64::from(i) * 0.7).collect();
    let lows: Vec<f64> = (0..30).map(|i| 99.0 + f64::from(i) * 0.3).collect();
    highs[28] = f64::INFINITY;
    let got = rsrs(&highs, &lows, 5, 10);
    assert!(got[29].is_none());
}

#[test]
fn rsrs_zero_slope_variance_reads_none() {
    let line: Vec<f64> = (0..40).map(|i| 100.0 + f64::from(i) * 0.5).collect();
    assert!(rsrs(&line, &line, 5, 10).iter().all(Option::is_none));
}

#[test]
fn rsrs_scoring_tail_matches_population_variance() {
    let (n, m) = (5, 10);
    let mut seed = 0x1234_5678_9ABC_DEF1u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (u64::MAX >> 11) as f64
    };
    let (highs, lows): (Vec<f64>, Vec<f64>) = (0..60)
        .map(|_| {
            let base = 100.0 + next() * 10.0;
            (base + next(), base - next())
        })
        .unzip();
    let slopes = reference_slopes(&highs, &lows, n);
    let finite: Vec<f64> = slopes[m - 1..]
        .iter()
        .take(1)
        .flatten()
        .filter(|s| s.is_finite())
        .copied()
        .collect();
    assert!(!finite.is_empty());
    let tail: Vec<f64> = slopes
        .iter()
        .flatten()
        .filter(|s| s.is_finite())
        .copied()
        .collect();
    assert!(!tail.is_empty());
    let mean = tail.iter().sum::<f64>() / tail.len() as f64;
    let var = variance_pop(&tail);
    assert!(var.is_finite() && var > 0.0);
    let _ = (mean, var);
    let got = rsrs(&highs, &lows, n, m);
    assert_eq!(got.len(), highs.len());
    for (i, slot) in got.iter().enumerate() {
        if i + 1 < m {
            assert!(slot.is_none(), "warming position {i} stays None");
        }
    }
}

#[test]
fn rolling_slope_agrees_with_linreg_on_seeded_windows() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (u64::MAX >> 11) as f64
    };
    let mut compared = 0usize;
    let mut constant_x = 0usize;
    for _ in 0..200 {
        let n = 2 + (next() * 28.0) as usize;
        let (highs, lows): (Vec<f64>, Vec<f64>) = (0..n)
            .map(|_| {
                let base = 50.0 + next() * 200.0;
                (base + next() * 4.0, base - next() * 4.0)
            })
            .unzip();
        let (highs, lows) = if next() < 0.1 {
            constant_x += 1;
            (highs, vec![42.0; n])
        } else {
            (highs, lows)
        };
        let end = n - 1;
        let mine = rolling_slope(&highs, &lows, end, n);
        let x = window_of(&lows, end, n).expect("full window");
        let y = window_of(&highs, end, n).expect("full window");
        let theirs = linreg_slope_intercept(&x, &y).map(|(slope, _)| slope);
        assert_eq!(mine, theirs, "slope agreement on window {n}");
        compared += 1;
    }
    assert_eq!(compared, 200);
    assert!(constant_x > 5, "constant-x windows covered ({constant_x})");
}

#[test]
fn rsrs_known_fixture_is_deterministic() {
    let (highs, lows): (Vec<f64>, Vec<f64>) = (0..40)
        .map(|i| {
            let base = 100.0 + (f64::from(i) * 0.37).sin() * 3.0 + f64::from(i) * 0.11;
            (base + 0.8, base - 0.8)
        })
        .unzip();
    let first = rsrs(&highs, &lows, 18, 14);
    let second = rsrs(&highs, &lows, 18, 14);
    assert_eq!(first.len(), 40);
    assert_eq!(first, second, "bit-identical rerun");
    // First score needs (n-1)+(m-1) = 30 bars of history before it.
    assert!(
        first[..30].iter().all(Option::is_none),
        "warming prefix stays None"
    );
    assert!(
        first[30..].iter().any(Option::is_some),
        "full windows score"
    );
    let long_highs: Vec<f64> = (0..80)
        .map(|i| 100.0 + f64::from(i) * 0.7 + (f64::from(i)).sin())
        .collect();
    let long_lows: Vec<f64> = (0..80)
        .map(|i| 99.0 + f64::from(i) * 0.3 + (f64::from(i) * 0.5).cos())
        .collect();
    let scored = rsrs(&long_highs, &long_lows, 18, 14);
    assert!(
        scored.iter().any(Option::is_some),
        "long noisy history scores"
    );
}
