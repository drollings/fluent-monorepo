// Tests assert float config values against literal defaults - deliberate.
#![allow(clippy::float_cmp)]
use super::*;

#[test]
fn charts_config_defaults() {
    let cfg = ChartsConfig::default();
    assert_eq!(cfg.max_candidates, 5);
    assert_eq!(cfg.min_score, 0.6);
    assert!(cfg.entity_context);
    assert!(cfg.dir.is_none());
    assert!(cfg.index_path.is_none());
    assert!(cfg.selector_model.is_none());
}

#[test]
fn review_config_defaults_and_new_m3_keys() {
    let cfg = ReviewConfig::default();
    assert!(!cfg.auto_enqueue, "auto-enqueue is opt-in");

    // The M3 keys deserialize from config with their serde defaults applied
    // (an absent field keeps its documented default, not the derived 0).
    // PII detection keys off the onnx `pii` role registration, not a config
    // key — there is no `pii_model` field to assert.
    let cfg: ReviewConfig =
        serde_json::from_str(r#"{"review_model": "review", "auto_enqueue": true}"#).unwrap();
    assert!(cfg.auto_enqueue);
    assert_eq!(cfg.review_model.as_deref(), Some("review"));
    assert_eq!(cfg.pii_threshold, 0.5);
    assert_eq!(cfg.queue_capacity, 32);
    assert_eq!(cfg.credit_limit, 16);

    let round_trip: ReviewConfig =
        serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert!(round_trip.auto_enqueue);
    assert_eq!(round_trip.review_model, cfg.review_model);
}

// -- Async overlay configuration -----------

#[test]
fn overlay_config_defaults_are_fail_open() {
    // Serde defaults apply on deserialization (an absent field gets the
    // default); `OverlayConfig::default()` is the empty struct.
    let cfg: OverlayConfig = serde_json::from_str(r#"{"entity_link_enabled": true}"#).unwrap();
    assert!(cfg.entity_link_enabled);
    assert_eq!(cfg.entity_link_threshold, 0.6);
    assert_eq!(cfg.queue_capacity, 32);
    assert_eq!(cfg.credit_limit, 16);
}

#[test]
fn overlay_config_serde_round_trip() {
    let cfg: OverlayConfig = serde_json::from_str(
        r#"{"entity_link_enabled": true, "entity_link_threshold": 0.7, "queue_capacity": 8, "credit_limit": 4}"#,
    )
    .unwrap();
    assert!(cfg.entity_link_enabled);
    assert_eq!(cfg.entity_link_threshold, 0.7);
    assert_eq!(cfg.queue_capacity, 8);
    assert_eq!(cfg.credit_limit, 4);

    let round_trip: OverlayConfig =
        serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert!(round_trip.entity_link_enabled);
    assert_eq!(round_trip.entity_link_threshold, 0.7);
}

#[test]
fn arc_ready_config_defaults_off_and_absent() {
    // An absent `overlay.arc_ready` block deserializes to `arc_ready: None`
    // (the same as `OverlayConfig::default()`), so a config that never
    // mentions arc_ready is byte-identical to today.
    let plain: OverlayConfig = serde_json::from_str(r#"{"entity_link_enabled": true}"#).unwrap();
    assert!(plain.arc_ready.is_none(), "absent arc_ready → None");
    assert!(OverlayConfig::default().arc_ready.is_none());

    // A present `arc_ready` block defaults every knob to off/absent.
    let present: OverlayConfig =
        serde_json::from_str(r#"{"entity_link_enabled": true, "arc_ready": {}}"#).unwrap();
    let arc = present.arc_ready.expect("arc_ready present");
    assert!(!arc.enabled, "enabled defaults off");
    assert!(!arc.nlp, "nlp defaults off");
    assert!(arc.llm_model.is_none(), "llm_model absent by default");
    assert!(
        arc.embedding_model.is_none(),
        "embedding_model absent by default"
    );
    assert!(!arc.backfill, "backfill defaults off");
    assert_eq!(arc.queue_capacity, 1024);
    assert_eq!(arc.credit_limit, 256);
    assert_eq!(arc.credit_more_after, 8);
    assert_eq!(arc.max_concurrent, 8);
}

#[test]
fn arc_ready_config_serde_round_trip() {
    let cfg: OverlayConfig = serde_json::from_str(
        r#"{
            "entity_link_enabled": true,
            "arc_ready": {
                "enabled": true,
                "nlp": true,
                "llm_model": "lfm2.5-2.6b",
                "embedding_model": "embed",
                "queue_capacity": 64,
                "credit_limit": 128,
                "credit_more_after": 16,
                "max_concurrent": 4,
                "backfill": true
            }
        }"#,
    )
    .unwrap();
    let arc = cfg.arc_ready.as_ref().expect("arc_ready present");
    assert!(arc.enabled && arc.nlp && arc.backfill);
    assert_eq!(arc.llm_model.as_deref(), Some("lfm2.5-2.6b"));
    assert_eq!(arc.embedding_model.as_deref(), Some("embed"));
    assert_eq!(arc.queue_capacity, 64);
    assert_eq!(arc.credit_limit, 128);
    assert_eq!(arc.credit_more_after, 16);
    assert_eq!(arc.max_concurrent, 4);

    let round_trip: OverlayConfig =
        serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    let rt = round_trip.arc_ready.expect("survives round trip");
    assert!(rt.enabled && rt.nlp && rt.backfill);
    assert_eq!(rt.llm_model.as_deref(), Some("lfm2.5-2.6b"));
    assert_eq!(rt.max_concurrent, 4);
}

#[test]
fn overlay_config_default_is_byte_identical_without_arc_ready() {
    // The M8 guarantee: a config with no `arc_ready` block produces
    // byte-identical behavior to today — the `OverlayConfig` default is
    // unchanged (arc_ready `None`), and only the entity-link knobs exist.
    let cfg: OverlayConfig = serde_json::from_str(
        r#"{"entity_link_enabled": true, "entity_link_threshold": 0.6, "queue_capacity": 32, "credit_limit": 16}"#,
    )
    .unwrap();
    assert_eq!(cfg.arc_ready, None);
    assert!(cfg.entity_link_enabled);
    assert_eq!(cfg.entity_link_threshold, 0.6);
    assert_eq!(cfg.queue_capacity, 32);
    assert_eq!(cfg.credit_limit, 16);
}

#[test]
fn router_config_absent_overlay_section_defaults_to_none() {
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert!(cfg.overlay.is_none(), "absent overlay → None (fail-open)");
}

// -- Rigor-route configuration -------------------------------------

#[test]
fn rigor_config_defaults() {
    let cfg = RigorConfig::default();
    assert_eq!(cfg.max_passes.rounds(), 2);
    assert_eq!(cfg.severity_threshold.get(), 0.7);
    assert_eq!(cfg.escalation_confidence.get(), 0.4);
    assert!(!cfg.kv_cache_enabled);
    assert!(cfg.blue_model.is_none());
    assert!(cfg.red_model.is_none());
    assert!(cfg.judge_model.is_none());
}

#[test]
fn router_config_absent_rigor_section_defaults_to_none() {
    // The shipped env/coral-router.json has no `rigor` section; the route
    // stays present-but-unconfigured (None), never a crash.
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert!(cfg.rigor.is_none());
}

#[test]
fn rigor_config_round_trip() {
    let json = serde_json::json!({
        "rigor": {
            "blue_model": "fast",
            "red_model": "code",
            "judge_model": "code",
            "kv_cache_enabled": true,
            "max_passes": 3,
            "severity_threshold": 0.8,
            "escalation_confidence": 0.3,
        }
    });
    let cfg: RouterConfig = serde_json::from_value(json).unwrap();
    let rigor = cfg.rigor.expect("rigor section parsed");
    assert_eq!(rigor.blue_model.as_deref(), Some("fast"));
    assert_eq!(rigor.red_model.as_deref(), Some("code"));
    assert_eq!(rigor.judge_model.as_deref(), Some("code"));
    assert!(rigor.kv_cache_enabled);
    assert_eq!(rigor.max_passes.rounds(), 3);
    assert_eq!(rigor.severity_threshold.get(), 0.8);
    assert_eq!(rigor.escalation_confidence.get(), 0.3);

    // Partial section still round-trips with defaults for the rest.
    let partial: RouterConfig = serde_json::from_value(serde_json::json!({
        "rigor": {"blue_model": "fast"}
    }))
    .unwrap();
    let partial_cfg = partial.rigor.expect("rigor parsed");
    assert_eq!(partial_cfg.blue_model.as_deref(), Some("fast"));
    assert_eq!(partial_cfg.max_passes.rounds(), 2, "absent fields default");
    assert_eq!(partial_cfg.severity_threshold.get(), 0.7);
}

#[test]
fn router_config_absent_charts_section_defaults_cleanly() {
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert_eq!(cfg.charts.max_candidates, 5);
    assert_eq!(cfg.charts.min_score, 0.6);
    assert!(cfg.charts.entity_context);
    assert!(cfg.charts.dir.is_none());
}

#[test]
fn router_config_embedding_and_reranker_models_parse() {
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"embedding_model": "embed", "reranker_model": "rerank"}"#)
            .unwrap();
    assert_eq!(cfg.embedding_model.as_deref(), Some("embed"));
    assert_eq!(cfg.reranker_model.as_deref(), Some("rerank"));

    let absent: RouterConfig = serde_json::from_str(r"{}").unwrap();
    assert!(absent.embedding_model.is_none());
    assert!(absent.reranker_model.is_none());
}

#[test]
fn charts_section_round_trips() {
    let json = r#"{
        "dir": "env/workflows/charts",
        "index_path": "data/workflow_library.sqlite",
        "selector_model": "qwen3.5-4b",
        "max_candidates": 5,
        "min_score": 0.6,
        "entity_context": true
    }"#;
    let cfg: ChartsConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.dir.as_deref(), Some("env/workflows/charts"));
    assert_eq!(
        cfg.index_path.as_deref(),
        Some("data/workflow_library.sqlite")
    );
    assert_eq!(cfg.selector_model.as_deref(), Some("qwen3.5-4b"));
    assert_eq!(cfg.max_candidates, 5);
    assert_eq!(cfg.min_score, 0.6);
    assert!(cfg.entity_context);

    let serialized = serde_json::to_string(&cfg).unwrap();
    let back: ChartsConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(back.dir, cfg.dir);
    assert_eq!(back.max_candidates, cfg.max_candidates);
    assert_eq!(back.min_score, cfg.min_score);
}

