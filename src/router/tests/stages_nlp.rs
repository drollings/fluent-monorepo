use super::*;
use crate::config::builder::NlpDeps;
use crate::config::RouterConfig;
use crate::pipeline::PipelineResult;
use fluent_concept::ConceptStore;
use fluent_concept::InMemoryConceptStore;
use fluent_types::openai_wire::RouterRequest;

/// A context carrying a user message (the `structured["request"]` handoff
/// the stage reads).
fn ctx_for(text: &str) -> WorkContext {
    let request = RouterRequest {
        model: "local".into(),
        messages: vec![fluent_types::openai_wire::RouterMessage {
            role: "user".into(),
            content: fluent_types::openai_wire::RouterMessageContent::Text(text.into()),
            tool_calls: None,
            tool_call_id: None,
        }],
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        session_id: None,
        agent_id: None,
        adapter: None,
        instance: None,
        snapshot: None,
        id_slot: None,
        metadata: Default::default(),
    };
    let mut ctx = WorkContext::default();
    ctx.set_structured("request", &request);
    ctx
}

fn en_stage() -> NlpStage {
    NlpStage::new(
        Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline")),
        None,
        None,
    )
}

#[test]
fn parses_and_publishes_signals() {
    let stage = en_stage();
    let (message, decision) = stage.decide(&ctx_for("show me the report"));
    assert_eq!(message, "parsed");
    assert_eq!(decision.stage, PipelineStage::Nlp);
    assert_eq!(decision.verdict, StageVerdict::Passed);
    let signals = StageMetadata::from(decision.metadata)
        .nlp_parse()
        .expect("nlp_parse");
    assert_eq!(signals.len(), 1);
    // Deterministic parser (middle rung): "show" is the verb predicate and
    // "report" (the theme, UD's direct object — the recipient "me" is iobj)
    // is its direct object.
    assert_eq!(signals[0].predicate, "show");
    assert_eq!(signals[0].direct_object.as_deref(), Some("report"));
}

#[test]
fn publishes_interlingua_and_confidence_accessors() {
    // A pipeline wired to a resolver stamps interlingua ids (the boot
    // composition, 13.5); the default `en_stage` has none.
    let vocab = std::sync::Arc::new(spacy_rs::vocab::Vocab::new(
        spacy_rs::lang::en::lexicon_config(),
    ));
    let tokenizer = spacy_rs::lang::en::tokenizer(vocab.clone()).expect("tokenizer");
    let store = std::sync::Arc::new(fluent_concept::InMemoryConceptStore::new());

    let resolver = std::sync::Arc::new(spacy_rs::interlingua::InterlinguaResolver::new(
        std::sync::Arc::clone(&store) as std::sync::Arc<dyn fluent_concept::ConceptStore>,
        std::sync::Arc::clone(vocab.strings()),
    ));
    let pipeline = std::sync::Arc::new(
        spacy_rs::NlpPipeline::new_with_resolver(
            vocab,
            tokenizer,
            spacy_rs::AnnotationValidator::new(),
            Some(resolver),
        )
        .expect("pipeline"),
    );
    let stage = NlpStage::new(pipeline, None, None);
    let (_, decision) = stage.decide(&ctx_for("show me the report"));
    let meta = StageMetadata::from(decision.metadata);
    // The C1 handoff (13.6): per-sentence interlingua frames + summary.
    let interlingua = meta.nlp_interlingua().expect("nlp_interlingua");
    assert_eq!(interlingua.len(), 1);
    assert!(
        interlingua[0].predicate_id.is_some(),
        "predicate id stamped"
    );
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert_eq!(conf.source, spacy_rs::AnnotationSource::ArcEager);
    assert!((0.0..=1.0).contains(&conf.overall));
}

#[test]
fn empty_text_is_skipped() {
    let stage = en_stage();
    let (_, decision) = stage.decide(&ctx_for(""));
    assert_eq!(decision.verdict, StageVerdict::Skipped);
}

#[test]
fn llm_rung_wins_when_fetch_returns_full_parse() {
    let full = r#"[
        {"text":"Show","pos":"verb","dep":"root","head":0,"lemma":"show"},
        {"text":"me","pos":"pron","dep":"iobj","head":-1,"lemma":"me"},
        {"text":"the","pos":"det","dep":"det","head":2,"lemma":"the"},
        {"text":"sales","pos":"noun","dep":"compound","head":1,"lemma":"sales"},
        {"text":"report","pos":"noun","dep":"dobj","head":-4,"lemma":"report"}
    ]"#;
    let fetch: spacy_rs::pipeline::LlmFetchSync =
        Arc::new(move |_tokens: Vec<String>| Ok(full.to_string()));
    let stage = NlpStage::new(
        Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline")),
        Some(fetch),
        None,
    )
    .with_refine_policy(spacy_rs::RefinePolicy {
        mode: spacy_rs::RefineMode::Always,
        ..spacy_rs::RefinePolicy::default()
    });
    let (_, decision) = stage.decide(&ctx_for("Show me the sales report"));
    assert_eq!(decision.verdict, StageVerdict::Passed);
    let signals = StageMetadata::from(decision.metadata)
        .nlp_parse()
        .expect("nlp_parse");
    assert_eq!(signals[0].predicate, "show");
    assert_eq!(signals[0].direct_object.as_deref(), Some("report"));
}

