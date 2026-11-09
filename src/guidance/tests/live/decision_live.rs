//! Live canary probe for decision-model calibration.
//!
//! Runs the shared `canary_suite` against the live endpoint and prints
//! the canary fingerprint (`canary_signature`) that scopes cache keys
//! and registry entries. Compiled only with `live-ai`, `#[ignore]`d,
//! skip-not-fail without env. Run via `make guidance-test-live`.
//!
//! Env contract: `LLM_BASE_URL` + `LLM_MODEL` (see `tests/live/README.md`).

use fluent_llm::client::{block_on, LlmClient};
use fluent_llm::decision::{
    canary_signature, canary_suite, verify_canary, DecideRequest, DecisionKind, DecisionOption,
};

fn live_env() -> Option<(String, String)> {
    let base = std::env::var("LLM_BASE_URL").ok()?;
    let model = std::env::var("LLM_MODEL").ok()?;
    Some((base, model))
}

#[test]
#[ignore = "live-AI: probes the decision model; run via `make guidance-test-live`"]
fn canary_probe_reports_fingerprint() {
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping canary probe");
        return;
    };

    let client = LlmClient::new(&base, &model);
    let suite = canary_suite();
    let mut decisions = Vec::new();
    for case in &suite {
        let request = DecideRequest {
            prompt: case.prompt.clone(),
            state: String::new(),
            question: String::new(),
            options: case
                .options
                .iter()
                .map(|label| DecisionOption {
                    label: label.clone(),
                    description: String::new(),
                })
                .collect(),
            kind: DecisionKind::Choice,
            version: 1,
            timeout_ms: 30_000,
            model: model.clone(),
        };
        match block_on(client.decide_request(request)) {
            Ok(decision) => {
                match verify_canary(&decision, case) {
                    Ok(()) => eprintln!("canary {}: pass", case.id),
                    Err(drift) => eprintln!("canary {}: DRIFT ({})", case.id, drift.detail),
                }
                decisions.push(decision);
            }
            Err(error) => {
                // Transport failure against a live endpoint is an
                // environment problem, not a code failure: report and stop.
                eprintln!("canary probe transport failure: {error}; stopping");
                return;
            }
        }
    }
    assert!(
        !decisions.is_empty(),
        "probe answered at least one canary case"
    );
    eprintln!("canary fingerprint: {}", canary_signature(&decisions));
}
