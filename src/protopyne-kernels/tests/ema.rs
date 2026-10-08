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
fn batch_agrees_with_incremental_update_fold() {
    let ema = EmaIndicator::new();
    let closes = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
    let ps = params(4);
    let batch = ema
        .batch(&Ohlcv::close_only(&closes), &ps)
        .expect("valid");
    let mut state = IndicatorState::new();
    let mut folded = Vec::new();
    for &close in &closes {
        folded.push(
            ema.update(&mut state, BarTick::close_only(close), &ps)
                .expect("valid"),
        );
    }
    assert_eq!(batch.values, folded);
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
