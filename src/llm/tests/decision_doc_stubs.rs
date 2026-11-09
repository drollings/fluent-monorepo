//! Stale-name guard for the decision module docs.
//!
//! Existence proofs (each import fails on rename) plus a doc-mention check:
//! every public item must appear in a `///` line of its own module, so
//! documentation cannot silently stop naming what it documents.

#[allow(unused_imports)]
use fluent_llm::decision::{
    aggregate_rotations, anonymize_hook, authorize, canary_signature, canary_suite, control_fires,
    decide_aggregated, decide_all, decide_noul_verdict, decide_score_verdict, decide_verdict,
    decide_with_ladder, decode_choice, decode_noul, decode_score, emit_point_artifact,
    evaluate_point, filter_noul, gate_for_point, gate_for_point_with, instrumented_shadow,
    noul_p_yes, prefix_cascade, rank_score, render_point_table, render_prompt, replay_shadow_log,
    rotated_options, rotation_agreement, route_choice, route_hierarchical, score_moment,
    shadow_adapter, verify_canary, AuditSink, BatchFuture, CacheKeyInput, CalibrationRegistry,
    CalibrationStatus, CanaryCase, CanaryDrift, CaseOrigin, CaseProvenance, Corpus,
    CorpusProvenance, DecideBackend, DecideRequest, Decision, DecisionAudit, DecisionBreaker,
    DecisionCache, DecisionError, DecisionKind, DecisionOption, DecisionProbe, DecisionTransport,
    DecodeDecisionUnit, GateParams, GateReport, PolicyVerdictUnit, PreparedPrompt, Question,
    QuestionSpec, RedactHook, RegistryEntry, RenderQuestionUnit, ReplayItem, ReplayOutcome,
    ScoreMoment, ScoredCase, ScriptedDecideBackend, ShadowBridge, ShadowJob, ShadowLog,
    ShadowOutcome, ShadowWork, ThresholdSet, Verdict, MAX_DECISION_OPTIONS,
    MAX_DECISION_PROMPT_BYTES,
};

const MODULES: &[&str] = &[
    include_str!("../src/decision/backend.rs"),
    include_str!("../src/decision/breaker.rs"),
    include_str!("../src/decision/bridge.rs"),
    include_str!("../src/decision/bulk.rs"),
    include_str!("../src/decision/cache.rs"),
    include_str!("../src/decision/canary.rs"),
    include_str!("../src/decision/decode.rs"),
    include_str!("../src/decision/eval.rs"),
    include_str!("../src/decision/ladder.rs"),
    include_str!("../src/decision/question.rs"),
    include_str!("../src/decision/registry.rs"),
    include_str!("../src/decision/shadow.rs"),
    include_str!("../src/decision/transport.rs"),
    include_str!("../src/decision/types.rs"),
    include_str!("../src/decision/wvr.rs"),
];

fn doc_lines() -> String {
    MODULES
        .iter()
        .flat_map(|m| m.lines())
        .filter(|l| l.trim_start().starts_with("///"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn stubs_name_only_documented_public_items() {
    let docs = doc_lines();
    for name in [
        "DecisionBreaker",
        "aggregate_rotations",
        "bulk_filter",
        "bulk_rerank",
        "bulk_route",
        "decide_aggregated",
        "decide_all",
        "filter_noul",
        "rank_score",
        "rotation_agreement",
        "route_choice",
        "route_hierarchical",
        "decision_cache_key",
        "CacheKeyInput",
        "DecisionCache",
        "canary_signature",
        "canary_suite",
        "verify_canary",
        "CanaryCase",
        "CanaryDrift",
        "decode_choice",
        "decode_noul",
        "decode_score",
        "control_fires",
        "emit_point_artifact",
        "evaluate_point",
        "gate_for_point",
        "gate_for_point_with",
        "render_point_table",
        "ScoredCase",
        "decide_with_ladder",
        "prefix_cascade",
        "render_prompt",
        "rotated_options",
        "DecisionOptions",
        "Question",
        "QuestionSpec",
        "MAX_DECISION_OPTIONS",
        "MAX_DECISION_PROMPT_BYTES",
        "authorize",
        "CalibrationRegistry",
        "CalibrationStatus",
        "CaseOrigin",
        "CaseProvenance",
        "Corpus",
        "CorpusProvenance",
        "GateParams",
        "GateReport",
        "RegistryEntry",
        "instrumented_shadow",
        "shadow_adapter",
        "ShadowLog",
        "ShadowOutcome",
        "anonymize_hook",
        "AuditSink",
        "DecisionAudit",
        "DecisionProbe",
        "DecisionTransport",
        "PreparedPrompt",
        "RedactHook",
        "BatchFuture",
        "DecideBackend",
        "DecideRequest",
        "Decision",
        "DecisionError",
        "DecisionKind",
        "DecisionOption",
        "DecodeDecisionUnit",
        "PolicyVerdictUnit",
        "RenderQuestionUnit",
        "ReplayItem",
        "ReplayOutcome",
        "ScoreMoment",
        "ScriptedDecideBackend",
        "ShadowBridge",
        "ShadowJob",
        "ShadowWork",
        "ThresholdSet",
        "Verdict",
        "decide_noul_verdict",
        "decide_score_verdict",
        "decide_verdict",
        "noul_p_yes",
        "replay_shadow_log",
        "score_moment",
    ] {
        assert!(docs.contains(name), "docs must keep naming `{name}`");
    }
}
