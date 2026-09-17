//! Pipeline stage dispatch throughput: a full `PipelineOrchestrator` built
//! once (mock transcript backend, no network), then `execute` per iteration.
//! Measures the serving-path dispatch cost, not config building.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use fluent_llm::client::ChatBackend;
use fluent_wvr::prelude::*;

fn pipeline() -> fluent_router::pipeline::PipelineOrchestrator {
    let config: fluent_router::config::RouterConfig = serde_json::from_str(
        r#"{
        "pipelines": {"default": {"deterministic_prefilter": true, "classifier": true, "blacklist": "env/pii-patterns.json"}},
        "models": {"fast": {"endpoint": "http://upstream.test:8080/v1/chat/completions", "name": "fast", "intelligence": 1, "cost_input": 0.000001, "cost_output": 0.000006, "cost_cached_read": 0.0000004, "tok_s": 10, "total_timeout_ms": 5000, "idle_timeout_ms": 2000, "stream": false, "filter_thinking": false, "retry_count": 0, "retry_base_interval_s": 1}},
        "roles": {"fast": {"models": ["fast"]}},
        "classification": {"root": {
            "type": "classifier",
            "description": "bench router",
            "model": "fast",
            "children": [
                {"key": "fast", "description": "bench route", "node": {"type": "terminal", "route": "fast", "role": "fast"}}
            ]
        }},
        "default_route": "fast"
    }"#,
    )
    .expect("bench config parses");
    let backend: Arc<dyn ChatBackend> = Arc::new(
        fluent_router::testing::mock::TranscriptProvider::new(HashMap::new()),
    );
    config
        .build_named_pipeline_with_backend("default", Some(backend))
        .expect("bench pipeline builds")
}

fn request_ctx() -> WorkContext {
    let mut req = fluent_router::testing::test_request("What is the capital of France?");
    req.session_id = Some("bench-session".into());
    let mut ctx = WorkContext::default();
    ctx.set_structured("request", &req);
    ctx
}

fn bench_pipeline_dispatch(c: &mut Criterion) {
    let pipeline = pipeline();
    let ctx = request_ctx();
    c.bench_function("pipeline_dispatch", |b| {
        b.iter(|| {
            let out = pipeline.execute(&ctx).expect("bench dispatch completes");
            std::hint::black_box(out);
        });
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets = bench_pipeline_dispatch
);
criterion_main!(benches);
