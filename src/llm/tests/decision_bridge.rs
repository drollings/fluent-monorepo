//! Detached shadow bridge tests (tests-first).

use std::sync::Arc;
use std::time::Duration;

use fluent_llm::decision::{ReplayItem, ShadowBridge, ShadowJob};

fn job(id: &str, work: impl FnOnce() -> JobFut + Send + 'static) -> ShadowJob {
    ShadowJob::new(id.to_string(), "q".to_string(), 1, work)
}

type JobFut = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

fn ok_slow(ms: u64) -> JobFut {
    Box::pin(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_queue_drops_with_visible_counter() {
    let mut bridge = ShadowBridge::new(1, 5_000);
    assert!(bridge.submit(job("a", || ok_slow(300))));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(bridge.submit(job("b", || ok_slow(10))));
    assert!(!bridge.submit(job("c", || ok_slow(10))), "full queue drops");
    assert_eq!(bridge.dropped_count(), 1);
    assert!(bridge.shutdown(5_000).await);
    assert_eq!(bridge.completed_count(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeouts_errors_panics_never_touch_caller() {
    let mut bridge = ShadowBridge::new(8, 50);
    assert!(bridge.submit(job("slow", || ok_slow(500))));
    assert!(bridge.submit(job("err", || Box::pin(async { Err("bad".to_string()) }))));
    assert!(bridge.submit(job("boom", || Box::pin(async { panic!("shadow panic") }))));
    assert!(bridge.submit(job("fine", || Box::pin(async { Ok(()) }))));
    assert!(bridge.shutdown(5_000).await);
    assert_eq!(bridge.timeout_count(), 1);
    assert_eq!(bridge.error_count(), 1);
    assert_eq!(bridge.panic_count(), 1);
    assert_eq!(bridge.completed_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_is_bounded() {
    let mut bridge = ShadowBridge::new(8, 10_000);
    assert!(bridge.submit(job("long", || ok_slow(2_000))));
    assert!(!bridge.shutdown(50).await, "bounded flush gives up");
}

#[test]
fn submit_is_sync_callable() {
    // `submit`/`shutdown_blocking` never block the caller: safe from sync
    // contexts while the driver lives on a running runtime.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let mut bridge = runtime.block_on(async { ShadowBridge::new(8, 1_000) });
    assert!(bridge.submit(job("x", || Box::pin(async { Ok(()) }))));
    assert!(bridge.shutdown_blocking(5_000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_never_logs_state() {
    use fluent_llm::decision::{replay_shadow_log, Decision, ScriptedDecideBackend};
    let backend = Arc::new(ScriptedDecideBackend::canned(
        Decision::from_distribution(
            "Yes",
            vec![("Yes".to_string(), 0.9), ("No".to_string(), 0.1)],
            "stub".to_string(),
            1,
            0,
            false,
        )
        .expect("canned"),
    ));
    let mut client = fluent_llm::client::LlmClient::new("http://127.0.0.1:9", "stub");
    client.decide_backend = Some(backend);
    let spec = fluent_llm::decision::QuestionSpec::new()
        .id("suff".to_string())
        .version(1)
        .kind("noul".to_string())
        .prompt_template("Suffices?".to_string())
        .options_csv("Yes,No".to_string())
        .act_threshold(0.9)
        .review_threshold(0.7)
        .timeout_ms(1000)
        .cacheable(false)
        .build();
    let items = vec![
        ReplayItem {
            point: "sufficiency".to_string(),
            question_id: "q1".to_string(),
            spec: spec.clone(),
            question: "enough?".to_string(),
            expected_label: Some("Yes".to_string()),
        },
        ReplayItem {
            point: "sufficiency".to_string(),
            question_id: "missing".to_string(),
            spec,
            question: "enough?".to_string(),
            expected_label: None,
        },
    ];
    let outcomes = replay_shadow_log(&client, items, &|id| {
        (id == "q1").then(|| "SECRET-STATE-BYTES".to_string())
    })
    .await;
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].matched, Some(true));
    assert_eq!(outcomes[1].matched, None);
    let encoded = format!("{outcomes:?}");
    assert!(!encoded.contains("SECRET-STATE-BYTES"));
}