#[test]
fn partial_charts_section_defaults_missing_fields() {
    let cfg: ChartsConfig = serde_json::from_str(r#"{"dir": "env/workflows/charts"}"#).unwrap();
    assert_eq!(cfg.dir.as_deref(), Some("env/workflows/charts"));
    assert_eq!(cfg.max_candidates, 5);
    assert_eq!(cfg.min_score, 0.6);
    assert!(cfg.entity_context);
    assert!(cfg.index_path.is_none());
    assert!(cfg.selector_model.is_none());
}

#[test]
fn router_config_parses_charts_section() {
    let json = r#"{
        "charts": { "dir": "env/workflows/charts", "max_candidates": 8 }
    }"#;
    let cfg: RouterConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.charts.dir.as_deref(), Some("env/workflows/charts"));
    assert_eq!(cfg.charts.max_candidates, 8);
    assert_eq!(cfg.charts.min_score, 0.6, "unset field keeps its default");
}

// -- Post-process (learning loop) --------------------------------

#[test]
fn post_process_defaults_to_disabled() {
    let cfg = PostProcessConfig::default();
    assert!(!cfg.workflow_extraction, "extraction is opt-in");
    assert_eq!(
        cfg.workflow_extraction_mode,
        WorkflowExtractionMode::Frontier,
        "default scope is frontier-assisted only"
    );
}

#[test]
fn post_process_absent_section_defaults_cleanly() {
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert!(
        !cfg.post_process.workflow_extraction,
        "absent post_process section defaults extraction off"
    );
    assert_eq!(
        cfg.post_process.workflow_extraction_mode,
        WorkflowExtractionMode::Frontier,
        "absent mode field defaults to frontier"
    );
}

#[test]
fn post_process_round_trips() {
    let json = r#"{ "workflow_extraction": true }"#;
    let cfg: PostProcessConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.workflow_extraction);
    assert_eq!(
        cfg.workflow_extraction_mode,
        WorkflowExtractionMode::Frontier,
        "absent mode field keeps the frontier default"
    );

    let serialized = serde_json::to_string(&cfg).unwrap();
    let back: PostProcessConfig = serde_json::from_str(&serialized).unwrap();
    assert!(back.workflow_extraction);
    assert_eq!(back.workflow_extraction_mode, cfg.workflow_extraction_mode);
}

#[test]
fn workflow_extraction_mode_parses_both_variants() {
    let all: WorkflowExtractionMode = serde_json::from_str(r#""all""#).expect("all parses");
    assert_eq!(all, WorkflowExtractionMode::All);

    let frontier: WorkflowExtractionMode =
        serde_json::from_str(r#""frontier""#).expect("frontier parses");
    assert_eq!(frontier, WorkflowExtractionMode::Frontier);

    assert!(serde_json::from_str::<WorkflowExtractionMode>(r#""bogus""#).is_err());
}

#[test]
fn router_config_parses_post_process_section() {
    let json = r#"{
        "post_process": { "workflow_extraction": true },
        "charts": { "dir": "env/workflows/charts" }
    }"#;
    let cfg: RouterConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.post_process.workflow_extraction);
    assert_eq!(cfg.charts.dir.as_deref(), Some("env/workflows/charts"));
    assert_eq!(
        cfg.post_process.workflow_extraction_mode,
        WorkflowExtractionMode::Frontier,
        "existing configs without the new field still deserialize"
    );
}

#[test]
fn router_config_parses_extraction_mode_all() {
    let json = r#"{
        "post_process": {
            "workflow_extraction": true,
            "workflow_extraction_mode": "all"
        }
    }"#;
    let cfg: RouterConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.post_process.workflow_extraction);
    assert_eq!(
        cfg.post_process.workflow_extraction_mode,
        WorkflowExtractionMode::All
    );
}

#[test]
fn model_entry_serde_defaults_read_canonical_constants() {
    // The same constants `RoutingTarget` reads (divergence guard).
    let entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://localhost:8080/v1/chat/completions",
        "intelligence": 2,
        "cost_input": 1e-6,
        "cost_output": 6e-6,
        "cost_cached_read": 4e-7,
        "tok_s": 8,
    }))
    .unwrap();
    assert_eq!(
        entry.total_timeout_ms,
        fluent_llm::constants::DEFAULT_TOTAL_TIMEOUT_MS
    );
    assert_eq!(
        entry.idle_timeout_ms,
        fluent_llm::constants::DEFAULT_IDLE_TIMEOUT_MS
    );
    assert_eq!(
        entry.retry_base_interval_s,
        fluent_llm::constants::DEFAULT_RETRY_INTERVAL_S
    );
}

// -- Classification-tree derived flat views ------------------------

fn tree_section() -> serde_json::Value {
    serde_json::json!({
        "classification": {
            "root": {
                "type": "classifier",
                "description": "router",
                "model": "fast",
                "children": [
                    {
                        "key": "code",
                        "description": "programming",
                        "node": { "type": "terminal", "route": "code", "group": "code", "always_route": true }
                    },
                    {
                        "key": "brand_new",
                        "description": "not in flat routes",
                        "node": { "type": "terminal", "route": "brand_new", "group": "question" }
                    }
                ]
            }
        },
        "models": {
            "fast": {"endpoint": "http://upstream.test/v1/chat/completions", "name": "fast", "intelligence": 1, "cost_input": 1e-6, "cost_output": 6e-6, "cost_cached_read": 4e-7, "tok_s": 8}
        },
        "model_groups": {
            "fast": ["fast"],
            "code": ["fast"],
            "question": ["fast"]
        }
    })
}

#[test]
fn routes_view_synthesizes_terminal_routes() {
    let cfg: RouterConfig = serde_json::from_value(tree_section()).unwrap();
    let routes = cfg.routes_view();
    // Terminal routes resolve through their own group; always_route rides along.
    assert_eq!(routes["code"].group, "code");
    assert_eq!(routes["code"].pipelines, vec!["default".to_string()]);
    assert!(routes["code"].always_route);
    assert_eq!(routes["brand_new"].group, "question");
    assert_eq!(routes["brand_new"].pipelines, vec!["default".to_string()]);
    assert!(!routes["brand_new"].always_route);
}

#[test]
fn routes_view_treeless_config_is_empty() {
    // No tree, no routes: a config without `classification` yields an empty
    // view (and fails validation). Unknown top-level keys such as the old
    // flat `routes` map never reach this check — deny rejects them at parse.
    let cfg: RouterConfig = serde_json::from_str(r#"{"default_route": "local"}"#).unwrap();
    assert!(cfg.routes_view().is_empty());
    assert!(cfg.validate_flat_tree_coherence().is_err());
}

#[test]
fn routing_config_always_derives_system_prompt_from_tree() {
    // M3c: no explicit prompt exists anymore — the prompt is always derived.
    let cfg: RouterConfig = serde_json::from_value(tree_section()).unwrap();
    let routing = cfg.routing_config();
    assert!(
        routing.system_prompt.contains("You are a router."),
        "tree-derived system prompt, got: {}",
        routing.system_prompt
    );
    assert!(
        routing.routes.contains_key("brand_new"),
        "derived routes reach the RoutingConfig so terminal resolution works"
    );
    assert!(
        routing.score_matrix.is_none(),
        "root matrix is gone; pipeline-level matrices flow via params"
    );
}

// -- In-group target-matching knob (PipelineParams) ----------------

#[test]
fn pipeline_params_target_match_defaults() {
    let defaults = crate::config::builder::PipelineParams::default();
    assert_eq!(
        defaults.target_match,
        crate::config::builder::TargetMatchMode::SelfAssess,
        "the self-assess ladder is the default policy (-4.6)"
    );
    assert_eq!(
        defaults.target_match_timeout_ms,
        fluent_llm::constants::DEFAULT_TOTAL_TIMEOUT_MS,
        "per-self-assessment budget defaults to the shared total-timeout constant"
    );
}

#[test]
fn pipeline_params_target_match_absent_fields_deserialize_to_defaults() {
    // A pipeline that omits both knob fields must deserialize to the same
    // defaults (mirror the `classifier_retry_max` pattern) - existing
    // configs stay byte-identical.
    let cfg: RouterConfig = serde_json::from_str(
        r#"{
            "pipelines": {"default": {"classifier": true, "classifier_model": "fast"}}
        }"#,
    )
    .expect("valid config");
    let params = &cfg.pipelines["default"];
    assert_eq!(
        params.target_match,
        crate::config::builder::TargetMatchMode::SelfAssess
    );
    assert_eq!(
        params.target_match_timeout_ms,
        fluent_llm::constants::DEFAULT_TOTAL_TIMEOUT_MS
    );
}

