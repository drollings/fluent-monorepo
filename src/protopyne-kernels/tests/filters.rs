use super::*;
use crate::filters::{EitIndicator, KalmanIndicator, VossIndicator};

fn approx(got: &[Option<f64>], want: &[Option<f64>], eps: f64) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want.iter()) {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!((g - w).abs() <= eps, "{g} vs {w}"),
            _ => panic!("na mismatch: {g:?} vs {w:?}"),
        }
    }
}

fn closes(data: &[f64]) -> Ohlcv<'_> {
    Ohlcv::close_only(data)
}

fn fold(kernel: &impl Indicator, data: &[f64], params: &ParamSet) -> Vec<Option<f64>> {
    let mut state = IndicatorState::new();
    data.iter()
        .map(|&c| {
            kernel
                .update(&mut state, BarTick::close_only(c), params)
                .expect("valid update")
        })
        .collect()
}

#[test]
fn eit_unit_alpha_matches_hand_steps() {
    // alpha = 1 collapses the recurrence to 0.75*src + 0.5*s1 - 0.25*s2:
    // [4,8,12,16] -> [4, 6, 12, 16] (init terms vanish: 1 - alpha = 0).
    let eit = EitIndicator::new();
    let ps = ParamSet::new().with_float("alpha", 1.0);
    let data = [4.0, 8.0, 12.0, 16.0];
    let result = eit.batch(&closes(&data), &ps).expect("valid");
    approx(&result.values, &[Some(4.0), Some(6.0), Some(12.0), Some(16.0)], 1e-9);
    approx(&fold(&eit, &data, &ps), &result.values, 1e-12);
}

#[test]
fn eit_agrees_fold_on_ramps() {
    let eit = EitIndicator::new();
    let ps = ParamSet::new().with_float("alpha", 0.5);
    let data: Vec<f64> = (1..=40).map(f64::from).collect();
    let result = eit.batch(&closes(&data), &ps).expect("valid");
    assert_eq!(result.values.len(), 40);
    approx(&fold(&eit, &data, &ps), &result.values, 1e-9);
}

#[test]
fn kalman_matches_hand_steps() {
    // period 1, alpha 1, beta 0: v3 = 1; bar0 seeds v1 = 10, v4 = 1/2,
    // v2 = 1/2; bar1: v4 = (1/2)/(3/2) = 1/3, v1 = 10 + 10/3 = 40/3.
    let kalman = KalmanIndicator::new();
    let ps = ParamSet::new()
        .with_int("length", 1)
        .with_float("alpha", 1.0)
        .with_float("beta", 0.0);
    let data = [10.0, 20.0, 30.0];
    let result = kalman.batch(&closes(&data), &ps).expect("valid");
    assert_eq!(result.values[0], Some(10.0));
    assert!((result.values[1].expect("value") - 40.0 / 3.0).abs() <= 1e-9);
    // bar2: v4 = (1/3)/(4/3) = 1/4, v1 = 40/3 + (30 - 40/3)/4 = 17.5.
    assert!((result.values[2].expect("value") - 17.5).abs() <= 1e-9);
    approx(&fold(&kalman, &data, &ps), &result.values, 1e-12);
}

#[test]
fn kalman_agrees_fold_with_defaults() {
    let kalman = KalmanIndicator::new();
    let ps = ParamSet::new().with_int("length", 14);
    let data: Vec<f64> = (1..=40).map(|i| 100.0 + f64::from(i).sin()).collect();
    let result = kalman.batch(&closes(&data), &ps).expect("valid");
    approx(&fold(&kalman, &data, &ps), &result.values, 1e-9);
}

#[test]
fn voss_gate_holds_then_computes() {
    // predict 1 -> order 3: bars 0..=3 emit (0, 0); bar 4 computes.
    // period 4, band 0.25: alpha = cos(pi/2) ~ 0, s3 = 1 - delta with
    // delta = 1/g - sqrt(1/g^2 - 1), g = cos(pi/8).
    let voss = VossIndicator::new();
    let ps = ParamSet::new()
        .with_int("length", 4)
        .with_int("predict", 1)
        .with_float("band", 0.25);
    let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let result = voss.batch(&closes(&data), &ps).expect("valid");
    assert_eq!(&result.values[..4], &[Some(0.0); 4]);
    let gamma = (0.25 * std::f64::consts::TAU / 4.0).cos();
    let delta = 1.0 / gamma - (1.0 / (gamma * gamma) - 1.0).sqrt();
    let s3 = 1.0 - delta;
    // filter_1/filter_2 and all prior voss are 0, so filter = s3/2 * (5-3)
    // and voss = 3 * filter.
    let filter = s3 / 2.0 * 2.0;
    approx(&result.values[4..5], &[Some(3.0 * filter)], 1e-9);
    let filter_leg = result.secondary("filter").expect("filter secondary");
    approx(&filter_leg[4..5], &[Some(filter)], 1e-9);
    approx(&fold(&voss, &data, &ps), &result.values, 1e-9);
    // Recursive with a defined-zero gate: the opening print is defined.
    assert_eq!(voss.warmup(&ps).expect("valid"), 1);
}

fn bits_of(values: &[Option<f64>]) -> Vec<Option<u64>> {
    values.iter().map(|v| v.map(f64::to_bits)).collect()
}

#[test]
fn buffered_update_matches_batch_on_constant_and_subnormal_inputs() {
    // Bit-exactness pin for the scratch migration, over the back-indexed
    // representative: these kernels read lookbacks, never whole windows, so
    // the pin proves the scalar reads carry exactly what the collects did.
    let eit = EitIndicator::new();
    let params = ParamSet::new().with_float("alpha", 0.5);
    for data in [
        vec![42.0; 40],
        (1..=40).map(|i| f64::from(i) * 1e-310).collect::<Vec<_>>(),
    ] {
        let batch = eit.batch(&closes(&data), &params).expect("valid").values;
        assert_eq!(bits_of(&fold(&eit, &data, &params)), bits_of(&batch));
    }
}