#[test]
fn llm_fetch_failure_falls_back_to_star_parse() {
    let fetch: spacy_rs::pipeline::LlmFetchSync = Arc::new(move |_tokens: Vec<String>| {
        Err(spacy_rs::pipeline::AnnotateError::Fetch("boom".into()))
    });
    let stage = NlpStage::new(
        Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline")),
        Some(fetch),
        None,
    );
    let (_, decision) = stage.decide(&ctx_for("show me the report"));
    assert_eq!(
        decision.verdict,
        StageVerdict::Passed,
        "parser rung answers"
    );
    let signals = StageMetadata::from(decision.metadata)
        .nlp_parse()
        .expect("nlp_parse");
    assert_eq!(signals[0].predicate, "show");
    assert_eq!(signals[0].direct_object.as_deref(), Some("report"));
}

#[test]
fn execute_produces_typed_decision() {
    let stage = en_stage();
    let ctx = ctx_for("show me the report");
    let output = stage.execute(&ctx).expect("execute");
    let decision: StageDecision = output.data_take().expect("typed decision");
    assert_eq!(decision.stage, PipelineStage::Nlp);
}

#[test]
fn encoder_field_reflected_in_describe() {
    let stage = en_stage();
    let desc = stage.describe();
    assert_eq!(desc["encoder_rung"], false, "no encoder → false");

    // A stage with a stub encoder closure (returns error → falls back).
    let encoder: spacy_rs::pipeline::EncoderFetchSync =
        Arc::new(|_doc| Err(spacy_rs::pipeline::AnnotateError::Fetch("stub".into())));
    let stage = NlpStage::new(
        Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline")),
        None,
        Some(encoder),
    );
    let desc = stage.describe();
    assert_eq!(desc["encoder_rung"], true, "encoder present → true");
}

#[test]
fn encoder_failure_falls_back_to_arc_eager() {
    let encoder: spacy_rs::pipeline::EncoderFetchSync = Arc::new(|_doc| {
        Err(spacy_rs::pipeline::AnnotateError::Fetch(
            "heads not trained".into(),
        ))
    });
    let stage = NlpStage::new(
        Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline")),
        None,
        Some(encoder),
    );
    let (_, decision) = stage.decide(&ctx_for("show me the report"));
    assert_eq!(
        decision.verdict,
        StageVerdict::Passed,
        "falls back to ArcEager"
    );
    let meta = StageMetadata::from(decision.metadata);
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert_eq!(
        conf.source,
        spacy_rs::AnnotationSource::ArcEager,
        "encoder failure → ArcEager provenance"
    );
}

#[test]
fn encoder_rung_wins_with_encoder_provenance() {
    // A stub encoder closure that produces a valid star parse (last token
    // ROOT, every other token attaches to it) aligned to the doc's orth by
    // construction — the shape the real adapter produces. It must WIN the
    // ladder and report `AnnotationSource::Encoder`.
    let encoder: spacy_rs::pipeline::EncoderFetchSync = Arc::new(|doc: &spacy_rs::Doc| {
        let n = doc.len();
        if n == 0 {
            return Err(spacy_rs::pipeline::AnnotateError::Encoder(
                "empty doc".into(),
            ));
        }
        let root = n - 1;
        let mut records = Vec::with_capacity(n);
        for i in 0..n {
            let text = doc.token_text(i);
            let (pos, dep, head) = if i == root {
                ("verb".to_string(), "root".to_string(), 0)
            } else {
                (
                    "noun".to_string(),
                    "nsubj".to_string(),
                    (root as i32) - (i as i32),
                )
            };
            records.push(spacy_rs::AnnotationRecord {
                text,
                pos,
                dep,
                head,
                tag: String::new(),
                lemma: String::new(),
                morph: String::new(),
                ent_iob: String::new(),
                ent_type: String::new(),
            });
        }
        Ok(spacy_rs::AnnotationSet(records))
    });
    let stage = NlpStage::new(
        Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline")),
        None,
        Some(encoder),
    )
    .with_refine_policy(spacy_rs::RefinePolicy {
        mode: spacy_rs::RefineMode::Always,
        ..spacy_rs::RefinePolicy::default()
    });
    let (_, decision) = stage.decide(&ctx_for("show me the report"));
    assert_eq!(decision.verdict, StageVerdict::Passed, "encoder rung wins");
    let meta = StageMetadata::from(decision.metadata);
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert_eq!(
        conf.source,
        spacy_rs::AnnotationSource::Encoder,
        "encoder rung must report Encoder provenance"
    );
}

#[test]
fn encoder_plus_fetch_both_provided_ladder_tries_in_order() {
    let full = r#"[
        {"text":"Show","pos":"verb","dep":"root","head":0,"lemma":"show"},
        {"text":"me","pos":"pron","dep":"iobj","head":-1,"lemma":"me"},
        {"text":"the","pos":"det","dep":"det","head":2,"lemma":"the"},
        {"text":"sales","pos":"noun","dep":"compound","head":1,"lemma":"sales"},
        {"text":"report","pos":"noun","dep":"dobj","head":-4,"lemma":"report"}
    ]"#;
    let fetch: spacy_rs::pipeline::LlmFetchSync =
        Arc::new(move |_tokens: Vec<String>| Ok(full.to_string()));
    // Encoder stub that would fail — but LLM wins first.
    let encoder: spacy_rs::pipeline::EncoderFetchSync =
        Arc::new(|_doc| Err(spacy_rs::pipeline::AnnotateError::Fetch("stub".into())));
    let stage = NlpStage::new(
        Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline")),
        Some(fetch),
        Some(encoder),
    )
    .with_refine_policy(spacy_rs::RefinePolicy {
        mode: spacy_rs::RefineMode::Always,
        ..spacy_rs::RefinePolicy::default()
    });
    let (_, decision) = stage.decide(&ctx_for("Show me the sales report"));
    assert_eq!(decision.verdict, StageVerdict::Passed);
    let meta = StageMetadata::from(decision.metadata);
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert_eq!(
        conf.source,
        spacy_rs::AnnotationSource::Llm,
        "LLM wins over failing encoder"
    );
}

