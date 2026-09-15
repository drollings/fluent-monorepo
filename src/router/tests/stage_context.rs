use super::*;
use std::sync::Mutex;

struct StubStage {
    name: &'static str,
}

impl WorkUnit for StubStage {
    fn name(&self) -> &str {
        self.name
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        unreachable!("stage_context tests drive stages through producers")
    }
}

impl_fieldless!(StubStage);

impl Describable for StubStage {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}

impl_component!(StubStage);

fn stub(name: &'static str) -> Arc<dyn Component> {
    Arc::new(StubStage { name })
}

fn passed(stage: PipelineStage, reason: &str) -> (StageDecision, Option<RoutingTarget>) {
    (StageDecision::new(stage, StageVerdict::Passed, reason), None)
}

fn orchestrator_with(
    names: &[&'static str],
    producers: Vec<Option<StageProducer>>,
) -> PipelineOrchestrator {
    let stages: Vec<Arc<dyn Component>> = names.iter().map(|n| stub(n)).collect();
    PipelineOrchestrator::with_producers(stages, producers)
}

#[test]
fn stage_sees_request_and_caller_context_untouched() {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_producer = Arc::clone(&seen);
    let producer: StageProducer = Arc::new(move |ctx, _prior| {
        let req = ctx
            .structured_ref("request")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        seen_producer
            .lock()
            .expect("observation lock")
            .push(req);
        Ok(passed(PipelineStage::DeterministicPreFilter, "ok"))
    });
    let orch = orchestrator_with(&["s"], vec![Some(producer)]);

    let mut caller = WorkContext::default();
    caller.set_structured("request", &serde_json::json!({"q": "hello"}));
    let before = caller.structured.get("request").cloned();

    let out = orch.execute(&caller).expect("pipeline runs");
    let result: PipelineResult = out.data_as().expect("typed result");
    assert_eq!(result.decisions.len(), 1);

    let observations = seen.lock().expect("observation lock");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0], serde_json::json!({"q": "hello"}));
    assert_eq!(
        caller.structured.get("request").cloned(),
        before,
        "the caller's context is never mutated by stage execution"
    );
}

#[test]
fn rerouted_request_reaches_later_stages() {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_second = Arc::clone(&seen);
    let first: StageProducer = Arc::new(|_ctx, _prior| {
        let decision = StageDecision::new(
            PipelineStage::DeterministicPreFilter,
            StageVerdict::Rerouted,
            "rewrite",
        )
        .with_metadata(serde_json::json!({"rewritten_request": "{\"q\":\"two\"}"}));
        Ok((decision, None))
    });
    let second: StageProducer = Arc::new(move |ctx, prior| {
        assert_eq!(prior.len(), 1, "later stages observe earlier decisions");
        seen_second
            .lock()
            .expect("observation lock")
            .push(ctx.structured_ref("request").cloned().unwrap_or_default());
        Ok(passed(PipelineStage::Classifier, "ok"))
    });
    let orch = orchestrator_with(&["a", "b"], vec![Some(first), Some(second)]);

    let mut caller = WorkContext::default();
    caller.set_structured("request", &serde_json::json!({"q": "one"}));
    let out = orch.execute(&caller).expect("pipeline runs");
    let result: PipelineResult = out.data_as().expect("typed result");
    assert_eq!(result.decisions.len(), 2);

    let observations = seen.lock().expect("observation lock");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0], serde_json::json!({"q": "two"}));
}

#[test]
fn metadata_borrowed_reader_matches_owned() {
    let value = serde_json::json!({
        "response": "done",
        "fallback": true,
        "rewritten_request": "r",
    });
    let owned = StageMetadata::from(value.clone());
    let borrowed = StageMetadata::metadata_ref(&value);
    assert_eq!(borrowed.response(), owned.response());
    assert_eq!(borrowed.fallback(), owned.fallback());
    assert_eq!(borrowed.rewritten_request(), owned.rewritten_request());
    assert_eq!(borrowed.response(), Some("done"));
    assert_eq!(borrowed.fallback(), Some(true));
    assert!(StageMetadata::metadata_ref(&serde_json::Value::Null).response().is_none());
}
