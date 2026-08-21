//! Opt-in live-AI test for the real `libneedle.so` FFI seam.
//!
//! Proves the C-ABI end-to-end: resolves `libneedle` from `NEEDLE_LIB_PATH`,
//! loads the native engine, binds a small route→tool schema, and runs one
//! grammar-constrained completion, asserting only **structural invariants**
//! (a valid typed envelope; any tool call names one of the declared tools).
//! It never asserts model output or routing-decision quality — the engine is
//! non-generative and its pick is not asserted.
//!
//! Two further tests drive the **real engine through the `NeedlePreFilter`
//! stage** (the enhance-roadmap paths): the routing window gate decides on the
//! first sentence/paragraph rather than the whole prompt, and every verdict the
//! stage can emit is structurally valid (window metadata, direct template
//! rendering, general-category fallback, route target). The engine's pick is
//! never asserted — only the stage's contract around whatever it returns.
//!
//! Compiled only when the `live-ai` feature is enabled and `#[ignore]`d, so it
//! can never run under `make test` / `make router-test` / `make router-mock` /
//! CI. Run via `make test-live` (or `make router-test-live`).
//!
//! Env contract: `NEEDLE_LIB_PATH` must point at a real `libneedle.so`
//! (mirrors the `needle::engine::resolve_library_path` resolution order). When
//! no library is resolvable the test skips cleanly (early `return`, never
//! panic) per the roadmap's skip-not-fail policy.

use std::collections::HashMap;
use std::sync::Arc;

use fluent_router::config::{
    ModelGroup, NeedleConfig, NeedleRouteSchema, RouteRef, RoutingConfig,
};
use fluent_router::needle::backend::NeedleBackend;
use fluent_router::needle::engine::{resolve_library_path, NativeNeedleEngine};
use fluent_router::needle::envelope::NeedleEnvelopeType;
use fluent_router::pipeline_types::{StageDecision, StageVerdict};
use fluent_router::stages::needle::NeedlePreFilter;
use fluent_router::stages::common::routing_window;
use fluent_router::types::{RouterMessage, RouterMessageContent, RouterRequest};
use fluent_wvr::prelude::*;

/// A tiny route→tool schema: the two tools the engine may call.
const TOOLS_JSON: &str = r#"[
    {"name": "code", "description": "Programming, software development, and debugging", "parameters": {"type": "object", "properties": {}, "required": []}},
    {"name": "translation", "description": "Translate between different languages", "parameters": {"type": "object", "properties": {}, "required": []}}
]"#;

/// Load a real engine, or `None` (→ test skips) when `NEEDLE_LIB_PATH` is
/// unresolvable. Shared by every live needle test.
fn live_engine() -> Option<Arc<dyn NeedleBackend>> {
    let lib_path = resolve_library_path()?;
    Some(Arc::new(
        NativeNeedleEngine::load(&lib_path, "You route a request to the single best tool.", None, None)
            .expect("a real libneedle must load"),
    ))
}

fn route_ref(group: &str, description: &str) -> RouteRef {
    RouteRef {
        group: group.into(),
        pipelines: vec!["default".into()],
        description: description.into(),
        always_route: false,
    }
}

/// A routing config whose route keys are the tools the engine may name: a
/// plain route (`code`), a template-bearing route (`extract`), and a `general`
/// route (`local`). Model entries are loopback and never reached — the stage
/// only resolves a routing target, it never dispatches.
fn stage_routing() -> RoutingConfig {
    let mut routes = HashMap::new();
    routes.insert("code".into(), route_ref("code", "programming and debugging"));
    routes.insert("extract".into(), route_ref("extract", "structured extraction"));
    routes.insert("local".into(), route_ref("local", "general q&a"));
    let mut models = HashMap::new();
    models.insert(
        "code_model".into(),
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://127.0.0.1:1/v1/chat/completions",
            "name": "code-model",
            "intelligence": 4,
            "cost_input": 1e-6, "cost_output": 6e-6, "cost_cached_read": 4e-7, "speed": 5,
        }))
        .unwrap(),
    );
    models.insert(
        "extract_model".into(),
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://127.0.0.1:1/v1/chat/completions",
            "name": "extract-model",
            "intelligence": 1,
            "cost_input": 1e-6, "cost_output": 6e-6, "cost_cached_read": 4e-7, "speed": 8,
        }))
        .unwrap(),
    );
    models.insert(
        "local_model".into(),
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://127.0.0.1:1/v1/chat/completions",
            "name": "local-model",
            "intelligence": 1,
            "cost_input": 1e-6, "cost_output": 6e-6, "cost_cached_read": 4e-7, "speed": 8,
        }))
        .unwrap(),
    );
    let mut model_groups = HashMap::new();
    model_groups.insert("code".into(), ModelGroup::Array(vec!["code_model".into()]));
    model_groups.insert("extract".into(), ModelGroup::Array(vec!["extract_model".into()]));
    model_groups.insert("local".into(), ModelGroup::Array(vec!["local_model".into()]));
    RoutingConfig {
        routes,
        models,
        model_groups,
        system_prompt: String::new(),
        safety_threshold: 0.7,
        default_route: "local".into(),
        score_matrix: None,
    }
}