/// A pipeline built *through the builder* with
/// a concept store (fail-open default absent) stamps real interlingua ids —
/// the boot-wiring guarantee, exercised end-to-end. Previously the resolver
/// was constructed only in direct `NlpStage` tests (G1).
#[test]
fn builder_built_pipeline_publishes_interlingua_ids() {
    let config: RouterConfig = serde_json::from_str(
        r#"{
            "pipelines": {
                "default": {"deterministic_prefilter": false, "nlp": true, "classifier": false}
            }
        }"#,
    )
    .expect("config");
    let store: Arc<dyn ConceptStore> = Arc::new(InMemoryConceptStore::new());
    let nlp_deps = NlpDeps {
        concept_store: Some(store),
        strings_path: None,
    };
    let map = config.build_all_pipelines_with_backend_onnx_and_nlp(None, None, &nlp_deps);
    let pipeline = map
        .get("default")
        .expect("pipeline built through the builder");
    let output = pipeline
        .execute(&ctx_for("show me the report"))
        .expect("execute");
    let result: PipelineResult = output.data_take().expect("pipeline result");
    let nlp_decision = result
        .decisions
        .iter()
        .find(|d| d.stage == PipelineStage::Nlp)
        .expect("nlp stage ran");
    assert_eq!(nlp_decision.verdict, StageVerdict::Passed);
    let meta = StageMetadata::from(nlp_decision.metadata.clone());
    let interlingua = meta.nlp_interlingua().expect("nlp_interlingua");
    assert_eq!(interlingua.len(), 1);
    assert!(
        interlingua[0].predicate_id.is_some(),
        "predicate id stamped by the resolver (end-to-end)"
    );
    assert!(
        interlingua[0].direct_object_id.is_some(),
        "direct object id stamped by the resolver (end-to-end)"
    );
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert_eq!(conf.source, spacy_rs::AnnotationSource::ArcEager);
}

/// With a concept store absent the builder
/// fails open (a `warn!`) and the pipeline still builds — the NlpStage
/// publishes a parse, but no interlingua ids (resolver `None`).
#[test]
fn builder_built_pipeline_fails_open_without_concept_store() {
    let config: RouterConfig = serde_json::from_str(
        r#"{
            "pipelines": {
                "default": {"deterministic_prefilter": false, "nlp": true, "classifier": false}
            }
        }"#,
    )
    .expect("config");
    let map = config.build_all_pipelines_with_backend_onnx_and_nlp(None, None, &NlpDeps::default());
    let pipeline = map.get("default").expect("pipeline built");
    let output = pipeline
        .execute(&ctx_for("show me the report"))
        .expect("execute");
    let result: PipelineResult = output.data_take().expect("pipeline result");
    let nlp_decision = result
        .decisions
        .iter()
        .find(|d| d.stage == PipelineStage::Nlp)
        .expect("nlp stage ran");
    let meta = StageMetadata::from(nlp_decision.metadata.clone());
    let interlingua = meta.nlp_interlingua().expect("nlp_interlingua");
    assert_eq!(interlingua.len(), 1);
    assert!(
        interlingua[0].predicate_id.is_none(),
        "no resolver ⇒ no interlingua ids (fail-open)"
    );
}

/// The confidence summary reports the resolver's
/// surfaced collision count, not a hardcoded zero. A collision is forced by
/// registering a second canonical under a taken id.
#[test]
fn confidence_summary_reports_collision_count() {
    let store = Arc::new(InMemoryConceptStore::new());
    // Prime a canonical for a lemma id so resolving a *different* canonical
    // with the same content hash collides. Use an id the resolver will hit.
    // (The InMemoryConceptStore is content-addressed; inserting a generic
    // YagoClass concept that shares the lemma's hash triggers a collision.)
    let lemma = "show";
    let id = fluent_types::lemma_id_for_str(lemma);
    store
        .insert(fluent_types::ConceptMetadata {
            id,
            canonical_name: "colliding-canonical".into(),
            namespace: fluent_types::InterlinguaNamespace::SpacyLemma,
            yago_iri: None,
            yago_class_iri: None,
            label: None,
            node_id: None,
            parent_class_id: None,
        })
        .expect("insert colliding canonical");
    let store_arc: Arc<dyn ConceptStore> = store;
    let nlp_deps = NlpDeps {
        concept_store: Some(store_arc),
        strings_path: None,
    };
    let config: RouterConfig = serde_json::from_str(
        r#"{
            "pipelines": {
                "default": {"deterministic_prefilter": false, "nlp": true, "classifier": false}
            }
        }"#,
    )
    .expect("config");
    let map = config.build_all_pipelines_with_backend_onnx_and_nlp(None, None, &nlp_deps);
    let pipeline = map.get("default").expect("pipeline built");
    let output = pipeline
        .execute(&ctx_for("show me the report"))
        .expect("execute");
    let result: PipelineResult = output.data_take().expect("pipeline result");
    let nlp_decision = result
        .decisions
        .iter()
        .find(|d| d.stage == PipelineStage::Nlp)
        .expect("nlp stage ran");
    let meta = StageMetadata::from(nlp_decision.metadata.clone());
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert!(
        conf.collision_count > 0,
        "the resolver's surfaced collision must reach the summary (G9)"
    );
}