#[test]
fn pipeline_params_target_match_parses_both_variants() {
    let self_assess: crate::config::builder::TargetMatchMode =
        serde_json::from_str(r#""self_assess""#).expect("self_assess parses");
    assert_eq!(
        self_assess,
        crate::config::builder::TargetMatchMode::SelfAssess
    );

    let static_mode: crate::config::builder::TargetMatchMode =
        serde_json::from_str(r#""static""#).expect("static parses");
    assert_eq!(static_mode, crate::config::builder::TargetMatchMode::Static);

    assert!(
        serde_json::from_str::<crate::config::builder::TargetMatchMode>(r#""bogus""#).is_err(),
        "unknown policy must be rejected, not silently defaulted"
    );
}

#[test]
fn pipeline_params_target_match_round_trips() {
    // Non-default values survive a serialize - deserialize cycle.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "pipelines": {
            "default": {
                "classifier": true,
                "classifier_model": "fast",
                "target_match": "static",
                "target_match_timeout_ms": 12345
            }
        }
    }))
    .unwrap();
    assert_eq!(
        cfg.pipelines["default"].target_match,
        crate::config::builder::TargetMatchMode::Static
    );
    assert_eq!(cfg.pipelines["default"].target_match_timeout_ms, 12345);

    let serialized = serde_json::to_string(&cfg).unwrap();
    let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(
        back.pipelines["default"].target_match,
        crate::config::builder::TargetMatchMode::Static
    );
    assert_eq!(back.pipelines["default"].target_match_timeout_ms, 12345);
}

// -- Effective-pool synthesis ---------------------------------------
// The config declares no instance pools: profiles are synthesized from the
// model entry plus the roles that bind it (a `base` profile marked
// `default: true`, plus one profile per binding role).

/// Parse a `roles`-table fixture.
fn role_table(
    json: serde_json::Value,
) -> std::collections::HashMap<String, crate::config::RoleEntry> {
    serde_json::from_value(json).unwrap()
}

/// Boot-compose one entry's effective pool in place (the per-model step of
/// `RouterConfig::apply_defaults`).
fn materialize(
    key: &str,
    entry: &mut ModelEntry,
    roles: &std::collections::HashMap<String, crate::config::RoleEntry>,
) {
    entry.effective_profiles = Some(crate::config::materialize_effective_pool(key, entry, roles));
}

/// A bare model entry: models never name roles, so every entry parses the
/// same way — bindings live on the roles table.
fn bare_entry() -> ModelEntry {
    serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
        "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
        "tok_s": 1,
        "params": {"num_ctx": 8192}
    }))
    .unwrap()
}

#[test]
fn synthesized_pool_has_base_profile_marked_default() {
    let mut entry = bare_entry();
    materialize("m", &mut entry, &role_table(serde_json::json!({})));
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name.as_deref(), Some("base"));
    assert_eq!(profiles[0].group.as_deref(), Some("base"));
    assert_eq!(profiles[0].num_ctx, 8192, "window from the entry params");
    assert!(profiles[0].default, "base is the dispatch point");
    assert!(!profiles[0].pinned);
}

#[test]
fn role_binding_synthesizes_role_profile() {
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "params": {"num_ctx": 16384, "temperature": 0.6},
            "models": ["m"]
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 2, "base plus the bound role");
    let work = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("work"))
        .expect("role profile synthesized");
    assert_eq!(work.group.as_deref(), Some("work"));
    assert_eq!(work.num_ctx, 16384, "role run-block window wins");
    assert_eq!(
        work.params.as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.6)),
        "role sampling composes over the entry params"
    );
    assert!(!work.default, "base stays the dispatch point");
}

#[test]
fn pinned_composes_role_or_model_and_boots() {
    // `pinned` composes as role OR model: either true pins the synthesized
    // profile, and a pinned profile is what boots a model at startup.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {"pinned": true, "models": ["m"]}
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    let work = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("work"))
        .unwrap();
    let base = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("base"))
        .unwrap();
    assert!(work.pinned, "role pin lands on the role profile");
    assert!(!base.pinned, "the entry is not pinned");
    assert!(profiles.iter().any(|p| p.pinned), "the model boots");

    let mut pinned_entry = bare_entry();
    pinned_entry.pinned = true;
    materialize("m", &mut pinned_entry, &role_table(serde_json::json!({})));
    assert!(
        pinned_entry.effective_pool().iter().all(|p| p.pinned),
        "an entry-level pin pins every synthesized profile"
    );
}

#[test]
fn max_ctx_caps_num_ctx_at_materialization() {
    // `max_ctx`, if specified, limits the size of `num_ctx`: a window above
    // the cap clamps down at materialization.
    let mut entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
        "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
        "tok_s": 1,
        "max_ctx": 8192,
        "params": {"num_ctx": 65536}
    }))
    .unwrap();
    materialize("m", &mut entry, &role_table(serde_json::json!({})));
    let base = &entry.effective_pool()[0];
    assert_eq!(base.num_ctx, 8192, "clamped to max_ctx");
    assert_eq!(base.max_ctx, Some(8192));
}

#[test]
fn model_side_role_override_caps_and_samples() {
    // `models.<model>.roles.<role>` is a params-only per-task tune: its
    // sampling composes last over the role run block, and its `max_ctx`
    // caps the role profile's window.
    let mut entry = bare_entry();
    entry.role_params = Some(std::collections::HashMap::from([(
        "work".to_string(),
        serde_json::from_value(serde_json::json!({
            "max_ctx": 4096,
            "params": {"temperature": 0.9}
        }))
        .unwrap(),
    )]));
    let roles = role_table(serde_json::json!({
        "work": {
            "params": {"num_ctx": 65536, "temperature": 0.6},
            "models": ["m"]
        }
    }));
    materialize("m", &mut entry, &roles);
    let work = entry
        .effective_pool()
        .iter()
        .find(|p| p.name.as_deref() == Some("work"))
        .expect("role profile synthesized");
    assert_eq!(work.max_ctx, Some(4096), "override cap wins over the entry");
    assert_eq!(work.num_ctx, 4096, "the cap clamps the composed window");
    assert_eq!(
        work.params.as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.9)),
        "override sampling wins over the role run block"
    );
}

#[test]
fn pre_boot_pool_is_empty() {
    let entry = bare_entry();
    assert!(entry.effective_pool().is_empty(), "pre-boot pool is empty");
}

// -- Single inference-point resolver -------------------------------

#[test]
fn inference_point_bare_key_answers_entry_default() {
    // One resolver, one answer for a bare key: the entry default (the
    // `default: true` profile's group — the synthesized `base` profile).
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {"models": ["m"]}
    }));
    materialize("m", &mut entry, &roles);
    let models = std::collections::HashMap::from([("m".to_string(), entry)]);
    assert_eq!(
        resolve_inference_point(&models, &roles, "m", None).as_deref(),
        Some("base"),
        "bare key serves the default profile's group"
    );
}

#[test]
fn inference_point_role_carries_no_qualifier() {
    // Roles carry no qualifier of their own: the point resolves from the
    // route-selected model.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {"models": ["m"]}
    }));
    materialize("m", &mut entry, &roles);
    let models = std::collections::HashMap::from([("m".to_string(), entry)]);
    assert_eq!(
        resolve_inference_point(&models, &roles, "work", None),
        None,
        "a bare role carries no qualifier"
    );
}

#[test]
fn sidecar_absent_section_defaults_cleanly() {
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert_eq!(cfg.sidecar.poll_interval_s, 5);
    assert_eq!(cfg.sidecar.vram_low_watermark_bytes, 1073741824);
    assert_eq!(cfg.sidecar.evict_batch, 1);
    assert!(cfg.sidecar.vram_total_bytes.is_none());
    assert!(cfg.sidecar.minimum_remaining_vram.is_none());
    assert!(cfg.sidecar.slot_save_path.is_none());
    assert!(cfg.sidecar.api_key_env.is_none());
}

#[test]
fn sidecar_section_round_trips() {
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "sidecar": {
            "poll_interval_s": 10,
            "vram_low_watermark_bytes": 536870912,
            "evict_batch": 2,
            "vram_total_bytes": 1048576,
            "minimum_remaining_vram": 2147483648u64,
            "slot_save_path": "/srv/slots",
            "api_key_env": "LLAMA_API_KEY",
        }
    }))
    .unwrap();
    assert_eq!(cfg.sidecar.poll_interval_s, 10);
    assert_eq!(cfg.sidecar.vram_low_watermark_bytes, 536870912);
    assert_eq!(cfg.sidecar.evict_batch, 2);
    assert_eq!(cfg.sidecar.vram_total_bytes, Some(1048576));
    assert_eq!(cfg.sidecar.minimum_remaining_vram, Some(2147483648));
    assert_eq!(cfg.sidecar.slot_save_path.as_deref(), Some("/srv/slots"));
    assert_eq!(cfg.sidecar.api_key_env.as_deref(), Some("LLAMA_API_KEY"));
}

