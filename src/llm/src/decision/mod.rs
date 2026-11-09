//! Decision-model support: bounded Choice/Score/Noul judgments.
//!
//! Pure parts (prompt rendering, decoding, policy) stay synchronous and
//! side-effect free; all network I/O lives in the native-async decide seam.

pub mod backend;
pub mod breaker;
pub mod bridge;
pub mod bulk;
pub mod cache;
pub mod canary;
pub mod decode;
pub mod eval;
pub mod ladder;
pub mod question;
pub mod registry;
pub mod shadow;
pub mod transport;
pub mod types;
pub mod wvr;

pub use backend::{
    BatchFuture, DecideBackend, DecideRequest, DecisionOption, ScriptedDecideBackend,
};

pub use breaker::DecisionBreaker;
pub use bridge::{
    replay_shadow_log, ReplayItem, ReplayOutcome, ShadowBridge, ShadowJob, ShadowWork,
};
pub use bulk::{
    aggregate_rotations, bulk_filter, bulk_rerank, bulk_route, decide_aggregated, decide_all,
    filter_noul, rank_score, rotation_agreement, route_choice, route_hierarchical,
};
pub use cache::{decision_cache_key, CacheKeyInput, DecisionCache};
pub use canary::{canary_signature, canary_suite, verify_canary, CanaryCase, CanaryDrift};
pub use eval::{
    control_fires, emit_point_artifact, evaluate_point, gate_for_point, render_point_table,
    ScoredCase,
};
pub use ladder::{decide_with_ladder, prefix_cascade};
pub use registry::{
    authorize, gate_for_point_with, CalibrationRegistry, CalibrationStatus, CaseOrigin,
    CaseProvenance, Corpus, CorpusProvenance, GateParams, GateReport, RegistryEntry,
};
pub use shadow::{instrumented_shadow, shadow_adapter, ShadowLog, ShadowOutcome};
pub use transport::{
    anonymize_hook, AuditSink, DecisionAudit, DecisionProbe, DecisionTransport, PreparedPrompt,
    RedactHook,
};

pub use decode::{decode_choice, decode_noul, decode_score};
pub use question::{
    render_prompt, rotated_options, DecisionOptions, Question, QuestionSpec, MAX_DECISION_OPTIONS,
    MAX_DECISION_PROMPT_BYTES,
};
pub use types::{
    decide_noul_verdict, decide_score_verdict, decide_verdict, noul_p_yes, score_moment, Decision,
    DecisionError, DecisionKind, ScoreMoment, ThresholdSet, Verdict,
};
pub use wvr::{DecodeDecisionUnit, PolicyVerdictUnit, RenderQuestionUnit};