/// M4.3: refinement products keep `source == Llm/Encoder` (not
/// confidence-bearing) — the `NlpConfidenceSummary` logic treats them as
/// non-gated (overall 1.0) even when a parse confidence is present, and the
/// exhaustive `is_confidence_bearing` match stays honest for future variants.
#[test]
fn refined_products_are_not_confidence_bearing_and_summary_is_well_formed() {
    // Every source must be classified exhaustively (no wildcard).
    assert!(!spacy_rs::AnnotationSource::Llm.is_confidence_bearing());
    assert!(!spacy_rs::AnnotationSource::RuleRung.is_confidence_bearing());
    assert!(!spacy_rs::AnnotationSource::HumanReview.is_confidence_bearing());
    assert!(!spacy_rs::AnnotationSource::Frontier.is_confidence_bearing());
    assert!(spacy_rs::AnnotationSource::ArcEager.is_confidence_bearing());
    assert!(spacy_rs::AnnotationSource::Encoder.is_confidence_bearing());

    // A refined result (Llm) carrying a parse confidence must still report
    // 1.0 — the summary gates only on confidence-bearing sources.
    let vocab = std::sync::Arc::new(spacy_rs::vocab::Vocab::new(
        spacy_rs::lang::en::lexicon_config(),
    ));
    let mut doc = spacy_rs::doc::Doc::new(std::sync::Arc::clone(&vocab));
    doc.push_back("hello", false).expect("push");
    let set = spacy_rs::AnnotationSet(vec![spacy_rs::AnnotationRecord {
        text: "hello".into(),
        pos: "noun".into(),
        dep: "root".into(),
        head: 0,
        tag: String::new(),
        lemma: "hello".into(),
        morph: String::new(),
        ent_iob: String::new(),
        ent_type: String::new(),
    }]);
    let mut refined = spacy_rs::AnnotationResult::new(set, spacy_rs::AnnotationSource::Llm);
    // Give it a fake parse confidence that would be gated if it were ArcEager.
    refined.parse_confidence = Some(spacy_rs::arc_eager::ParseConfidence {
        overall: 0.2,
        role_coverage: 0.0,
        oracle_tie_count: 7,
        token_scores: vec![0.2],
        oracle_margins: vec![0.0],
        semantic_plausibility: Some(0.1),
    });
    refined.collision_count = 1;
    let summary = confidence_summary(&refined);
    assert_eq!(summary.source, spacy_rs::AnnotationSource::Llm);
    // Non-bearing source => confidence is the 1.0 convention, not the low value.
    assert!((summary.overall - 1.0).abs() < f64::EPSILON);
    assert!((summary.role_coverage - 1.0).abs() < f64::EPSILON);
    assert_eq!(summary.oracle_tie_count, 0);
    assert_eq!(summary.collision_count, 1);
    // Encoder IS bearing — its confidence passes through.
    let mut enc = refined.clone();
    enc.source = spacy_rs::AnnotationSource::Encoder;
    let enc_summary = confidence_summary(&enc);
    assert!((enc_summary.overall - 0.2).abs() < f64::EPSILON);
}

/// M4.3: an `OnUncertain`-refined parse's `NlpConfidenceSummary` is
/// well-formed (bounded, sourced correctly) — the stage publishes it even
/// when the refiner wins.
#[test]
fn on_uncertain_refined_parse_summary_is_well_formed() {
    // A fetch that returns a valid annotation array (same shape the real
    // LlmRung produces). The stage is wired with `OnUncertain`; the text
    // "hello" yields a trivial deterministic base that the policy may or may
    // not refine, but either way the summary must be bounded and the source
    // must be one of the known variants.
    let full = r#"[{"text":"hello","pos":"noun","dep":"root","head":0,"lemma":"hello"}]"#;
    let fetch: spacy_rs::pipeline::LlmFetchSync =
        std::sync::Arc::new(move |_tokens: Vec<String>| Ok(full.to_string()));
    let pipeline = std::sync::Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline"));
    let stage = NlpStage::with_strings(pipeline, Some(fetch), None, None).with_refine_policy(
        spacy_rs::RefinePolicy {
            mode: spacy_rs::RefineMode::OnUncertain,
            ..spacy_rs::RefinePolicy::default()
        },
    );
    let (_, decision) = stage.decide(&ctx_for("hello"));
    assert_eq!(decision.verdict, StageVerdict::Passed);
    let meta = StageMetadata::from(decision.metadata);
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    // Summary is well-formed regardless of whether refinement ran.
    assert!((0.0..=1.0).contains(&conf.overall));
    assert!((0.0..=1.0).contains(&conf.role_coverage));
    assert!(matches!(
        conf.source,
        spacy_rs::AnnotationSource::ArcEager
            | spacy_rs::AnnotationSource::Llm
            | spacy_rs::AnnotationSource::Encoder
            | spacy_rs::AnnotationSource::RuleRung
    ));
}

fn on_uncertain() -> spacy_rs::RefinePolicy {
    spacy_rs::RefinePolicy {
        mode: spacy_rs::RefineMode::OnUncertain,
        ..spacy_rs::RefinePolicy::default()
    }
}