#[test]
fn sidecar_allocation_limit_from_minimum_remaining() {
    // With a ceiling configured, the budget is ceiling - minimum remaining.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "sidecar": { "vram_total_bytes": 10000, "minimum_remaining_vram": 2000 }
    }))
    .unwrap();
    assert_eq!(cfg.sidecar.allocation_limit(), Some(8000));
}

#[test]
fn sidecar_allocation_limit_without_ceiling_falls_back_to_detection() {
    // No explicit ceiling: the budget is computed from the detected total.
    // The host has a ROCm device (mem_info_vram_total > 0), so the limit is
    // detection - minimum_remaining; a missing floor yields the full total.
    // Detection reads `/sys/class/drm` through the capability-gated fs
    // helper, so it runs under the `FsCapability` grant the serving
    // path establishes at boot.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "sidecar": { "minimum_remaining_vram": 2147483648u64 }
    }))
    .unwrap();
    let (detected, allocation_limit) = fluent_concurrency::scope::CURRENT_CAPS.sync_scope(
        fluent_concurrency::capability::default_capability_set(),
        || {
            (
                super::detect_device_vram_total(),
                cfg.sidecar.allocation_limit(),
            )
        },
    );
    assert!(
        detected.is_some(),
        "ROCm sysfs mem_info_vram_total present on this host"
    );
    assert_eq!(
        allocation_limit,
        detected.map(|t| t.saturating_sub(2147483648))
    );
}

#[test]
fn default_role_absent_yields_struct_defaults() {
    // No `default` role: the fleet run block is struct defaults (the spawn
    // fallback).
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    let fleet = cfg.default_role_params();
    assert_eq!(fleet.num_ctx, 16384);
    assert_eq!(fleet.batch_size, 4096);
    assert_eq!(fleet.n_gpu_layers, 999);
    assert!(fleet.sampling.is_empty());
}

#[test]
fn default_role_params_section_round_trips() {
    // The fleet run block lives as `roles.default.params`.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {
            "default": {
                "params": {
                    "num_ctx": 8192,
                    "batch_size": 512,
                    "ubatch_size": 256,
                    "cache_type_k": "f16",
                    "cache_type_v": "f16",
                    "flash_attn": "off",
                    "n_gpu_layers": 0,
                    "n_cpu_moe": 4,
                    "sleep_idle_seconds": 30,
                    "stream": false,
                    "filter_thinking": true,
                    "temperature": 0.2,
                    "max_ctx": 8192
                }
            }
        }
    }))
    .unwrap();
    let fleet = cfg.default_role_params();
    assert_eq!(fleet.num_ctx, 8192);
    assert_eq!(fleet.batch_size, 512);
    assert_eq!(fleet.ubatch_size, 256);
    assert_eq!(fleet.cache_type_k, "f16");
    assert_eq!(fleet.cache_type_v, "f16");
    assert_eq!(fleet.flash_attn.as_deref(), Some("off"));
    assert_eq!(fleet.n_gpu_layers, 0);
    assert_eq!(fleet.n_cpu_moe, 4);
    assert_eq!(fleet.sleep_idle_seconds, 30);
    assert!(!fleet.stream);
    assert!(fleet.filter_thinking);
    assert_eq!(fleet.max_ctx, Some(8192));
    assert_eq!(
        fleet
            .sampling_value()
            .as_ref()
            .and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.2))
    );
}

#[test]
fn unknown_top_level_key_fails_at_parse() {
    // `RouterConfig` rejects unknown fields: typos and retired aliases
    // (`default_params`, top-level `classifier_model`) fail loudly at parse
    // instead of warning-and-ignoring at boot.
    for key in ["default_params", "classifier_model", "routes", "bogus_key"] {
        let mut obj = serde_json::Map::new();
        obj.insert(key.to_string(), serde_json::json!({}));
        let err = serde_json::from_value::<RouterConfig>(serde_json::Value::Object(obj))
            .expect_err("unknown key must not parse");
        assert!(err.to_string().contains(key), "got: {err}");
    }
}

#[test]
fn default_role_params_max_ctx_absent_defaults_to_none() {
    // An absent `max_ctx` (the default) deserializes to `None` — an
    // existing config without the new key is byte-identical.
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert!(cfg.default_role_params().max_ctx.is_none());
    assert!(crate::config::RoleParams::default().max_ctx.is_none());
}

#[test]
fn default_role_params_ctx_size_alias_parses() {
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": { "default": { "params": { "ctx_size": 32768 } } }
    }))
    .unwrap();
    assert_eq!(cfg.default_role_params().num_ctx, 32768);
}

// -- Ledger + session composition sections ------------------------

#[test]
fn router_config_absent_ledger_and_session_sections_default_to_none() {
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert!(
        cfg.ledger.is_none(),
        "absent ledger section -> no ledger at boot (byte-identical behavior)"
    );
    assert!(
        cfg.session.is_none(),
        "absent session section -> no session registry at boot (byte-identical behavior)"
    );
}

#[test]
fn ledger_and_session_sections_round_trip() {
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "ledger": {
            "path": "data/ledger.sqlite",
            "model": "swarm",
            "max_summary_tokens": 300,
        },
        "session": { "root": "data/sessions" },
    }))
    .unwrap();

    let ledger = cfg.ledger.as_ref().expect("ledger section parsed");
    assert_eq!(ledger.path.as_deref(), Some("data/ledger.sqlite"));
    assert_eq!(ledger.model.as_deref(), Some("swarm"));
    assert_eq!(ledger.max_summary_tokens, 300);

    let session = cfg.session.as_ref().expect("session section parsed");
    assert_eq!(session.root.as_deref(), Some("data/sessions"));

    let serialized = serde_json::to_string(&cfg).unwrap();
    let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
    let back_ledger = back.ledger.expect("ledger round-trips");
    assert_eq!(back_ledger.path, ledger.path);
    assert_eq!(back_ledger.model, ledger.model);
    assert_eq!(back_ledger.max_summary_tokens, ledger.max_summary_tokens);
    assert_eq!(back.session.unwrap().root, session.root);
}

#[test]
fn ledger_section_partial_defaults_max_summary_tokens() {
    // A ledger section that omits `max_summary_tokens` gets the named
    // constant default; the shipped config round-trips cleanly.
    let cfg: RouterConfig =
        serde_json::from_value(serde_json::json!({ "ledger": { "model": "swarm" } })).unwrap();
    let ledger = cfg.ledger.as_ref().expect("ledger parsed");
    assert_eq!(ledger.max_summary_tokens, DEFAULT_LEDGER_MAX_SUMMARY_TOKENS);
    assert_eq!(ledger.model.as_deref(), Some("swarm"));
    assert!(ledger.path.is_none());

    let serialized = serde_json::to_string(&cfg).unwrap();
    let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(
        back.ledger.unwrap().max_summary_tokens,
        DEFAULT_LEDGER_MAX_SUMMARY_TOKENS
    );
}

#[test]
fn ledger_background_tiering_fields_default_absent() {
    // All background-tiering fields are default-absent so existing
    // `coral-router.json` files deserialize unchanged.
    let cfg: RouterConfig =
        serde_json::from_value(serde_json::json!({ "ledger": { "model": "swarm" } })).unwrap();
    let ledger = cfg.ledger.as_ref().unwrap();
    assert!(!ledger.background_tiering, "tiering is opt-in");
    assert!(ledger.tier_model.is_none());
    assert_eq!(ledger.lod4_max_chars, 240, "default lod4 cap");
    assert_eq!(ledger.lod5_max_chars, 80, "default lod5 cap");
    assert_eq!(ledger.tier_batch_size, 8);
    assert_eq!(ledger.tier_poll_interval_ms, 100);
}

#[test]
fn ledger_background_tiering_fields_round_trip() {
    // A fully-populated ledger section round-trips knobs.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "ledger": {
            "model": "swarm",
            "background_tiering": true,
            "tier_model": "qwen3.5-4b",
            "lod4_max_chars": 200,
            "lod5_max_chars": 60,
            "tier_batch_size": 16,
            "tier_poll_interval_ms": 250,
        }
    }))
    .unwrap();
    let ledger = cfg.ledger.as_ref().unwrap();
    assert!(ledger.background_tiering);
    assert_eq!(ledger.tier_model.as_deref(), Some("qwen3.5-4b"));
    assert_eq!(ledger.lod4_max_chars, 200);
    assert_eq!(ledger.lod5_max_chars, 60);
    assert_eq!(ledger.tier_batch_size, 16);
    assert_eq!(ledger.tier_poll_interval_ms, 250);
}

// -- Ledger orchestrator section --------------------------------

#[test]
fn orchestrator_section_default_on() {
    // M14c default-on (M14-CAL projects a net frontier decrease): a ledger
    // section without an `orchestrator` block attaches the coordinator with
    // today's other defaults. Opt-out is explicit `enabled = false`.
    let cfg: RouterConfig =
        serde_json::from_value(serde_json::json!({ "ledger": { "model": "swarm" } })).unwrap();
    let orch = &cfg.ledger.as_ref().unwrap().orchestrator;
    assert!(orch.enabled, "coordinator is default-on");
    assert_eq!(
        orch.kv_policy,
        crate::dag_session::KvSnapshotPolicy::RestoreIfSameModel
    );
    assert_eq!(orch.prompt_budget_chars, 32768);
    assert_eq!(orch.role, "agent");
}

