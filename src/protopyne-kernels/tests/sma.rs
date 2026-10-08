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
fn batch_agrees_with_incremental_update_fold() {
    let sma = SmaIndicator::new();
    let closes = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
    let ps = params(4);
    let batch = sma
        .batch(&Ohlcv::close_only(&closes), &ps)
        .expect("valid");
    let mut state = IndicatorState::new();
    let mut folded = Vec::new();
    for &close in &closes {
        folded.push(
            sma.update(&mut state, BarTick::close_only(close), &ps)
                .expect("valid"),
        );
    }
    assert_eq!(batch.values, folded);
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
