use crate::indicator::IndicatorResult;
use crate::kernel_eval::{kernel_depends, kernel_message, kernel_provides, warmed_count};

#[test]
fn message_format() {
    assert_eq!(
        kernel_message("sma", 10, 4),
        "sma evaluated 10 bars (4 warmed)"
    );
    assert_eq!(kernel_message("x", 0, 0), "x evaluated 0 bars (0 warmed)");
}

#[test]
fn warmed_counts_mixed_windows() {
    let result = IndicatorResult::new("sma".to_owned(), vec![Some(1.0), None, Some(f64::NAN)]);
    assert_eq!(warmed_count(&result), 2);
}

#[test]
fn kernel_depends_provides_intern_zero_one_n_deps() {
    use crate::indicator::{Indicator, IndicatorSpec};
    use std::sync::Arc;
    struct Stub {
        spec: IndicatorSpec,
    }
    impl Indicator for Stub {
        fn spec(&self) -> &IndicatorSpec {
            &self.spec
        }
        fn warmup(
            &self,
            _params: &crate::indicator::ParamSet,
        ) -> Result<usize, crate::error::KernelError> {
            Ok(1)
        }
        fn batch(
            &self,
            _data: &crate::indicator::Ohlcv<'_>,
            _params: &crate::indicator::ParamSet,
        ) -> Result<IndicatorResult, crate::error::KernelError> {
            Ok(IndicatorResult::new(self.spec.id.clone(), vec![]))
        }
        fn update(
            &self,
            _state: &mut crate::indicator::IndicatorState,
            _bar: crate::indicator::BarTick,
            _params: &crate::indicator::ParamSet,
        ) -> Result<Option<f64>, crate::error::KernelError> {
            Ok(None)
        }
    }
    for deps in [vec![], vec!["b"], vec!["b", "c", "d"]] {
        let spec = IndicatorSpec::new()
            .id("u".to_owned())
            .display_name("u".to_owned())
            .warmup_bars(1_usize)
            .depends(
                deps.iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>(),
            )
            .build();
        let ind: Arc<dyn Indicator> = Arc::new(Stub { spec });
        let got: Vec<String> = kernel_depends(&ind)
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(got, deps);
        let provides: Vec<String> = kernel_provides(&ind)
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(provides, vec!["u"]);
    }
}