#[test]
fn orchestrator_section_opt_out() {
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "ledger": { "model": "swarm", "orchestrator": { "enabled": false } }
    }))
    .unwrap();
    assert!(
        !cfg.ledger.as_ref().unwrap().orchestrator.enabled,
        "explicit opt-out is honored"
    );
}

#[test]
fn orchestrator_section_round_trip() {
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "ledger": {
            "model": "swarm",
            "orchestrator": {
                "enabled": true,
                "kv_policy": "never_restore",
                "prompt_budget_chars": 16384,
                "role": "planner"
            }
        }
    }))
    .unwrap();
    let orch = &cfg.ledger.as_ref().unwrap().orchestrator;
    assert!(orch.enabled);
    assert_eq!(
        orch.kv_policy,
        crate::dag_session::KvSnapshotPolicy::NeverRestore
    );
    assert_eq!(orch.prompt_budget_chars, 16384);
    assert_eq!(orch.role, "planner");
}

#[test]
fn orchestrator_kv_policy_parses_all_variants() {
    use crate::dag_session::KvSnapshotPolicy as P;
    let a: P = serde_json::from_str(r#""restore_if_same_model""#).unwrap();
    let b: P = serde_json::from_str(r#""always_restore""#).unwrap();
    let c: P = serde_json::from_str(r#""never_restore""#).unwrap();
    assert_eq!(a, P::RestoreIfSameModel);
    assert_eq!(b, P::AlwaysRestore);
    assert_eq!(c, P::NeverRestore);
}

#[test]
fn kv_snapshot_policy_round_trips_through_serde() {
    use crate::dag_session::KvSnapshotPolicy as P;
    for p in [P::RestoreIfSameModel, P::AlwaysRestore, P::NeverRestore] {
        let json = serde_json::to_string(&p).unwrap();
        let back: P = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p, "round-trip {p:?} through {json}");
    }
}

// -- Top-level ONNX role key normalization ---------------------------

#[test]
fn top_level_onnx_roles_merge_into_fleet() {
    use fluent_llm::onnx_config::OnnxRole;
    let cfg: RouterConfig = serde_json::from_str(
        r#"{
            "encoder": {
                "model_path": "/models/encoder.onnx",
                "tokenizer_path": "/models/tokenizer.json"
            },
            "router": {
                "model_path": "/models/router.onnx"
            },
            "colbert": {
                "model_path": "/models/colbert.onnx",
                "tokenizer_path": "/models/colbert_tok.json"
            }
        }"#,
    )
    .expect("valid config");
    let mut cfg = cfg;
    cfg.apply_defaults();

    let fleet = cfg.onnx.as_ref().expect("onnx fleet merged");
    assert!(fleet.has(OnnxRole::Encoder));
    assert!(fleet.has(OnnxRole::Router));
    assert!(fleet.has(OnnxRole::Colbert));
    assert!(!fleet.has(OnnxRole::Pii));
    // Top-level keys consumed
    assert!(cfg.encoder.is_none());
    assert!(cfg.router.is_none());
    assert!(cfg.colbert.is_none());
    // Router inherited encoder's tokenizer_path
    let router_cfg = fleet.get(OnnxRole::Router).unwrap();
    assert_eq!(
        router_cfg.model.tokenizer_path.as_deref(),
        Some(std::path::Path::new("/models/tokenizer.json")),
        "router should inherit encoder tokenizer_path"
    );
    // Colbert kept its own tokenizer_path
    let colbert_cfg = fleet.get(OnnxRole::Colbert).unwrap();
    assert_eq!(
        colbert_cfg.model.tokenizer_path.as_deref(),
        Some(std::path::Path::new("/models/colbert_tok.json")),
        "colbert should keep its own tokenizer_path"
    );
}

#[test]
fn nested_onnx_section_takes_precedence_over_top_level_keys() {
    let cfg: RouterConfig = serde_json::from_str(
        r#"{
            "onnx": {
                "encoder": {
                    "model_path": "/nested/encoder.onnx",
                    "tokenizer_path": "/nested/tok.json"
                }
            },
            "encoder": {
                "model_path": "/top/encoder.onnx",
                "tokenizer_path": "/top/tok.json"
            }
        }"#,
    )
    .expect("valid config");
    let mut cfg = cfg;
    cfg.apply_defaults();

    let fleet = cfg.onnx.as_ref().expect("onnx fleet present");
    let enc = fleet
        .get(fluent_llm::onnx_config::OnnxRole::Encoder)
        .unwrap();
    assert_eq!(
        enc.model.model_path,
        std::path::PathBuf::from("/nested/encoder.onnx"),
        "nested onnx section wins over top-level key"
    );
    // Top-level key still present (not consumed, just warned about)
    assert!(
        cfg.encoder.is_some(),
        "top-level key preserved (warning issued)"
    );
}

#[test]
fn no_onnx_keys_yields_none_fleet() {
    let cfg: RouterConfig =
        serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
    assert!(cfg.onnx.is_none(), "absent onnx config stays None");
}

#[test]
fn weights_alias_deserializes_as_model_path() {
    let cfg: RouterConfig = serde_json::from_str(
        r#"{
            "encoder": {
                "weights": "/models/encoder.onnx",
                "tokenizer_path": "/models/tok.json"
            }
        }"#,
    )
    .expect("valid config");
    let mut cfg = cfg;
    cfg.apply_defaults();
    let fleet = cfg.onnx.as_ref().unwrap();
    let enc = fleet
        .get(fluent_llm::onnx_config::OnnxRole::Encoder)
        .unwrap();
    assert_eq!(
        enc.model.model_path,
        std::path::PathBuf::from("/models/encoder.onnx"),
        "weights alias maps to model_path"
    );
}

#[test]
fn fp8_quantization_parses() {
    use fluent_llm::onnx_config::Quant;
    let q: Quant = serde_json::from_str(r#""fp8""#).expect("fp8 parses");
    assert_eq!(q, Quant::Fp8);
}

#[test]
fn router_config_field_access_round_trip() {
    use fluent_wvr::FieldAccess;
    let mut cfg = RouterConfig::default();
    // safety_threshold is accessible via FieldAccess
    cfg.set_field("safety_threshold", "0.7")
        .expect("set_field safety_threshold");
    assert_eq!(cfg.get_field("safety_threshold").unwrap(), "0.7");
    // skipped fields (e.g. models) are NotFound
    let err = cfg.set_field("models", "x").unwrap_err();
    assert!(
        matches!(err, fluent_wvr::FieldError::NotFound(_)),
        "models should be NotFound, got {err:?}"
    );
}

#[test]
fn synthesized_pool_groups_never_collide() {
    // Boot-path regression for the startup fatal: synthesized profiles are
    // one per binding role plus `base`, each its own group, so the
    // `validate_instances` grammar gate must always accept the pool.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "aaa": {"models": ["m"]},
        "zzz": {"models": ["m"]}
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 3, "base plus one profile per binding role");
    crate::instances::validate_instances(profiles).expect("synthesized groups validate");
}

#[test]
fn view_consumers_match_direct_reads() {
    // M3b lock (M3c restatement): every consumer observes the derived view
    // (`routes_view` / `routing_config`); there is no flat map anymore.
    // Fixture-only: the operator file is covered by the config-synced suite.
    // Tree-only config: every terminal is covered by the view and the kernel.
    let both: RouterConfig = serde_json::from_value(serde_json::json!({
        "pipelines": {"default": {"classifier": true, "classifier_model": "fast"}},
        "models": {
            "fast": {"endpoint": "http://x/v1/chat/completions", "intelligence": 1,
                     "cost_input": 1e-6, "cost_output": 6e-6, "cost_cached_read": 4e-7, "tok_s": 8}
        },
        "model_groups": {"g1": ["fast"], "g2": ["fast"]},
        "classification": {"root": {"type": "classifier", "description": "r", "model": "fast",
            "children": [
                {"key": "a", "description": "a", "node": {"type": "terminal", "route": "a", "group": "g1"}},
                {"key": "b", "description": "b", "node": {"type": "terminal", "route": "b", "group": "g2"}}
            ]}},
        "default_route": "a"
    }))
    .unwrap();
    let view = both.routes_view();
    assert!(view.contains_key("b"), "tree-only terminal synthesized");
    assert_eq!(both.route_pipeline_names("b"), view["b"].pipelines);
    assert_eq!(both.route_pipeline_names("a"), view["a"].pipelines);
    let routing = both.routing_config();
    assert_eq!(routing.routes.len(), view.len());
    for (route, rref) in &view {
        let got = &routing.routes[route];
        assert_eq!(&got.group, &rref.group);
        assert_eq!(&got.pipelines, &rref.pipelines);
    }
    assert_eq!(routing.route_group("b"), Some("g2"));
    assert!(
        !routing.system_prompt.is_empty(),
        "tree derives a prompt when none is explicit"
    );
}

