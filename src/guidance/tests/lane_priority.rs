use super::{Lane, LaneConfig, LaneError, LaneGate};
use common_core::golden::compare_series;
use std::sync::Arc;
use std::time::{Duration, Instant};

// The lane contract: interactive calls are always admitted first and keep
// their own timeout; background calls wait for spare interactive capacity
// (defer, never fail) and keep theirs. Timeouts bound the call only —
// saturation queues, it never sheds load.

fn tight_config() -> LaneConfig {
    LaneConfig {
        interactive_in_flight: 1,
        background_in_flight: 1,
        interactive_timeout_ms: 5_000,
        background_timeout_ms: 5_000,
    }
}

#[tokio::test]
async fn background_defers_while_interactive_saturated() {
    let gate = Arc::new(LaneGate::new(tight_config()));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    // Holder owns the single interactive permit before the background
    // call is even admitted: the rendezvous makes saturation certain.
    let holder = {
        let gate = Arc::clone(&gate);
        tokio::spawn(async move {
            gate.admit(Lane::Interactive, || async move {
                let _ = started_tx.send(());
                tokio::time::sleep(Duration::from_millis(100)).await;
                "interactive"
            })
            .await
        })
    };
    started_rx.await.expect("holder owns the lane");
    let defer_start = Instant::now();
    let background = gate
        .admit(Lane::Background, || async { "background" })
        .await
        .expect("background deferred, never failed");
    let deferred = defer_start.elapsed();
    let held = holder
        .await
        .expect("holder joins")
        .expect("interactive admitted");
    assert_eq!(background, "background");
    assert_eq!(held, "interactive");
    // The background call waited out the hold instead of running through it.
    assert!(
        deferred >= Duration::from_millis(80),
        "must defer until release, took {deferred:?}"
    );
}

#[tokio::test]
async fn lane_timeout_bounds_the_call_not_the_queue() {
    let gate = LaneGate::new(LaneConfig {
        interactive_timeout_ms: 20,
        ..tight_config()
    });
    let err = gate
        .admit(Lane::Interactive, || async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            "slow"
        })
        .await
        .expect_err("over-budget call times out");
    match err {
        LaneError::Timeout { lane, timeout_ms } => {
            assert_eq!(lane, Lane::Interactive);
            assert_eq!(timeout_ms, 20);
        }
        LaneError::Closed => panic!("gate closed unexpectedly"),
    }
}

#[tokio::test]
async fn zero_load_adds_no_measurable_latency() {
    // Control: with no contention the lanes must behave like direct calls.
    let gate = LaneGate::with_defaults();
    let mut lane_ms = Vec::new();
    let mut direct_ms = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        gate.admit(Lane::Interactive, || async {})
            .await
            .expect("admit");
        lane_ms.push(Some(start.elapsed().as_millis() as f64));
        let start = Instant::now();
        async {}.await;
        direct_ms.push(Some(start.elapsed().as_millis() as f64));
    }
    let report = compare_series(&lane_ms, &direct_ms, 10.0);
    assert!(
        report.matches,
        "lanes must not slow the uncontended path: {report:?}"
    );
}

#[test]
fn lane_defaults_preserve_single_server_behavior() {
    // Defaults bind nothing under normal load and match the role timeout,
    // so an unconfigured gate behaves like today's direct calls.
    let config = LaneConfig::default();
    assert!(config.interactive_in_flight >= 1);
    assert!(config.background_in_flight >= 1);
    assert_eq!(config.interactive_timeout_ms, config.background_timeout_ms);
    let gate = LaneGate::with_defaults();
    let outcome =
        common_core::runtime::block_on_shared(gate.admit(Lane::Background, || async { "direct" }));
    assert_eq!(outcome.expect("uncontended background runs"), "direct");
}