/// The typed residual handoff: exactly the six documented keys, built from
/// the confidence summary plus the signals' uncovered roles — never raw
/// string-key traversal outside `StageMetadata`.
#[test]
fn residual_shape_golden_is_typed_handoff() {
    use crate::pipeline_types::NlpResidual;
    let summary = crate::pipeline_types::NlpConfidenceSummary {
        source: spacy_rs::AnnotationSource::ArcEager,
        overall: 0.82,
        role_coverage: 0.5,
        oracle_tie_count: 1,
        collision_count: 0,
        semantic_plausibility: None,
        refine_reason: Some(spacy_rs::RefineReason::Confidence(
            spacy_rs::ConfidenceReason::RoleCoverage,
        )),
    };
    let signals = vec![
        spacy_rs::routing::RoutingSignal {
            sentence: "show me the report".into(),
            predicate: "show".into(),
            subject: None,
            direct_object: Some("report".into()),
            indirect_object: None,
            modifiers: vec![],
            qualifiers: vec![],
            arguments: vec![],
            dependents: vec![],
            tokens: vec![],
            lemmas: vec![],
            pos: vec![],
            deps: vec![],
            heads: vec![],
            interlingua: None,
        },
        spacy_rs::routing::RoutingSignal {
            sentence: "the cat sat".into(),
            predicate: "sat".into(),
            subject: Some("cat".into()),
            direct_object: None,
            indirect_object: None,
            modifiers: vec![],
            qualifiers: vec![],
            arguments: vec![],
            dependents: vec![],
            tokens: vec![],
            lemmas: vec![],
            pos: vec![],
            deps: vec![],
            heads: vec![],
            interlingua: None,
        },
    ];
    let residual = NlpResidual::from_summary_and_signals(&summary, &signals);
    assert!((residual.overall - 0.82).abs() < f64::EPSILON);
    assert!((residual.role_coverage - 0.5).abs() < f64::EPSILON);
    assert_eq!(residual.oracle_tie_count, 1);
    assert_eq!(residual.collision_count, 0);
    assert_eq!(
        residual.refine_reason,
        Some(spacy_rs::RefineReason::Confidence(
            spacy_rs::ConfidenceReason::RoleCoverage
        ))
    );
    // Sentence one lacks the subject, sentence two lacks the object.
    assert_eq!(residual.uncovered_roles, vec!["dobj", "nsubj"]);
    let value = serde_json::to_value(&residual).expect("serializes");
    let keys: std::collections::BTreeSet<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "collision_count",
            "oracle_tie_count",
            "overall",
            "refine_reason",
            "role_coverage",
            "uncovered_roles",
        ]
        .into_iter()
        .collect(),
        "exactly the six documented handoff keys"
    );
    // The typed round-trip through `StageMetadata` preserves the record.
    let mut meta = StageMetadata::empty();
    meta.set_nlp_residual(&residual);
    let back = meta.nlp_residual().expect("nlp_residual");
    assert_eq!(back, residual);
    let reread = StageMetadata::metadata_ref(&meta.as_value().clone()).nlp_residual();
    assert_eq!(reread, Some(residual));
}

/// Fully-covered signals leave no uncovered roles.
#[test]
fn residual_uncovered_roles_empty_when_covered() {
    use crate::pipeline_types::{NlpConfidenceSummary, NlpResidual};
    let summary = NlpConfidenceSummary {
        source: spacy_rs::AnnotationSource::ArcEager,
        overall: 0.95,
        role_coverage: 1.0,
        oracle_tie_count: 0,
        collision_count: 0,
        semantic_plausibility: None,
        refine_reason: Some(spacy_rs::RefineReason::NoTrigger),
    };
    let signals = vec![spacy_rs::routing::RoutingSignal {
        sentence: "the cat chased the mouse".into(),
        predicate: "chased".into(),
        subject: Some("cat".into()),
        direct_object: Some("mouse".into()),
        indirect_object: None,
        modifiers: vec![],
        qualifiers: vec![],
        arguments: vec![],
        dependents: vec![],
        tokens: vec![],
        lemmas: vec![],
        pos: vec![],
        deps: vec![],
        heads: vec![],
        interlingua: None,
    }];
    let residual = NlpResidual::from_summary_and_signals(&summary, &signals);
    assert!(residual.uncovered_roles.is_empty());
}

/// Deterministic-certain input under `OnUncertain` never consults the refine
/// fetch: zero LLM calls, `NoTrigger` recorded on the summary and residual.
#[test]
fn residual_certain_parse_makes_no_refine_call() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = Arc::clone(&calls);
    let fetch: spacy_rs::pipeline::LlmFetchSync = Arc::new(move |_tokens: Vec<String>| {
        calls_c.fetch_add(1, Ordering::SeqCst);
        Ok("[]".to_string())
    });
    let vocab = std::sync::Arc::new(spacy_rs::vocab::Vocab::new(
        spacy_rs::lang::en::lexicon_config(),
    ));
    let tokenizer = spacy_rs::lang::en::tokenizer(vocab.clone()).expect("tokenizer");
    let store = std::sync::Arc::new(fluent_concept::InMemoryConceptStore::new());
    let resolver = std::sync::Arc::new(spacy_rs::interlingua::InterlinguaResolver::new(
        std::sync::Arc::clone(&store) as std::sync::Arc<dyn fluent_concept::ConceptStore>,
        std::sync::Arc::clone(vocab.strings()),
    ));
    let pipeline = std::sync::Arc::new(
        spacy_rs::NlpPipeline::new_with_resolver(
            vocab,
            tokenizer,
            spacy_rs::AnnotationValidator::new(),
            Some(resolver),
        )
        .expect("pipeline"),
    );
    let stage = NlpStage::with_strings(pipeline, Some(fetch), None, None)
        .with_refine_policy(on_uncertain());
    let (_, decision) = stage.decide(&ctx_for("The cat sat on the mat."));
    assert_eq!(decision.verdict, StageVerdict::Passed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "certain parse must not consult the refiner"
    );
    let meta = StageMetadata::from(decision.metadata);
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert_eq!(
        conf.refine_reason,
        Some(spacy_rs::RefineReason::NoTrigger),
        "no trigger recorded, got {:?}",
        conf.refine_reason
    );
    let residual = meta.nlp_residual().expect("nlp_residual");
    assert_eq!(
        residual.refine_reason,
        Some(spacy_rs::RefineReason::NoTrigger)
    );
    assert!((residual.overall - conf.overall).abs() < f64::EPSILON);
    assert!((residual.role_coverage - conf.role_coverage).abs() < f64::EPSILON);
}