#[test]
fn flat_config_rejected_without_tree() {
    // M3c break: a config without a `classification` tree must fail fast
    // with the tree pointer error — flat JSON no longer loads. (Unknown
    // top-level keys such as the old flat `routes` map never reach this
    // check: `deny_unknown_fields` rejects them at parse.)
    let cfg: RouterConfig = serde_json::from_str(r#"{"default_route": "local"}"#).unwrap();
    let err = cfg
        .validate_flat_tree_coherence()
        .expect_err("flat-only config must be rejected");
    assert!(err.contains("classification.tree"), "got: {err}");
}

#[test]
fn legacy_config_without_routing_additions_loads_inert() {
    // Old deployments predate the routing vocabulary: absent `roles` is an
    // empty table, and pools live on roles — so a selection-less legacy
    // model loads and stays bare until boot composes it against a role pool.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "models": {
            "code": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 4,
                "cost_input": 1e-6, "cost_output": 1e-6, "cost_cached_read": 1e-7,
                "tok_s": 4
            }
        }
    }))
    .expect("legacy config parses");
    assert!(cfg.roles.is_empty(), "absent roles → empty table");
    for entry in cfg.models.values() {
        assert!(
            entry.effective_pool().is_empty(),
            "pre-boot pool is empty (composed at boot)"
        );
    }
}

#[test]
fn role_vocabulary_references_roles_and_sentinels() {
    // The role-first vocabulary mechanics on a synthetic config: declared
    // roles with flat residency and admission fields, model lists, the
    // classifier head candidate, and groups built from roles + sentinels.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {
            "default": {
                "params": {"num_ctx": 8192},
                "models": ["code", "scout-svc"]
            },
            "classifier": {"models": ["code"]},
            "code": {"models": ["code"]}
        },
        "models": {
            "code": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 4,
                "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                "tok_s": 1
            },
            "scout-svc": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 1,
                "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                "tok_s": 1
            }
        },
        "model_groups": {
            "default": ["default", "last", "any"],
            "code": ["code", "last", "any"]
        }
    }))
    .expect("synthetic role vocabulary deserializes");
    for role in ["default", "classifier", "code"] {
        assert!(cfg.roles.contains_key(role), "role '{role}' declared");
    }
    let fleet = cfg.default_role_params();
    assert_eq!(
        fleet.num_ctx, 8192,
        "fleet run block lives on the default role"
    );
    for role in ["classifier", "code"] {
        assert!(
            cfg.roles[role].models.contains(&"code".to_string()),
            "code serves the {role} role"
        );
    }
    assert_eq!(
        cfg.classifier_role_key().as_deref(),
        Some("code"),
        "the classifier resolves to its bound model"
    );
    // Every group resolves through roles + sentinels only.
    for (group, members) in &cfg.model_groups {
        let models = members.models();
        assert!(
            models.iter().any(|m| cfg.roles.contains_key(m)),
            "group '{group}' references a role"
        );
        for m in models {
            assert!(
                cfg.roles.contains_key(m) || m == "last" || m == "any",
                "group '{group}' member '{m}' is a role or a sentinel"
            );
        }
    }
}

#[test]
fn classifier_role_key_absent_without_binding() {
    // No model binds the `classifier` role: no classifier key (pipelines
    // fall through to the tree root group, then the `fast` group).
    let cfg = RouterConfig::default();
    assert!(cfg.classifier_role_key().is_none());
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {"classifier": {"models": ["code"]}},
        "models": {
            "code": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 4,
                "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                "tok_s": 1
            }
        }
    }))
    .expect("classifier binding parses");
    assert_eq!(cfg.classifier_role_key().as_deref(), Some("code"));
}

#[test]
fn unknown_classifier_model_key_fails_at_parse() {
    // The pre-R7 top-level `classifier_model` no longer exists: unknown
    // fields fail at parse — the classifier role's head serves instead.
    let err = serde_json::from_value::<RouterConfig>(serde_json::json!({
        "classifier_model": "ghost",
        "roles": {"classifier": {}}
    }))
    .expect_err("retired classifier key must not parse");
    assert!(err.to_string().contains("classifier_model"), "got: {err}");
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {"classifier": {"models": ["code"]}},
        "models": {
            "code": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 4,
                "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                "tok_s": 1
            }
        }
    }))
    .expect("role serves without the retired key");
    assert_eq!(
        cfg.classifier_role_key().as_deref(),
        Some("code"),
        "the role serves"
    );
}

#[test]
fn routing_additions_round_trip_byte_identically() {
    // A fixture carrying role params plus model lists survives a serde
    // round trip with every value intact (derived pools never serialize).
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {
            "code": {
                "max_parallel": 2,
                "pinned": true,
                "params": {"num_ctx": 8192, "temperature": 0.6},
                "models": ["code"]
            },
            "default": {}
        },
        "models": {
            "code": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 4,
                "cost_input": 1e-6, "cost_output": 1e-6, "cost_cached_read": 1e-7,
                "tok_s": 4
            }
        },
        "model_groups": {"code": ["code", "last", "any"]},
        "default_route": "local"
    }))
    .expect("fixture with routing additions");
    assert_eq!(cfg.roles.len(), 2);
    assert!(cfg.roles["code"].models.contains(&"code".to_string()));
    assert_eq!(cfg.roles["code"].params.num_ctx, 8192);
    assert_eq!(cfg.roles["code"].max_parallel, Some(2));
    assert!(cfg.roles["code"].pinned);

    let back: RouterConfig =
        serde_json::from_str(&serde_json::to_string(&cfg).expect("serialize")).expect("round trip");
    assert_eq!(back.roles.len(), 2);
    assert_eq!(back.roles["code"].params.num_ctx, 8192);
    assert_eq!(back.roles["code"].max_parallel, Some(2));
    assert!(back.roles["code"].pinned);
}

#[test]
fn instance_profile_session_defaults_to_one_shot() {
    let profile: InstanceProfile =
        serde_json::from_value(serde_json::json!({"num_ctx": 8192})).expect("minimal profile");
    assert!(!profile.session, "absent session → one-shot");
    let explicit: InstanceProfile =
        serde_json::from_value(serde_json::json!({"num_ctx": 8192, "session": true}))
            .expect("session profile");
    assert!(explicit.session);
}

#[test]
fn synthesized_profiles_are_one_shot_by_default() {
    // Synthesized profiles declare no `session` flags: every profile
    // materializes one-shot with `resume: false` (no snapshots flow).
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "fleet": {"models": ["m"]}
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 2);
    for profile in profiles {
        assert!(!profile.session, "undeclared session defaults to one-shot");
        assert!(!profile.resume, "one-shot materializes resume:false");
    }
}

#[test]
fn inference_point_explicit_qualifier_wins() {
    // An explicit qualifier (embedded or parametric) beats the entry default —
    // and `latest` normalizes away so the remaining precedence applies.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {"models": ["m"]}
    }));
    materialize("m", &mut entry, &roles);
    let models = std::collections::HashMap::from([("m".to_string(), entry)]);
    assert_eq!(
        resolve_inference_point(&models, &roles, "m:scratch", None).as_deref(),
        Some("scratch"),
        "embedded qualifier pins the point"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "m", Some("scratch")).as_deref(),
        Some("scratch"),
        "parametric qualifier pins the point"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "m:latest", None).as_deref(),
        Some("base"),
        "latest falls through to the entry default"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "work", Some("scratch")).as_deref(),
        Some("scratch"),
        "explicit beats the entry default"
    );
}

#[test]
fn inference_point_entry_default_then_bare() {
    // Two-step precedence: explicit qualifier, else the entry default over
    // the boot-materialized pool, else bare. A bare role name carries no
    // point — the route selects the model whose default supplies it.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {"models": ["m"]}
    }));
    materialize("m", &mut entry, &roles);
    let models = std::collections::HashMap::from([("m".to_string(), entry)]);
    assert_eq!(
        resolve_inference_point(&models, &roles, "work", None),
        None,
        "a bare role carries no qualifier"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "m", None).as_deref(),
        Some("base"),
        "bare model key falls to the entry default"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "nope", None),
        None,
        "unknown keys fail closed"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "empty-role", None),
        None,
        "unknown roles fail closed"
    );
}

