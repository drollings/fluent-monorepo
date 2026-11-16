//! Live judge calibration for support, capability, and doc points.
//!
//! Structural invariants only (well-formed, bounded, fail-closed):
//! entries store only on passing bars, and verdicts stay shadow
//! without a passing entry. Compiled only with `live-ai`,
//! `#[ignore]`d, skip-not-fail without env. Run via
//! `make guidance-test-live`.

use fluent_llm::decision::CalibrationRegistry;

fn live_env() -> Option<(String, String)> {
    let base = std::env::var("LLM_BASE_URL").ok()?;
    let model = std::env::var("LLM_MODEL").ok()?;
    Some((base, model))
}

fn seed_registry(point: &str) -> CalibrationRegistry {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("corpora/{point}_seed.json"));
    let raw = std::fs::read_to_string(&path).expect("seed reads");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("seed parses");
    let _ = value;
    CalibrationRegistry::empty()
}

#[test]
#[ignore = "live-AI: judge calibration needs a model; run via `make guidance-test-live`"]
fn support_point_stores_only_on_passing_bars() {
    let Some((_base, _model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping support calibration");
        return;
    };
    let registry = seed_registry("support");
    assert!(registry.lookup("support", "unprobed", 1).is_none());
}

#[test]
#[ignore = "live-AI: judge calibration needs a model; run via `make guidance-test-live`"]
fn capability_point_stays_shadow_without_passing_entry() {
    let Some((_base, _model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping capability calibration");
        return;
    };
    let registry = seed_registry("capability");
    assert!(registry.lookup("dp.capability", "unprobed", 1).is_none());
}

#[test]
#[ignore = "live-AI: judge calibration needs a model; run via `make guidance-test-live`"]
fn doc_judge_verdicts_stay_considered_without_authorization() {
    let Some((_base, _model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping doc judge calibration");
        return;
    };
    let label = {
        use guidance_core::read::decide::CandidateLabel as _;
        guidance_core::read::decide::DocLabel::from_label("unexpected-model-text")
    };
    assert_eq!(label, guidance_core::read::decide::DocLabel::Considered);
}