/// Genuinely uncertain input under `OnUncertain` consults the refine fetch;
/// the recorded reason is never `NoTrigger`.
#[test]
fn residual_uncertain_parse_makes_refine_call() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let full = r#"[{"text":"John","pos":"propn","dep":"nsubj","head":1,"lemma":"John"},{"text":"left","pos":"verb","dep":"root","head":0,"lemma":"leave"}]"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = Arc::clone(&calls);
    let fetch: spacy_rs::pipeline::LlmFetchSync = Arc::new(move |_tokens: Vec<String>| {
        calls_c.fetch_add(1, Ordering::SeqCst);
        Ok(full.to_string())
    });
    let pipeline = std::sync::Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline"));
    let stage = NlpStage::with_strings(pipeline, Some(fetch), None, None)
        .with_refine_policy(on_uncertain());
    let (_, decision) = stage.decide(&ctx_for("Xylophone zzzqx blorpt."));
    assert_eq!(decision.verdict, StageVerdict::Passed);
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "uncertain parse must consult the refiner"
    );
    let meta = StageMetadata::from(decision.metadata);
    let conf = meta.nlp_confidence().expect("nlp_confidence");
    assert_ne!(
        conf.refine_reason,
        Some(spacy_rs::RefineReason::NoTrigger),
        "a trigger must be recorded"
    );
    let residual = meta.nlp_residual().expect("nlp_residual");
    assert_eq!(residual.refine_reason, conf.refine_reason);
}