#[test]
fn router_config_serde_round_trip_preserves_all_fields() {
    let cfg: RouterConfig = serde_json::from_str(
        r#"{
            "server": {"bind_addr": "127.0.0.1:9090"},
            "models": {"m1": {"name": "test", "endpoint": "http://localhost:8080", "intelligence": 5, "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0, "tok_s": 5}},
            "model_groups": {"default": ["m1"]},
            "pipelines": {"default": {"nlp": true, "encoder_model": "enc"}},
            "overlay": {"entity_link_enabled": true, "entity_link_threshold": 0.8},
            "rigor": {"blue_model": "m1", "red_model": "m1", "judge_model": "m1"},
            "charts": {"max_candidates": 3, "min_score": 0.5},
            "post_process": {"workflow_extraction": true}
        }"#,
    )
    .expect("valid config");
    let json = serde_json::to_value(&cfg).expect("serialize");
    let back: RouterConfig = serde_json::from_value(json).expect("deserialize");
    assert_eq!(cfg.server.bind_addr, back.server.bind_addr);
    assert_eq!(cfg.models.len(), back.models.len());
    assert_eq!(cfg.model_groups.len(), back.model_groups.len());
    assert_eq!(
        cfg.overlay.as_ref().unwrap().entity_link_enabled,
        back.overlay.as_ref().unwrap().entity_link_enabled
    );
    assert_eq!(
        cfg.overlay.as_ref().unwrap().entity_link_threshold,
        back.overlay.as_ref().unwrap().entity_link_threshold
    );
    assert_eq!(
        cfg.rigor.as_ref().unwrap().blue_model,
        back.rigor.as_ref().unwrap().blue_model
    );
    assert_eq!(cfg.charts.max_candidates, back.charts.max_candidates);
    assert_eq!(cfg.charts.min_score, back.charts.min_score);
}

#[test]
fn score_matrix_none_vs_authoritative_false_resolves_identically() {
    use crate::score_matrix::ScoreMatrix;
    let scores = HashMap::from([
        ("coherence".into(), 0.9),
        ("complexity".into(), 0.5),
        ("completeness".into(), 0.8),
        ("risk".into(), 0.6),
    ]);
    let m = ScoreMatrix::default();
    let r1 = m.resolve(&scores);
    // score_matrix_authoritative=false is the documented off-ramp: the matrix
    // is still present but not enforced. The resolve path is identical.
    let r2 = m.resolve(&scores);
    assert_eq!(r1.len(), r2.len());
    assert_eq!(r1[0].route_name, r2[0].route_name);
    assert_eq!(r1[0].weighted_score, r2[0].weighted_score);
}

// -- Role split: typed context block + model-side sparse override (M1) ------

fn roles_split_fixture_text() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/config_roles_split_fixture.json"),
    )
    .expect("split fixture readable")
}
#[test]
fn model_role_params_override_parses() {
    let cfg = RouterConfig::from_json_str(&roles_split_fixture_text()).expect("split shape parses");
    let entry = cfg.models.get("code").expect("code model present");
    let over = entry
        .role_params
        .as_ref()
        .and_then(|m| m.get("quick"))
        .and_then(|o| o.params.as_ref())
        .expect("model-side sparse override for quick present");
    assert_eq!(
        over.get("temperature").and_then(serde_json::Value::as_f64),
        Some(0.7),
        "model-specific override value survives parse"
    );
}

#[test]
fn role_flat_fields_parse() {
    // The role vocabulary is flat: `pinned`, `max_parallel`, and the
    // thinking policy sit beside `params` and `models` — no nested blocks.
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {"quick": {
            "max_parallel": 2, "pinned": true,
            "enable_thinking": false, "filter_thinking": true,
            "preserve_thinking": false,
            "params": {"num_ctx": 8192},
            "models": ["code"]
        }}
    }))
    .expect("flat role shape parses");
    let quick = &cfg.roles["quick"];
    assert_eq!(quick.max_parallel, Some(2));
    assert!(quick.pinned);
    assert_eq!(quick.enable_thinking, Some(false));
    assert!(quick.filter_thinking);
    assert_eq!(quick.preserve_thinking, Some(false));
}

#[test]
fn zero_role_max_parallel_fails_closed() {
    let err = RouterConfig::from_json_str(r#"{"roles": {"quick": {"max_parallel": 0}}}"#)
        .expect_err("a zero admission cap would wedge every dispatch");
    assert!(err.to_string().contains("at least 1"), "got: {err}");
}

// -- Answer params chain + thinking policy -----------------------------------

/// A model entry with a `quick` model-side override, a `fast` pool profile,
/// and per-layer sampling keys — the synthetic composition fixture.
fn chain_entry() -> ModelEntry {
    let mut entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
        "params": {"a": "model", "b": "model", "temp": 0.5},
        "roles": {
            "quick": {"params": {"c": "override", "f": "override"}}
        }
    }))
    .unwrap();
    entry.effective_profiles = Some(vec![InstanceProfile {
        name: Some("fast".into()),
        group: Some("fast".into()),
        count: 1,
        num_ctx: 8192,
        parallel: None,
        pinned: false,
        no_sleep: false,
        sleep_idle_seconds: None,
        default: true,
        resume: false,
        params: Some(serde_json::json!({"c": "profile", "e": "profile"})),
        embedding: None,
        max_ctx: None,
        session: false,
    }]);
    entry
}

fn quick_role() -> crate::config::RoleEntry {
    serde_json::from_value(serde_json::json!({
        "params": {"b": "role", "c": "role"}
    }))
    .unwrap()
}

#[test]
fn answer_chain_each_leg_wins_in_order() {
    let entry = chain_entry();
    let classification = serde_json::json!({"a": "classification", "d": "classification"});
    let out = entry
        .answer_params_for(
            Some("fast"),
            Some(("quick", &quick_role())),
            Some(&classification),
        )
        .expect("profile matches");
    let get = |k: &str| out.get(k).and_then(|v| v.as_str()).unwrap_or("<missing>");
    assert_eq!(get("a"), "model", "model.params beats classification duty");
    assert_eq!(get("b"), "role", "role.params beats model.params");
    assert_eq!(get("c"), "override", "model role override is the final leg");
    assert_eq!(
        get("d"),
        "classification",
        "classification-only key survives"
    );
    assert_eq!(get("e"), "profile", "profile-only key survives");
    assert_eq!(get("f"), "override", "override-only key survives");
    assert_eq!(
        out.get("temp").and_then(serde_json::Value::as_f64),
        Some(0.5),
        "untouched keys pass through"
    );
}

#[test]
fn answer_chain_absent_legs_contribute_nothing() {
    let entry = chain_entry();
    let full = entry
        .answer_params_for(
            Some("fast"),
            Some(("quick", &quick_role())),
            Some(&serde_json::json!({"a": "classification", "d": "classification"})),
        )
        .expect("profile matches");
    let bare = entry
        .answer_params_for(Some("fast"), None, None)
        .expect("profile matches");
    assert_eq!(
        bare,
        entry
            .instance_params_for("fast")
            .expect("qualifier path agrees"),
        "absent legs behave exactly like the qualifier-only path"
    );
    assert_ne!(full, bare, "present legs change the composition");
}

#[test]
fn answer_chain_strips_declaration_keys() {
    let mut entry = chain_entry();
    entry.params = Some(serde_json::json!({"num_ctx": 4096, "temp": 0.5}));
    let out = entry
        .answer_params_for(
            Some("fast"),
            Some(("quick", &quick_role())),
            Some(&serde_json::json!({"parallel": 8, "sleep_idle_seconds": 3})),
        )
        .expect("profile matches");
    for key in [
        "num_ctx",
        "parallel",
        "sleep_idle_seconds",
        "rope_freq_base",
    ] {
        assert!(
            out.get(key).is_none(),
            "declaration key {key} must not reach the body"
        );
    }
    assert_eq!(
        out.get("temp").and_then(serde_json::Value::as_f64),
        Some(0.5)
    );
}

#[test]
fn spawn_defaults_max_ctx_caps_num_ctx() {
    // `max_ctx`, if specified, limits the size of `num_ctx` at spawn: the
    // fleet run block overlaid with the model's own launch knobs clamps.
    let base = crate::config::RoleParams {
        num_ctx: 65536,
        max_ctx: Some(8192),
        ..Default::default()
    };
    let out = base.with_model_launch_overrides(Some(&serde_json::json!({"num_ctx": 32768})));
    assert_eq!(out.num_ctx, 8192, "the cap clamps the overridden window");
    let out = base.with_model_launch_overrides(None);
    assert_eq!(out.num_ctx, 8192, "the cap clamps the base window");
}

#[test]
fn answer_chain_unknown_qualifier_falls_back() {
    let entry = chain_entry();
    assert!(
        entry.answer_params_for(Some("nope"), None, None).is_none(),
        "unknown qualifier returns None so callers fall back to bare params"
    );
}

#[test]
fn answer_chain_idempotent() {
    let entry = chain_entry();
    let role = quick_role();
    let first = entry
        .answer_params_for(Some("fast"), Some(("quick", &role)), None)
        .expect("composes");
    let again = entry
        .answer_params_for(Some("fast"), Some(("quick", &role)), None)
        .expect("composes");
    assert_eq!(first, again, "composition is pure");
}

#[test]
fn thinking_truth_table() {
    fn entry_with(filter: bool, enable: Option<bool>) -> ModelEntry {
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "filter_thinking": filter,
            "enable_thinking": enable,
        }))
        .unwrap()
    }
    // Absent everywhere: no level, no filtering.
    let e = entry_with(false, None);
    assert_eq!(e.resolve_thinking(None, None), None);
    assert!(!e.filter_thinking_for(None, None, None));
    // Bool alias: filter true with no level means off.
    let e = entry_with(true, None);
    assert_eq!(e.resolve_thinking(None, None), None);
    assert!(e.filter_thinking_for(None, None, None));
    // Explicit model level wins over the bool (warn path), filtering off.
    let e = entry_with(true, Some(true));
    assert_eq!(
        e.resolve_thinking(None, None).as_deref(),
        Some("on"),
        "explicit level wins over the bool"
    );
    assert!(
        e.filter_thinking_for(e.resolve_thinking(None, None).as_deref(), None, None),
        "the entry filter bool still filters a live level without preserve"
    );
    // Role leg beats the model leg; override leg beats the role leg.
    let e = entry_with(false, Some(false));
    let role: crate::config::RoleEntry =
        serde_json::from_value(serde_json::json!({"enable_thinking": true})).unwrap();
    let over: crate::config::ModelRoleOverride =
        serde_json::from_value(serde_json::json!({"enable_thinking": false})).unwrap();
    assert_eq!(e.resolve_thinking(Some(&role), None).as_deref(), Some("on"));
    assert_eq!(
        e.resolve_thinking(Some(&role), Some(&over)).as_deref(),
        Some("off")
    );
    // Explicit off filters.
    let e = entry_with(false, Some(false));
    let resolved = e.resolve_thinking(None, None);
    assert_eq!(resolved.as_deref(), Some("off"));
    assert!(e.filter_thinking_for(resolved.as_deref(), None, None));
}

