use super::*;

fn params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

#[test]
fn batch_matches_hand_computed_values() {
    // k = 2/3. Seed = mean(10, 20) = 15; each step: close*k + prev*(1-k).
    let ema = EmaIndicator::new();
    let closes = [10.0, 20.0, 30.0, 40.0, 50.0];
    let result = ema
        .batch(&Ohlcv::close_only(&closes), &params(2))
        .expect("valid");
    assert_eq!(result.spec_id, "ema");
    let expected = [None, Some(15.0), Some(25.0), Some(35.0), Some(45.0)];
    assert_eq!(result.values.len(), expected.len());
    for (got, want) in result.values.iter().zip(expected.iter()) {
        match (got, want) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!((g - w).abs() < 1e-9, "{g} vs {w}"),
            _ => panic!("na mismatch: {got:?} vs {want:?}"),
        }
    }
}

#[test]
fn invalid_periods_are_typed_errors_and_oversize_is_all_na() {
    let ema = EmaIndicator::new();
    let pair = [1.0, 2.0];
    let data = Ohlcv::close_only(&pair);
    assert!(matches!(
        ema.batch(&data, &params(0)),
        Err(KernelError::InvalidParam(_))
    ));
    let short = ema.batch(&data, &params(5)).expect("honest na");
    assert_eq!(short.values, vec![None, None]);
    assert_eq!(ema.warmup(&params(7)).expect("warmup"), 7);
}

fn trending_closes(bars: usize) -> Vec<f64> {
    (0..bars)
        .map(|i| 100.0 + i as f64 * 0.05 + ((i * 37 % 11) as f64) * 0.13)
        .collect()
}

fn max_abs_diff(a: &[Option<f64>], b: &[Option<f64>]) -> f64 {
    a.iter()
        .zip(b.iter())
        .map(|pair| match pair {
            (Some(x), Some(y)) => (x - y).abs(),
            (None, None) => 0.0,
            _ => f64::INFINITY,
        })
        .fold(0.0, f64::max)
}

#[test]
fn windowed_plus_warmup_converges_to_full_history_over_visible_slice() {
    // Recursive kernels seed from their input window's mean, so a windowed
    // run opens with a seeding transient the full-history run does not have:
    // exact equality holds only for finite-memory kernels (see the SMA pin).
    // What the declared warmup buys a recursive kernel is convergence — the
    // transient decays geometrically with distance from the window start, so
    // more warmup bars means strictly smaller deviation, largest at the
    // window's first visible bar. Restructuring ownership to seed from true
    // history state would change evaluation architecture, so the terminal
    // keeps full-history evaluation (today's exact values, no regression)
    // and this test pins the convergence the warmup actually provides.
    let ema = EmaIndicator::new();
    let ps = params(14);
    let warmup = ema.warmup(&ps).expect("declared warmup");
    let closes = trending_closes(500);
    let full = ema
        .batch(&Ohlcv::close_only(&closes), &ps)
        .expect("full history");
    let visible_start: usize = 400;
    let windowed_at = |extra: usize| {
        let eval_start = visible_start.saturating_sub(warmup + extra);
        ema.batch(&Ohlcv::close_only(&closes[eval_start..]), &ps)
            .expect("windowed")
    };
    let one = windowed_at(0);
    let four = windowed_at(3 * warmup);
    let end = closes.len() - visible_start;
    let diff_one = max_abs_diff(&one.values[warmup..][..end], &full.values[visible_start..]);
    let diff_four = max_abs_diff(
        &four.values[3 * warmup + warmup..][..end],
        &full.values[visible_start..],
    );
    assert!(
        diff_one <= 0.05,
        "declared-warmup transient stays small on the fixture: {diff_one}"
    );
    assert!(
        diff_four * 8.0 <= diff_one,
        "tripling the warmup shrinks the transient geometrically: {diff_four} vs {diff_one}"
    );
    // Largest at the first visible bar: the transient decays with distance
    // from the window start, never grows mid-window.
    let first = (one.values[warmup].unwrap() - full.values[visible_start].unwrap()).abs();
    assert!(
        (first - diff_one).abs() <= 1e-12,
        "the transient peaks at the window head: {first} vs max {diff_one}"
    );
}