/// Refine/residual trigger calibration (measurement only): a labeled
/// certain/uncertain × complete/incomplete corpus pins the trigger table
/// before any parse/correction caching or `CorrectionIndex` writes.
///
/// Axes (roadmap trigger note): certainty is producer self-doubt
/// (`RefineReason::NoTrigger` vs any trigger); completeness is task-value
/// (typed residual `uncovered_roles` empty vs not). The control group —
/// certain-complete — must record zero refine calls. Precision (fire implies
/// the refiner actually changed the parse) and recall (changed implies
/// fired) are exact bounds on this fixed corpus; the deterministic pipeline
/// makes them stable, never fitted noise. Each case runs twice on a fresh
/// pipeline to pin determinism. Thresholds are locked in-test so silent
/// tuning breaks loudly.
#[test]
fn refine_calibration_fire_vs_changed_route() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Case {
        text: &'static str,
        expect_certain: bool,
        expect_complete: bool,
        /// Seed a colliding canonical for this lemma: the resolve step
        /// surfaces a `CollisionNote`, firing the task-value collision
        /// trigger on an otherwise certain-complete sentence. This is the
        /// uncertain-complete quadrant — confidence intact, route incomplete.
        collide_lemma: Option<&'static str>,
    }
    let cases = vec![
        // Control: certain-complete, zero refine calls.
        Case {
            text: "The cat chased the mouse.",
            expect_certain: true,
            expect_complete: true,
            collide_lemma: None,
        },
        Case {
            text: "The teacher praised the student.",
            expect_certain: true,
            expect_complete: true,
            collide_lemma: None,
        },
        // Certain-incomplete: confident parses missing a role never refine.
        Case {
            text: "Show me the report.",
            expect_certain: true,
            expect_complete: false,
            collide_lemma: None,
        },
        Case {
            text: "The blorpt wugged.",
            expect_certain: true,
            expect_complete: false,
            collide_lemma: None,
        },
        // Uncertain-complete: the control sentences with a seeded collision.
        Case {
            text: "The cat chased the mouse.",
            expect_certain: false,
            expect_complete: true,
            collide_lemma: Some("mouse"),
        },
        Case {
            text: "The teacher praised the student.",
            expect_certain: false,
            expect_complete: true,
            collide_lemma: Some("student"),
        },
        // Uncertain-incomplete: low role coverage.
        Case {
            text: "Xylophone zzzqx blorpt.",
            expect_certain: false,
            expect_complete: false,
            collide_lemma: None,
        },
        Case {
            text: "Run the tests.",
            expect_certain: false,
            expect_complete: false,
            collide_lemma: None,
        },
    ];

    // Threshold lock: the defaults the bounds below were measured against.
    // Any silent tuning fails here first, never as a quiet drift.
    let defaults = spacy_rs::RefinePolicy::default();
    assert_eq!(defaults.mode, spacy_rs::RefineMode::Off);
    assert!((defaults.min_overall - 0.7).abs() < f64::EPSILON);
    assert!((defaults.min_role_coverage - 0.5).abs() < f64::EPSILON);
    assert!((defaults.min_token_score - 0.5).abs() < f64::EPSILON);
    assert!((defaults.unresolved_token_threshold - 0.3).abs() < f64::EPSILON);
    assert!(defaults.refine_on_ties);
    assert!(defaults.refine_on_unresolved_critical_role);
    assert!(defaults.refine_on_unresolved_propn);
    assert!(defaults.refine_on_collision_note);

    fn resolver_pipeline_with_collision(collide_lemma: Option<&str>) -> Arc<spacy_rs::NlpPipeline> {
        let vocab = Arc::new(spacy_rs::vocab::Vocab::new(
            spacy_rs::lang::en::lexicon_config(),
        ));
        let tokenizer = spacy_rs::lang::en::tokenizer(vocab.clone()).expect("tokenizer");
        let store = Arc::new(fluent_concept::InMemoryConceptStore::new());
        if let Some(lemma) = collide_lemma {
            store
                .insert(fluent_types::ConceptMetadata {
                    id: fluent_types::lemma_id_for_str(lemma),
                    canonical_name: "colliding-canonical".into(),
                    namespace: fluent_types::InterlinguaNamespace::SpacyLemma,
                    yago_iri: None,
                    yago_class_iri: None,
                    label: None,
                    node_id: None,
                    parent_class_id: None,
                })
                .expect("insert colliding canonical");
        }
        let resolver = Arc::new(spacy_rs::interlingua::InterlinguaResolver::new(
            Arc::clone(&store) as Arc<dyn fluent_concept::ConceptStore>,
            Arc::clone(vocab.strings()),
        ));
        Arc::new(
            spacy_rs::NlpPipeline::new_with_resolver(
                vocab,
                tokenizer,
                spacy_rs::AnnotationValidator::new(),
                Some(resolver),
            )
            .expect("pipeline"),
        )
    }

    // Per-text refiner answers: structurally valid alternative parses the
    // adoption gate can take, keyed by first token. Uncertain bases are low
    // quality, so an adopted alternative changes the parse (the recall
    // signal); certain bases never consult the fetch at all.
    fn annotation_for(tokens: &[String]) -> String {
        let joined = tokens.join(" ");
        if joined.starts_with("The cat chased") {
            r#"[{"text":"The","pos":"det","dep":"det","head":1,"lemma":"the"},{"text":"cat","pos":"noun","dep":"nsubj","head":1,"lemma":"cat"},{"text":"chased","pos":"verb","dep":"root","head":0,"lemma":"chase"},{"text":"the","pos":"det","dep":"det","head":1,"lemma":"the"},{"text":"mouse","pos":"noun","dep":"dobj","head":-2,"lemma":"mouse"},{"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}]"#.to_string()
        } else if joined.starts_with("The teacher praised") {
            r#"[{"text":"The","pos":"det","dep":"det","head":1,"lemma":"the"},{"text":"teacher","pos":"noun","dep":"nsubj","head":1,"lemma":"teacher"},{"text":"praised","pos":"verb","dep":"root","head":0,"lemma":"praise"},{"text":"the","pos":"det","dep":"det","head":1,"lemma":"the"},{"text":"student","pos":"noun","dep":"dobj","head":-2,"lemma":"student"},{"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}]"#.to_string()
        } else if joined.starts_with("Xylophone") {
            r#"[{"text":"Xylophone","pos":"propn","dep":"nsubj","head":2,"lemma":"Xylophone"},{"text":"zzzqx","pos":"noun","dep":"dobj","head":1,"lemma":"zzzqx"},{"text":"blorpt","pos":"verb","dep":"root","head":0,"lemma":"blorpt"},{"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}]"#.to_string()
        } else if joined.starts_with("Run the tests") {
            r#"[{"text":"Run","pos":"verb","dep":"root","head":0,"lemma":"run"},{"text":"the","pos":"det","dep":"det","head":1,"lemma":"the"},{"text":"tests","pos":"noun","dep":"dobj","head":-2,"lemma":"test"},{"text":".","pos":"punct","dep":"punct","head":-1,"lemma":"."}]"#.to_string()
        } else {
            "[]".to_string()
        }
    }

    let on_uncertain_policy = || spacy_rs::RefinePolicy {
        mode: spacy_rs::RefineMode::OnUncertain,
        ..spacy_rs::RefinePolicy::default()
    };
    let mut mismatches = Vec::new();
    let mut control_calls = 0;
    let mut control_total = 0;
    let mut fired_total = 0;
    let mut fired_changed = 0;
    let mut changed_total = 0;
    let mut changed_fired = 0;
    for case in &cases {
        // Baseline (`OnUncertain`, no fetch): the trigger that WOULD fire is
        // evaluated and recorded, but no model can be consulted — this labels
        // the quadrant without spending a call. The seeded store rides along
        // so collision cases label as the trigger sees them.
        let base_stage = NlpStage::with_strings(
            resolver_pipeline_with_collision(case.collide_lemma),
            None,
            None,
            None,
        )
        .with_refine_policy(on_uncertain_policy());
        let (_, base) = base_stage.decide(&ctx_for(case.text));
        assert_eq!(base.verdict, StageVerdict::Passed, "baseline {}", case.text);
        let base_meta = StageMetadata::from(base.metadata);
        let base_conf = base_meta.nlp_confidence().expect("baseline confidence");
        let base_residual = base_meta.nlp_residual().expect("baseline residual");
        let certain = base_conf.refine_reason == Some(spacy_rs::RefineReason::NoTrigger);
        let complete = base_residual.uncovered_roles.is_empty();
        if certain != case.expect_certain {
            mismatches.push(format!(
                "{}: certain={certain} (reason {:?}), expected certain={}",
                case.text, base_conf.refine_reason, case.expect_certain
            ));
        }
        if complete != case.expect_complete {
            mismatches.push(format!(
                "{}: uncovered={:?}, expected complete={}",
                case.text, base_residual.uncovered_roles, case.expect_complete
            ));
        }
        let base_signals = base_meta.nlp_parse().expect("baseline signals");

        // Two refine runs on fresh pipelines pin determinism.
        let mut runs = Vec::new();
        for _ in 0..2 {
            let calls = Arc::new(AtomicUsize::new(0));
            let calls_c = Arc::clone(&calls);
            let fetch: spacy_rs::pipeline::LlmFetchSync = Arc::new(move |tokens: Vec<String>| {
                calls_c.fetch_add(1, Ordering::SeqCst);
                Ok(annotation_for(&tokens))
            });
            let stage = NlpStage::with_strings(
                resolver_pipeline_with_collision(case.collide_lemma),
                Some(fetch),
                None,
                None,
            )
            .with_refine_policy(on_uncertain_policy());
            let (_, decision) = stage.decide(&ctx_for(case.text));
            assert_eq!(
                decision.verdict,
                StageVerdict::Passed,
                "refine {}",
                case.text
            );
            let meta = StageMetadata::from(decision.metadata);
            let conf = meta.nlp_confidence().expect("confidence");
            let signals = meta.nlp_parse().expect("signals");
            runs.push((
                calls.load(Ordering::SeqCst),
                conf.source,
                conf.refine_reason,
                signals,
            ));
        }
        if runs[0].3 != runs[1].3 || runs[0].1 != runs[1].1 || runs[0].0 != runs[1].0 {
            mismatches.push(format!("{}: refine runs disagree", case.text));
        }
        let (calls, source, reason, signals) = &runs[0];
        let fired = *calls > 0;
        // Changed route: the refiner's product won (source flipped to the
        // model rung) or the published signals differ from the baseline.
        let changed = *source == spacy_rs::AnnotationSource::Llm || *signals != base_signals;
        if case.expect_certain && case.expect_complete {
            control_total += 1;
            control_calls += *calls;
        }
        if certain && complete && fired {
            mismatches.push(format!("{}: certain-complete must not fire", case.text));
        }
        if fired {
            fired_total += 1;
            if changed {
                fired_changed += 1;
            }
        }
        if changed {
            changed_total += 1;
            if fired {
                changed_fired += 1;
            }
        }
        let _ = reason;
    }
    assert!(
        mismatches.is_empty(),
        "calibration mismatches:\n{}",
        mismatches.join("\n")
    );
    assert_eq!(
        (control_total, control_calls),
        (2, 0),
        "control (certain-complete) must record zero refine calls"
    );
    assert_eq!(
        (fired_changed, fired_total),
        (4, 4),
        "precision must be 1.0: every fire changes the parse"
    );
    assert_eq!(
        (changed_fired, changed_total),
        (4, 4),
        "recall must be 1.0: every change was fired by the trigger"
    );
}

