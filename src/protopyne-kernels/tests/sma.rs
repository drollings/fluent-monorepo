use super::*;

fn params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

#[test]
fn batch_matches_hand_computed_values() {
    let sma = SmaIndicator::new();
    let closes = [1.0, 2.0, 3.0, 4.0, 5.0];
    let result = sma
        .batch(&Ohlcv::close_only(&closes), &params(3))
        .expect("valid");
    assert_eq!(result.spec_id, "sma");
    assert_eq!(
        result.values,
        vec![None, None, Some(2.0), Some(3.0), Some(4.0)]
    );
}

#[test]
fn invalid_periods_are_typed_errors_and_oversize_is_all_na() {
    let sma = SmaIndicator::new();
    let pair = [1.0, 2.0];
    let data = Ohlcv::close_only(&pair);
    assert!(matches!(
        sma.batch(&data, &params(0)),
        Err(KernelError::InvalidParam(_))
    ));
    assert!(matches!(
        sma.batch(&data, &params(-3)),
        Err(KernelError::InvalidParam(_))
    ));
    assert!(matches!(
        sma.batch(&data, &ParamSet::new()),
        Err(KernelError::InvalidParam(_))
    ));
    let short = sma.batch(&data, &params(5)).expect("honest na");
    assert_eq!(short.values, vec![None, None]);
    assert_eq!(sma.warmup(&params(7)).expect("warmup"), 7);
}

fn trending_closes(bars: usize) -> Vec<f64> {
    (0..bars)
        .map(|i| 100.0 + i as f64 * 0.05 + ((i * 37 % 11) as f64) * 0.13)
        .collect()
}

#[test]
fn windowed_plus_warmup_matches_full_history_over_visible_slice() {
    // Native-timeframe rule: evaluating over full-resolution bars restricted
    // to visible-window-plus-warmup reproduces the full-history output over
    // the visible slice. The warmup length is the kernel's declared warmup,
    // never a magic number — and the mean only reads its own window, so the
    // windowed run agrees bit-for-bit.
    let sma = SmaIndicator::new();
    let ps = params(14);
    let warmup = sma.warmup(&ps).expect("declared warmup");
    let closes = trending_closes(500);
    let full = sma
        .batch(&Ohlcv::close_only(&closes), &ps)
        .expect("full history");
    let visible_start: usize = 400;
    let eval_start = visible_start.saturating_sub(warmup);
    let windowed = sma
        .batch(&Ohlcv::close_only(&closes[eval_start..]), &ps)
        .expect("windowed");
    assert_eq!(full.values.len(), closes.len());
    assert_eq!(windowed.values.len(), closes.len() - eval_start);
    for (offset, want) in full.values[visible_start..].iter().enumerate() {
        assert_eq!(
            windowed.values[offset + warmup],
            *want,
            "visible bar {} matches exactly",
            visible_start + offset
        );
    }
}

#[test]
fn window_shorter_than_warmup_yields_holes_never_values() {
    // Warmup shortfall: a window too short for the declared warmup yields
    // holes, never fabricated values.
    let sma = SmaIndicator::new();
    let short = sma
        .batch(&Ohlcv::close_only(&trending_closes(5)), &params(14))
        .expect("short input runs");
    assert_eq!(short.values, vec![None, None, None, None, None]);
}