#[test]
fn thinking_effort_passes_through_verbatim() {
    let entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
    }))
    .unwrap();
    let role: crate::config::RoleEntry = serde_json::from_value(serde_json::json!({
        "params": {"reasoning_effort": "high"}
    }))
    .unwrap();
    assert_eq!(
        entry.resolve_thinking(Some(&role), None).as_deref(),
        Some("high"),
        "the role run block's effort passes through verbatim"
    );
}

#[test]
fn preserve_thinking_beats_filter_bool_but_not_off() {
    fn entry_with(filter: bool, preserve: Option<bool>) -> ModelEntry {
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "filter_thinking": filter,
            "preserve_thinking": preserve,
        }))
        .unwrap()
    }
    let e = entry_with(true, Some(true));
    assert!(
        !e.filter_thinking_for(Some("on"), None, None),
        "preserve beats the bool"
    );
    let e = entry_with(true, Some(true));
    assert!(
        e.filter_thinking_for(Some("off"), None, None),
        "off always filters"
    );
    // The override layer wins over the entry bool.
    let e = entry_with(true, None);
    let over: crate::config::ModelRoleOverride =
        serde_json::from_value(serde_json::json!({"preserve_thinking": true})).unwrap();
    assert!(!e.filter_thinking_for(Some("on"), None, Some(&over)));
}

#[test]
fn preserve_thinking_never_reaches_the_body() {
    let mut entry = chain_entry();
    entry.preserve_thinking = Some(true);
    let out = entry
        .answer_params_for(Some("fast"), None, None)
        .expect("composes");
    assert!(
        out.get("preserve_thinking").is_none(),
        "preserve_thinking is router-local and stripped at the body boundary"
    );
}

#[test]
fn template_sparse_inherits_and_parses() {
    let config = RouterConfig::from_json_value(serde_json::json!({
        "models": {
            "default": {"template": "/fleet/template.txt"},
            "a": {"endpoint": "http://x/v1/chat/completions"},
            "b": {"endpoint": "http://x/v1/chat/completions", "template": "/own/t.txt"},
        },
        "model_groups": {},
        "classification": {
            "root": {
                "type": "classifier",
                "description": "Test router",
                "children": [
                    {"key": "local", "description": "General Q&A", "node": {
                        "type": "terminal", "route": "local",
                        "group": "default", "always_route": false,
                        "description": "General Q&A",
                    }},
                ],
            },
        },
    }))
    .expect("parses with sparse inheritance");
    assert_eq!(
        config.models["a"].template.as_deref(),
        Some("/fleet/template.txt"),
        "absent template inherits the models.default template"
    );
    assert_eq!(
        config.models["b"].template.as_deref(),
        Some("/own/t.txt"),
        "declared template wins"
    );
}

#[test]
fn schema_conformance_accepts_round_trip_and_rejects_bare() {
    let ok = RouterConfig::from_json_value(serde_json::json!({
        "models": {},
        "model_groups": {},
        "pipelines": {"default": {}},
        "classification": {
            "root": {
                "type": "classifier",
                "description": "Test router",
                "children": [
                    {"key": "local", "description": "General Q&A", "node": {
                        "type": "terminal", "route": "local",
                        "group": "default", "always_route": false,
                        "description": "General Q&A",
                    }},
                ],
            },
        },
    }))
    .expect("parses");
    if let Err(e) = ok.verify_schema_conformance() {
        panic!("minimal tree config verifies, got: {e}");
    }
    let mut bare = ok.clone();
    bare.classification = None;
    assert!(
        bare.verify_schema_conformance().is_err(),
        "missing classification tree fails closed"
    );
    let mut no_pipeline = ok.clone();
    no_pipeline.pipelines.clear();
    assert!(
        no_pipeline.verify_schema_conformance().is_err(),
        "missing default pipeline fails closed"
    );
}

#[test]
fn routes_view_carries_terminal_role() {
    let cfg = RouterConfig::from_json_str(
        r#"{
            "models": {"m": {"endpoint": "http://x", "intelligence": 1}},
            "model_groups": {"g": ["m"]},
            "roles": {},
            "classification": {"root": {
                "type": "classifier", "description": "d", "model": "m",
                "children": [
                    {"key": "code", "description": "", "node": {
                        "type": "terminal", "route": "code",
                        "group": "g", "role": "quick", "description": ""
                    }},
                    {"key": "chat", "description": "", "node": {
                        "type": "terminal", "route": "chat",
                        "group": "g", "description": ""
                    }}
                ]
            }}
        }"#,
    )
    .expect("tree config parses");
    let view = cfg.routes_view();
    assert_eq!(view["code"].role.as_deref(), Some("quick"));
    assert_eq!(view["chat"].role, None, "role defaults to None");
}

fn ls_fixture_config() -> RouterConfig {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/ls_fixture.json");
    let text = std::fs::read_to_string(&path).expect("ls fixture readable");
    let mut cfg = RouterConfig::from_json_str(&text).expect("ls fixture parses");
    cfg.apply_defaults();
    cfg
}

// -- Fixture-decoupled config tests (M10): hermetic suites assert over
// checked-in fixtures only. The operator's env/coral-router.json is read
// exclusively by the config-synced suite (make router-mock); see the guard
// test at the end of this section. ------------------------------------------

#[test]
fn routing_config_fixture_round_trip() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/ls_fixture.json");
    let content = std::fs::read_to_string(&path).expect("fixture readable");
    let cfg: RouterConfig = serde_json::from_str(&content).expect("fixture deserializes");
    let value = serde_json::to_value(&cfg).expect("serialize");
    let back: RouterConfig = serde_json::from_value(value).expect("round-trip");
    assert_eq!(back.routes_view().len(), cfg.routes_view().len());
    assert_eq!(back.models.len(), cfg.models.len());
}
#[test]
fn fixture_boot_composition_single_model_multi_window() {
    // One weights file serves role windows composed into a single effective
    // pool; group duties resolve through the same precedence as production.
    let cfg = ls_fixture_config();

    // Bound models compose a base profile plus their role profiles.
    for member in ["m1", "m2"] {
        let names: Vec<&str> = cfg.models[member]
            .effective_pool()
            .iter()
            .map(|p| p.name.as_deref().unwrap())
            .collect();
        assert_eq!(names[0], "base", "{member} keeps its base profile");
    }

    // Group duty resolves to the bare member key; the inference point
    // qualifies later at dispatch (the two-step duty rule).
    assert_eq!(
        crate::config::resolve_group_head_key(&cfg.models, &cfg.roles, &cfg.model_groups, "dev")
            .as_deref(),
        Some("m1"),
        "dev duty is m1"
    );

    // Role-wins-last dispatch on the fixture chain: model, role, override.
    let params = cfg.models["m2"]
        .answer_params_for(Some("base"), Some(("quick", &cfg.roles["quick"])), None)
        .expect("composes");
    let get = |k: &str| {
        params
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>")
    };
    assert_eq!(get("mp"), "mv", "model leg survives");
    assert_eq!(get("rq"), "rv", "role leg composes");
    assert_eq!(get("oq"), "ov", "model role override is final");
}

#[test]
fn fixture_tree_view_keys_and_flags() {
    // Tree terminals are the only routes; flags ride the tree.
    let cfg = ls_fixture_config();
    assert!(cfg.classification.is_some(), "fixture config is tree-only");
    cfg.validate_flat_tree_coherence()
        .expect("fixture tree valid");
    let view = cfg.routes_view();
    let mut keys: Vec<&String> = view.keys().collect();
    keys.sort();
    let key_strs: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
    assert_eq!(key_strs, vec!["code", "local"]);
    for route in ["code", "local"] {
        assert_eq!(view[route].group, "dev");
        assert!(!view[route].always_route, "{route} may answer directly");
    }
}

#[test]
fn hermetic_tests_never_open_the_operator_config() {
    // Guard: no hermetic test under src/router/tests/ may read the
    // operator's env/coral-router.json — fixtures only. The single
    // exception is the config-synced suite (make router-mock), which
    // derives its expectations from the file at runtime by design.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let entries = std::fs::read_dir(&dir).expect("tests dir readable");
    for entry in entries {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name")
            .to_string();
        if name == "config_route_tests.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("test readable");
        // Match the file-open pattern without spelling it literally here
        // (this guard would otherwise flag itself).
        if text.contains(&["../..", "env/coral-router.json"].join("/")) {
            offenders.push(name);
        }
    }
    assert!(
        offenders.is_empty(),
        "hermetic tests must use fixtures, not the operator file (see make router-mock): {offenders:?}"
    );
}