/// With `nlp: false` the pipeline emits no NLP/overlay handoff keys: the
/// typed residual never leaks into configs that never opted in.
#[test]
fn nlp_overlay_disabled_pipeline_carries_no_handoff_keys() {
    let nlp_deps = NlpDeps {
        concept_store: None,
        strings_path: None,
    };
    let config: RouterConfig = serde_json::from_str(
        r#"{
            "pipelines": {
                "default": {"deterministic_prefilter": false, "nlp": false, "classifier": false}
            }
        }"#,
    )
    .expect("config");
    let map = config.build_all_pipelines_with_backend_onnx_and_nlp(None, None, &nlp_deps);
    let pipeline = map.get("default").expect("pipeline built");
    let output = pipeline
        .execute(&ctx_for("show me the report"))
        .expect("execute");
    let result: PipelineResult = output.data_take().expect("pipeline result");
    assert!(
        result
            .decisions
            .iter()
            .all(|d| !matches!(d.stage, PipelineStage::Nlp | PipelineStage::Overlay)),
        "no NLP/overlay decisions when disabled"
    );
    for d in &result.decisions {
        let meta = StageMetadata::from(d.metadata.clone());
        assert!(meta.nlp_residual().is_none(), "no residual when disabled");
        assert!(
            meta.overlay_route_hints().is_none(),
            "no hints when disabled"
        );
    }
}

#[test]
fn nlp_stage_new_with_fetch_defaults_to_off() {
    let fetch: spacy_rs::pipeline::LlmFetchSync =
        std::sync::Arc::new(|_tokens: Vec<String>| Ok("[]".to_string()));
    let pipeline = std::sync::Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline"));
    let stage = NlpStage::new(pipeline, Some(fetch), None);
    assert_eq!(
        stage.refine_policy().mode,
        spacy_rs::RefineMode::Off,
        "NlpStage::new must default to Off even when fetch is wired"
    );
    let pipeline2 = std::sync::Arc::new(spacy_rs::NlpPipeline::en_default().expect("en pipeline"));
    let encoder: spacy_rs::pipeline::EncoderFetchSync =
        std::sync::Arc::new(|_doc| Err(spacy_rs::pipeline::AnnotateError::Encoder("x".into())));
    let stage2 = NlpStage::new(pipeline2, None, Some(encoder));
    assert_eq!(
        stage2.refine_policy().mode,
        spacy_rs::RefineMode::Off,
        "NlpStage::new must default to Off even when encoder is wired"
    );
}
