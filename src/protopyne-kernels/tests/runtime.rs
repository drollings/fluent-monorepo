use super::*;
use std::sync::Arc;

fn config() -> PineRuntimeConfig {
    PineRuntimeConfig::new()
        .max_bars(100_u64)
        .warmup_bars(5_u64)
        .build()
}

fn bar(price: &str) -> protopyne_types::Bar {
    use chrono::TimeZone as _;
    protopyne_types::Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(2026, 1, 2, 9, 0, 0)
            .single()
            .expect("valid timestamp"))
        .o(price.parse().expect("decimal"))
        .h(price.parse().expect("decimal"))
        .l(price.parse().expect("decimal"))
        .c(price.parse().expect("decimal"))
        .volume(10_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn run_over_walks_every_bar_in_order() {
    let runtime = PineRuntime::new(config());
    let bars = [bar("1.0"), bar("2.0"), bar("3.0")];
    let mut seen = Vec::new();
    let evaluated = runtime
        .run_over(&bars, InputSet::new(), |ctx| {
            seen.push(ctx.cursor());
        })
        .expect("within cap");
    assert_eq!(evaluated, 3);
    assert_eq!(seen, vec![0, 1, 2]);
}

#[test]
fn run_over_rejects_input_beyond_cap() {
    let runtime = PineRuntime::new(
        PineRuntimeConfig::new()
            .max_bars(2_u64)
            .warmup_bars(0_u64)
            .build(),
    );
    let bars = [bar("1.0"), bar("2.0"), bar("3.0")];
    let err = runtime
        .run_over(&bars, InputSet::new(), |_| {})
        .expect_err("must refuse");
    assert!(matches!(err, KernelError::TooManyBars { .. }));
}

#[test]
fn execute_validates_config_fail_closed() {
    let ok = PineRuntime::new(config());
    let output = ok
        .execute(&WorkContext::default())
        .expect("valid config executes");
    assert!(output.success);

    let bad = PineRuntime::new(
        PineRuntimeConfig::new()
            .max_bars(2_u64)
            .warmup_bars(9_u64)
            .build(),
    );
    assert!(matches!(
        bad.execute(&WorkContext::default()),
        Err(WorkError::Execution(_))
    ));
}

#[test]
fn set_field_at_registration_hot_fields_direct() {
    let mut runtime = PineRuntime::new(config());
    runtime.set_field("max_bars", "50").expect("known field");
    assert_eq!(runtime.config().max_bars, 50);
    assert_eq!(runtime.get_field("max_bars").expect("read back"), "50");
    assert!(runtime.set_field("nope", "1").is_err());
    // Hot path reads the field directly, never through `get_field`.
    assert_eq!(runtime.config().warmup_bars, 5);
}

#[test]
fn instrumented_wrapper_before_type_erasure() {
    let wrapped = Instrumented::new(PineRuntime::new(config()), "pine_runtime");
    let shared: Arc<dyn Component> = Arc::new(wrapped);
    assert_eq!(shared.name(), "pine_runtime");
    let output = shared
        .execute(&WorkContext::default())
        .expect("executes through wrapper");
    assert!(output.success);
}