/// The needle config: template on `extract`, `general` on `local`.
fn stage_needle_config() -> NeedleConfig {
    let mut config = NeedleConfig::default();
    config.schema_overrides.insert(
        "extract".into(),
        NeedleRouteSchema {
            name: "extract".into(),
            description: "structured extraction".into(),
            examples: vec![],
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            intents: vec![],
            output_template: Some("Extracted: {value}".into()),
            general: false,
        },
    );
    config.schema_overrides.insert(
        "local".into(),
        NeedleRouteSchema {
            name: "local".into(),
            description: "general q&a".into(),
            examples: vec![],
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            intents: vec!["general".into()],
            output_template: None,
            general: true,
        },
    );
    config
}

fn ctx_for(command: &str) -> WorkContext {
    let request = RouterRequest {
        model: "test".into(),
        messages: vec![RouterMessage {
            role: "user".into(),
            content: RouterMessageContent::Text(command.into()),
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

fn run_stage(stage: &NeedlePreFilter, command: &str) -> StageDecision {
    let output = stage
        .execute(&ctx_for(command))
        .expect("stage execute must not error");
    output.data_as().expect("typed StageDecision")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live-AI: requires NEEDLE_LIB_PATH to a real libneedle.so; run via `make test-live`"]
async fn needle_live_ffi_completion_structural() {
    let Some(backend) = live_engine() else {
        eprintln!("libneedle.so not found (NEEDLE_LIB_PATH unset/absent); skipping live needle test");
        return;
    };
    assert!(backend.is_available(), "a loaded engine reports available");

    let envelope = backend
        .complete("write a rust function that sorts a list", TOOLS_JSON, 256)
        .expect("completion succeeds against a real libneedle");

    // Structural invariants only.
    assert!(
        matches!(
            envelope.r#type,
            NeedleEnvelopeType::Call | NeedleEnvelopeType::Text | NeedleEnvelopeType::Refuse
        ),
        "envelope must be a known type, got {:?}",
        envelope.r#type
    );
    if envelope.r#type == NeedleEnvelopeType::Call {
        if let Some(tool) = envelope.single_tool() {
            assert!(
                tool == "code" || tool == "translation",
                "a Call envelope's tool must name a declared tool, got '{tool}'"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live-AI: requires NEEDLE_LIB_PATH to a real libneedle.so; run via `make test-live`"]
async fn needle_live_stage_decides_on_window_not_full_prompt() {
    let Some(backend) = live_engine() else {
        eprintln!("libneedle.so not found (NEEDLE_LIB_PATH unset/absent); skipping live needle test");
        return;
    };
    // `max_command_chars` is below the full prompt but above its routing
    // window (the first sentence). A stage that gated on the whole message
    // would gate-skip ("too long"); the window-gated stage consults the engine.
    let mut config = stage_needle_config();
    config.max_command_chars = 60;
    let stage = NeedlePreFilter::new(backend, config, stage_routing());

    let window = "Write a Rust function that sorts a vec.";
    let command = format!("{window} {}", "extra context ".repeat(20));
    assert!(
        command.chars().count() > 60,
        "test premise: full prompt must exceed max_command_chars"
    );
    assert_eq!(
        routing_window(&command),
        window,
        "test premise: the routing window is the first sentence"
    );

    let decision = run_stage(&stage, &command);
    let meta = fluent_router::pipeline_types::StageMetadata::from(decision.metadata);
    assert_eq!(
        meta.needle_window(),
        Some(window),
        "the recorded window must be the first sentence, not the whole prompt"
    );
    assert!(
        !decision.reason.contains("too long"),
        "the window gate must not reject a prompt whose window fits: {}",
        decision.reason
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live-AI: requires NEEDLE_LIB_PATH to a real libneedle.so; run via `make test-live`"]
async fn needle_live_stage_verdicts_are_structurally_valid() {
    let Some(backend) = live_engine() else {
        eprintln!("libneedle.so not found (NEEDLE_LIB_PATH unset/absent); skipping live needle test");
        return;
    };
    let stage = NeedlePreFilter::new(backend, stage_needle_config(), stage_routing());
    let decision = run_stage(&stage, "Write a Rust function that sorts a vec.");
    let meta = fluent_router::pipeline_types::StageMetadata::from(decision.metadata);

    // Whatever the real engine returned, the stage's contract holds:
    assert!(
        matches!(
            decision.verdict,
            StageVerdict::Rerouted | StageVerdict::Passed | StageVerdict::Skipped
        ),
        "verdict must be a known Needle outcome, got {:?}",
        decision.verdict
    );
    assert!(
        meta.needle_window().is_some_and(|w| !w.is_empty()),
        "every live verdict must record the routing window"
    );
    match decision.verdict {
        StageVerdict::Rerouted => {
            let rt = meta
                .routing_target()
                .expect("a Rerouted verdict must carry a routing target");
            assert_eq!(rt.target_name.as_deref(), Some("code"));
        }
        StageVerdict::Passed => {
            // Either a direct template response (rendered from `{value}`) or a
            // recorded action call — both carry the named tool.
            assert!(meta.needle_tool().is_some(), "a Passed verdict names the tool");
            if let Some(resp) = meta.needle_response() {
                assert!(
                    resp.starts_with("Extracted:"),
                    "a direct response must be the rendered output_template, got {resp:?}"
                );
            }
        }
        StageVerdict::Skipped => {
            assert!(
                meta.needle_reason().is_some_and(|r| !r.is_empty()),
                "a Skipped verdict must carry a decline reason"
            );
        }
        _ => unreachable!("verdict already constrained above"),
    }
}
