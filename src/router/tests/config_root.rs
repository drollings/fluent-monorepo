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

// -- Async overlay configuration (ROADMAP_20260827_ORT §6) -----------

#[test]
fn overlay_config_defaults_are_fail_open() {
    // Serde defaults apply on deserialization (an absent field gets the
    // default); `OverlayConfig::default()` is the empty struct.
    let cfg: OverlayConfig =
        serde_json::from_str(r#"{"entity_link_enabled": true}"#).unwrap();
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
    let plain: OverlayConfig =
        serde_json::from_str(r#"{"entity_link_enabled": true}"#).unwrap();
    assert!(plain.arc_ready.is_none(), "absent arc_ready → None");
    assert!(OverlayConfig::default().arc_ready.is_none());

    // A present `arc_ready` block defaults every knob to off/absent.
    let present: OverlayConfig = serde_json::from_str(
        r#"{"entity_link_enabled": true, "arc_ready": {}}"#,
    )
    .unwrap();
    let arc = present.arc_ready.expect("arc_ready present");
    assert!(!arc.enabled, "enabled defaults off");
    assert!(!arc.nlp, "nlp defaults off");
    assert!(arc.llm_model.is_none(), "llm_model absent by default");
    assert!(arc.embedding_model.is_none(), "embedding_model absent by default");
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
    assert!(routing.score_matrix.is_none(), "root matrix is gone; pipeline-level matrices flow via params");
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
    assert_eq!(params.target_match, crate::config::builder::TargetMatchMode::SelfAssess);
    assert_eq!(
        params.target_match_timeout_ms,
        fluent_llm::constants::DEFAULT_TOTAL_TIMEOUT_MS
    );
}

#[test]
fn pipeline_params_target_match_parses_both_variants() {
    let self_assess: crate::config::builder::TargetMatchMode =
        serde_json::from_str(r#""self_assess""#).expect("self_assess parses");
    assert_eq!(self_assess, crate::config::builder::TargetMatchMode::SelfAssess);

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
    assert_eq!(cfg.pipelines["default"].target_match, crate::config::builder::TargetMatchMode::Static);
    assert_eq!(cfg.pipelines["default"].target_match_timeout_ms, 12345);

    let serialized = serde_json::to_string(&cfg).unwrap();
    let back: RouterConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(back.pipelines["default"].target_match, crate::config::builder::TargetMatchMode::Static);
    assert_eq!(back.pipelines["default"].target_match_timeout_ms, 12345);
}

// -- Instance-pool declaration -------------------------------------

fn profile_json(name: &str, count: u32, group: &str, num_ctx: u64) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "count": count,
        "group": group,
        "num_ctx": num_ctx,
    })
}

/// Parse a `roles`-table fixture. Pools live on roles now (`roles.<role>.instances`).
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
    entry.effective_profiles = Some(crate::config::materialize_effective_pool(key, roles));
}

/// A bare model entry: models never name roles, so every entry parses the
/// same way — bindings live on the roles table. An entry listed in no
/// role's `models` map inherits the fleet `default` pool; tests that need a
/// bound model add `"models": {"<key>": {<binding>}}` to the role-table
/// fixture and materialize under the same key.
fn bare_entry() -> ModelEntry {
    serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
        "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
        "tok_s": 1,
    }))
    .unwrap()
}

#[test]
fn instances_count_expansion_names_siblings_in_shared_group() {
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": {
                "swarm": profile_json("swarm", 3, "swarm", 16384),
                "ledger": { "num_ctx": 131072, "pinned": true, "default": true }
            },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);

    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 4);
    // Profiles are emitted in sorted map-key order: ledger < swarm.
    assert_eq!(profiles[0].name.as_deref(), Some("ledger"));
    assert_eq!(profiles[0].group.as_deref(), Some("ledger"));
    assert!(profiles[0].pinned);
    assert!(profiles[0].default);
    // count: 3 -> `<key>-0` .. `<key>-2` in the shared group.
    assert_eq!(profiles[1].name.as_deref(), Some("swarm-0"));
    assert_eq!(profiles[1].group.as_deref(), Some("swarm"));
    assert_eq!(profiles[2].name.as_deref(), Some("swarm-1"));
    assert_eq!(profiles[3].name.as_deref(), Some("swarm-2"));
    assert_eq!(profiles[3].group.as_deref(), Some("swarm"));
    assert_eq!(profiles[3].num_ctx, 16384);
}

#[test]
fn instances_single_profile_defaults_name_to_map_key() {
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": {
                "scratch": { "num_ctx": 131072, "sleep_idle_seconds": 30 }
            },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name.as_deref(), Some("scratch"));
    assert_eq!(profiles[0].group.as_deref(), Some("scratch"));
    assert_eq!(profiles[0].sleep_idle_seconds, Some(30));
    assert_eq!(profiles[0].count, 1);
}

#[test]
fn unknown_sessions_key_fails_at_parse() {
    // The pre-R7 `sessions` pool key no longer exists: like the top level,
    // `ModelEntry` rejects unknown fields, so an old config carrying a pool
    // fails loudly at parse instead of silently losing it — pools live in
    // `roles.<role>.instances`.
    let err = serde_json::from_value::<ModelEntry>(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
        "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
        "tok_s": 1,
        "sessions": { "ctx16384": { "num_ctx": 16384 } }
    }))
    .expect_err("retired sessions key must not parse");
    assert!(err.to_string().contains("sessions"), "got: {err}");
}

#[test]
fn no_instances_yields_empty_profile_list() {
    let entry = bare_entry();
    assert!(entry.effective_pool().is_empty(), "pre-boot pool is empty");
    let mut entry = bare_entry();
    materialize("m", &mut entry, &role_table(serde_json::json!({})));
    assert!(
        entry.effective_pool().is_empty(),
        "no roles and no selection materialize nothing"
    );
}

#[test]
fn warm_alias_maps_to_no_sleep() {
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": { "swarm": { "num_ctx": 16384, "warm": true } },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert!(profiles[0].no_sleep);
}

// -- M3: per-profile `max_ctx` context-size cap ---------------------

#[test]
fn instance_profile_max_ctx_absent_defaults_to_none() {
    // An absent `max_ctx` (the default) deserializes to `None` — an
    // existing config without the new key is byte-identical.
    let profile: InstanceProfile =
        serde_json::from_value(profile_json("swarm", 1, "swarm", 16384)).unwrap();
    assert!(profile.max_ctx.is_none());
}

#[test]
fn instance_profile_max_ctx_round_trips() {
    let profile: InstanceProfile = serde_json::from_value(serde_json::json!({
        "name": "swarm",
        "count": 1,
        "group": "swarm",
        "num_ctx": 16384,
        "max_ctx": 8192,
    }))
    .unwrap();
    assert_eq!(profile.max_ctx, Some(8192));
    let back: InstanceProfile =
        serde_json::from_str(&serde_json::to_string(&profile).unwrap()).unwrap();
    assert_eq!(back.max_ctx, Some(8192));
    assert_eq!(back.num_ctx, 16384);
}

#[test]
fn instance_profiles_clamps_num_ctx_to_max_ctx() {
    // A profile whose `max_ctx` cap sits below its `num_ctx` is clamped
    // down at materialization: the materialized context window is the cap.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": { "swarm": { "num_ctx": 65536, "max_ctx": 8192 } },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].num_ctx, 8192, "clamped to max_ctx");
    assert_eq!(profiles[0].max_ctx, Some(8192));
}

#[test]
fn instance_profiles_max_ctx_absent_is_noop() {
    // The clamp is a no-op when `max_ctx` is absent (the default): the
    // materialized `num_ctx` is untouched.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": { "swarm": { "num_ctx": 16384 } },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles[0].num_ctx, 16384);
    assert!(profiles[0].max_ctx.is_none());
}

#[test]
fn instance_profiles_cap_above_num_ctx_is_noop() {
    // A cap at/above `num_ctx` never inflates the window — the declared
    // `num_ctx` is the upper bound.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": { "swarm": { "num_ctx": 16384, "max_ctx": 32768 } },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles[0].num_ctx, 16384, "cap above num_ctx is a no-op");
}

// -- Fleet-wide `roles.default.instances` inheritance ---------------------

/// Parse a role-pool profile map (`roles.<role>.instances` shape).
fn fleet_defaults(
    json: serde_json::Value,
) -> std::collections::HashMap<String, InstanceProfile> {
    serde_json::from_value(json).unwrap()
}

/// Wrap a pool map as the `default` role's table (the fleet-inherit source).
fn default_role_with(
    pool: std::collections::HashMap<String, InstanceProfile>,
) -> std::collections::HashMap<String, crate::config::RoleEntry> {
    std::collections::HashMap::from([(
        "default".to_string(),
        crate::config::RoleEntry {
            context: crate::config::RoleContext::default(),
            concurrency: None,
            params: crate::config::RoleParams::default(),
            instances: pool,
            models: std::collections::HashMap::new(),
        },
    )])
}

#[test]
fn fleet_default_key_inherited_when_entry_declares_none() {
    // An entry serving no role and declaring no selection inherits the
    // `default` role's pool (the fleet-inherit fallback, same code path).
    let mut entry = bare_entry();
    assert!(entry.effective_pool().is_empty());
    let roles = default_role_with(fleet_defaults(serde_json::json!({
        "scratch": { "num_ctx": 4096 }
    })));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name.as_deref(), Some("scratch"));
    assert_eq!(profiles[0].group.as_deref(), Some("scratch"));
    assert_eq!(profiles[0].num_ctx, 4096);
}

#[test]
fn cross_role_profile_collision_first_role_wins_whole() {
    // Same profile name in two contributing roles: the first role's profile
    // wins whole — no field-level merge fuses the two declarations.
    // Membership lives on the model: the entry binds both roles.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "aaa": {
            "instances": {
                "scratch": { "num_ctx": 4096, "pinned": true, "default": true,
                             "params": { "temperature": 0.9 } },
                "ledger": { "num_ctx": 131072, "pinned": true, "default": true }
            },
            "models": {"m": {}}
        },
        "zzz": {
            "instances": {
                "scratch": { "num_ctx": 16384 }
            },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 2);
    // Sorted map-key order: ledger < scratch.
    let ledger = &profiles[0];
    assert_eq!(ledger.name.as_deref(), Some("ledger"));
    assert_eq!(ledger.num_ctx, 131072);
    let scratch = &profiles[1];
    assert_eq!(scratch.name.as_deref(), Some("scratch"));
    assert_eq!(scratch.num_ctx, 4096, "first role wins");
    assert!(scratch.pinned, "winning profile keeps its flags");
    assert!(
        scratch.params.is_some(),
        "winning profile keeps its params"
    );
}

#[test]
fn both_roles_default_collision_first_wins() {
    // Two `default: true` flags from two contributing roles must never fuse
    // silently: exactly one profile answers the default lookup — the
    // lexicographically-first key in deterministic map order.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "aaa": {
            "instances": { "alpha": { "num_ctx": 4096, "default": true } },
            "models": {"m": {}}
        },
        "zzz": {
            "instances": { "beta": { "num_ctx": 8192, "default": true } },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    let defaulted: Vec<&str> = profiles
        .iter()
        .filter(|p| p.default)
        .filter_map(|p| p.name.as_deref())
        .collect();
    assert_eq!(defaulted.len(), 2, "both flags survive the merge");
    let winner = profiles
        .iter()
        .find(|p| p.default)
        .and_then(|p| p.name.clone())
        .expect("a default exists");
    assert_eq!(winner.as_str(), "alpha", "first in map order wins");
}

#[test]
fn inherited_profile_expands_count_and_clamps_max_ctx() {
    // `count` sibling expansion and the `max_ctx` clamp run on the composed
    // map through the same code path — never a fork.
    let mut entry = bare_entry();
    let roles = default_role_with(fleet_defaults(serde_json::json!({
        "worker": { "num_ctx": 65536, "max_ctx": 16384, "count": 3 }
    })));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 3);
    for (i, profile) in profiles.iter().enumerate() {
        assert_eq!(
            profile.name.as_deref(),
            Some(format!("worker-{i}").as_str())
        );
        assert_eq!(profile.group.as_deref(), Some("worker"));
        assert_eq!(profile.num_ctx, 16384, "inherited clamp applies");
    }
}

#[test]
fn role_selection_narrows_pool_and_composes_sampling() {
    // The role pools compose into each model's effective pool: one model
    // selects the fleet `base` profile (sampling composes role-base under
    // the profile knobs), and another selects its capped override (its own
    // group, role base composed in). Synthetic roles — never the shipped
    // file, which is an operator artifact, not a test oracle.
    let roles = role_table(serde_json::json!({
        "fleet": {
            "params": {"temperature": 0.1},
            "instances": {
                "base": {
                    "num_ctx": 8192, "max_ctx": 262144, "default": true,
                    "params": {"temperature": 0.6}
                },
                "scout": {"num_ctx": 4096, "max_ctx": 32768}
            },
            "models": {
                "base-svc": {"select": "base"},
                "scout-svc": {"select": "scout"}
            }
        }
    }));
    let mut base = bare_entry();
    materialize("base-svc", &mut base, &roles);
    let profiles = base.effective_pool();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name.as_deref(), Some("base"));
    assert_eq!(profiles[0].num_ctx, 8192);
    assert_eq!(profiles[0].max_ctx, Some(262144));
    assert!(profiles[0].default, "selection designates the dispatch point");
    assert_eq!(
        profiles[0]
            .params
            .as_ref()
            .and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.6)),
        "profile knobs win over the role base"
    );
    // The override narrows to its own profile with the role base composed.
    let mut scout = bare_entry();
    materialize("scout-svc", &mut scout, &roles);
    let profiles = scout.effective_pool();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name.as_deref(), Some("scout"));
    assert_eq!(profiles[0].group.as_deref(), Some("scout"));
    assert_eq!(profiles[0].max_ctx, Some(32768), "distinct cap survives");
    assert_eq!(profiles[0].num_ctx, 4096);
    assert!(profiles[0].default, "selection designates the dispatch point");
    assert_eq!(
        profiles[0]
            .params
            .as_ref()
            .and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.1)),
        "role base reaches the selected override"
    );
}
#[test]
fn role_head_key_resolves_bound_models() {
    // Membership lives on the role side: a role fans out to the models it
    // names (sorted model-key order); unknown keys pass through.
    let roles = role_table(serde_json::json!({
        "default": {"models": {"svc": {}}},
        "spare": {"models": {"svc": {}}},
        "lonely": {}
    }));
    let mut models: std::collections::HashMap<String, ModelEntry> =
        std::collections::HashMap::new();
    models.insert("svc".to_string(), bare_entry());
    assert_eq!(
        crate::config::role_head_key(&models, &roles, "spare", false).as_deref(),
        Some("svc"),
        "role resolves to its bound model"
    );
    assert_eq!(
        crate::config::role_head_key(&models, &roles, "default", false).as_deref(),
        Some("svc"),
        "the default role resolves to its bound model"
    );
    assert_eq!(
        crate::config::role_head_key(&models, &roles, "ghost", false).as_deref(),
        Some("ghost"),
        "unknown keys pass through"
    );
    assert!(
        crate::config::role_head_key(&models, &roles, "lonely", false).is_none(),
        "declared role with no bound models stays unresolvable"
    );
    // The classifier key honors the same membership.
    let mut cfg_models: std::collections::HashMap<String, ModelEntry> =
        std::collections::HashMap::new();
    cfg_models.insert("svc".to_string(), bare_entry());
    let cfg = RouterConfig {
        roles: role_table(serde_json::json!({
            "default": {"models": {"svc": {}}},
            "classifier": {"models": {"svc": {}}}
        })),
        models: cfg_models,
        ..RouterConfig::default()
    };
    assert_eq!(
        cfg.classifier_role_key().as_deref(),
        Some("svc"),
        "classifier resolves to its bound model"
    );
}

#[test]
fn non_object_params_warn_and_degrade() {
    // `params` is valid on pool profiles, selections, and model entries
    // alike — but only as JSON objects. (Role run blocks carry no nested
    // `params` object: sampling keys are flattened into `roles.<role>.params`
    // itself.) Non-object values would vanish silently inside
    // `overlay_params`, so boot warns loudly over each one (fail-open: the
    // merge itself is unchanged).
    use crate::test_support::capture_logs;
    let mut cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {
            "fleet": {
                "params": {"temperature": 0.1},
                "instances": {
                    "p": {"num_ctx": 8192, "params": [1, 2]}
                },
                "models": {"m": {"select": "p", "params": "x"}}
            }
        },
        "models": {
            "m": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 1,
                "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                "tok_s": 1,
                "params": 42
            }
        }
    }))
    .expect("junk params still deserialize");
    let (_, logs) = capture_logs(|| cfg.apply_defaults());
    let joined = logs.join("\n");
    for path in [
        "roles.fleet.instances.p.params",
        "models.m.params",
        "roles.fleet.models.m.params",
    ] {
        assert!(joined.contains(path), "warns over {path}, logs:\n{joined}");
    }
    // Fail-open: junk sides contribute nothing, boot still composes — the
    // valid role sampling base survives underneath.
    let profiles = cfg.models["m"].effective_pool();
    assert_eq!(profiles.len(), 1);
    assert_eq!(
        profiles[0].params,
        Some(serde_json::json!({"temperature": 0.1}))
    );
}

#[test]
fn role_binding_to_unknown_model_warns_and_is_ignored() {
    // A role-side binding naming a model with no `models` entry can never
    // contribute to a pool: boot warns loudly (fail-open) and the binding
    // is ignored — membership and pools observe only declared models.
    use crate::test_support::capture_logs;
    let mut cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {
            "work": {
                "instances": {
                    "scratch": {"num_ctx": 8192}
                },
                "models": {
                    "m": {},
                    "ghost": {"select": "scratch"}
                }
            }
        },
        "models": {
            "m": {
                "endpoint": "http://x/v1/chat/completions",
                "intelligence": 1,
                "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                "tok_s": 1
            }
        }
    }))
    .expect("dangling binding parses");
    let (_, logs) = capture_logs(|| cfg.apply_defaults());
    let joined = logs.join("\n");
    assert!(
        joined.contains("ghost"),
        "warns over the unknown bound model, logs:\n{joined}"
    );
    assert_eq!(
        crate::config::models_serving_role(&cfg.roles, &cfg.models, "work"),
        vec!["m"],
        "unknown names never join membership"
    );
    assert_eq!(
        cfg.models["m"].effective_pool().len(),
        1,
        "declared model's pool composes despite the dangling sibling"
    );
}

#[test]
fn legacy_nested_role_params_hoist_flat_at_parse() {
    // The retired `params`-inside-`params` shape is not a composition layer:
    // `from_json_str` hoists the nested sampling keys into the enclosing run
    // block (enclosing keys win on collision) and drops the nested object.
    let cfg = RouterConfig::from_json_str(
        r#"{
            "roles": {
                "default": {
                    "params": {
                        "num_ctx": 8192,
                        "params": {"temperature": 0.1, "top_k": 40}
                    }
                },
                "spare": {
                    "params": {"batch_size": 1024, "params": {"top_k": 10}}
                }
            }
        }"#,
    )
    .expect("legacy nested roles parse");
    let fleet = cfg.default_role_params();
    assert_eq!(
        fleet.sampling_value().as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.1)),
        "nested sampling key hoisted into the run block"
    );
    let spare = &cfg.roles["spare"].params;
    assert_eq!(spare.batch_size, 1024, "declared scalar wins");
    assert_eq!(
        spare.sampling_value().as_ref().and_then(|p| p.get("top_k")),
        Some(&serde_json::json!(10)),
        "declared nested key wins over the fleet value"
    );
    assert_eq!(
        spare.sampling_value().as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.1)),
        "fleet sampling key inherited"
    );
    assert!(
        !spare.sampling.contains_key("params"),
        "no params-inside-params survives typing"
    );
}

#[test]
fn fleet_run_block_sparse_merges_over_roles_at_parse() {
    // Roles declare only deltas: absent run-block keys inherit the fleet
    // block at parse (before typing), declared keys win per key, shallowly.
    // Sampling keys are flat members of the run block itself — never a
    // nested `params` object.
    let cfg = RouterConfig::from_json_str(
        r#"{
            "roles": {
                "default": {
                    "params": {
                        "num_ctx": 8192, "batch_size": 512,
                        "temperature": 0.1, "top_k": 40
                    }
                },
                "spare": {
                    "params": {"batch_size": 1024, "top_k": 10}
                },
                "bare": {}
            }
        }"#,
    )
    .expect("sparse roles parse");
    let spare = &cfg.roles["spare"].params;
    assert_eq!(spare.num_ctx, 8192, "absent scalar inherits fleet");
    assert_eq!(spare.batch_size, 1024, "declared scalar wins");
    assert_eq!(
        spare.sampling_value().as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.1)),
        "fleet sampling key inherited"
    );
    assert_eq!(
        spare.sampling_value().as_ref().and_then(|p| p.get("top_k")),
        Some(&serde_json::json!(10)),
        "declared sampling key wins"
    );
    let bare = &cfg.roles["bare"].params;
    assert_eq!(
        bare.num_ctx, 8192,
        "role without a params block inherits whole"
    );
    assert_eq!(bare.batch_size, 512);
    assert_eq!(
        bare.sampling_value().as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.1))
    );
    // Direct serde (no constructor) skips the leg — struct defaults apply.
    let raw: RouterConfig = serde_json::from_str(r#"{"roles": {"spare": {}}}"#).unwrap();
    assert_eq!(
        raw.roles["spare"].params.num_ctx, 16384,
        "no fleet leg without the constructor"
    );
}

#[test]
fn models_default_template_sparse_fills_sibling_entries_at_parse() {
    // `models.default` is the fleet template: every sibling entry inherits
    // its absent top-level keys (and its absent `params` sampling keys,
    // shallowly) at parse; declared keys win at both levels.
    let cfg = RouterConfig::from_json_str(
        r#"{
            "models": {
                "default": {
                    "intelligence": 1,
                    "cost_input": 0.000001,
                    "tok_s": 20,
                    "total_timeout_ms": 50000,
                    "embedding": "/models/base-embed.gguf",
                    "params": {"num_ctx": 8192, "temperature": 0.1}
                },
                "bare": {
                    "endpoint": "http://x/v1/chat/completions"
                },
                "tuned": {
                    "endpoint": "http://y/v1/chat/completions",
                    "intelligence": 5,
                    "tok_s": 42,
                    "params": {"temperature": 0.9}
                }
            }
        }"#,
    )
    .expect("template parses");
    let bare = &cfg.models["bare"];
    assert_eq!(bare.intelligence, 1, "absent scalar inherits template");
    assert_eq!(bare.tok_s, 20.0, "absent tok_s inherits template");
    assert_eq!(bare.total_timeout_ms, 50000);
    assert_eq!(
        bare.embedding.as_deref(),
        Some("/models/base-embed.gguf"),
        "absent embedding inherits template"
    );
    assert_eq!(
        bare.params.as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.1)),
        "absent sampling key inherits template"
    );
    let tuned = &cfg.models["tuned"];
    assert_eq!(tuned.intelligence, 5, "declared scalar wins");
    assert_eq!(tuned.tok_s, 42.0, "declared tok_s wins");
    assert_eq!(
        tuned.params.as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.9)),
        "declared sampling key wins"
    );
    assert_eq!(
        tuned.params.as_ref().and_then(|p| p.get("num_ctx")),
        Some(&serde_json::json!(8192)),
        "template fills the sibling's missing sampling keys"
    );
    // Direct serde (no constructor) skips the leg — struct defaults apply.
    let raw: RouterConfig = serde_json::from_value(serde_json::json!({
        "models": {"m": {"endpoint": "http://x/v1/chat/completions"}}
    }))
    .unwrap();
    assert_eq!(raw.models["m"].tok_s, 20.0, "struct default without the leg");
    assert!(raw.models["m"].embedding.is_none());
}

#[test]
fn models_default_template_feeds_fleet_run_block() {
    // The models template is the fleet base even when `roles.default` is an
    // empty `{}`: its launch knobs fill the fleet run block's absent keys at
    // parse, so the supervisor's spawn defaults come from one place.
    let cfg = RouterConfig::from_json_str(
        r#"{
            "models": {
                "default": {
                    "params": {"num_ctx": 8192, "batch_size": 2048, "temperature": 0.1}
                }
            },
            "roles": {"default": {}}
        }"#,
    )
    .expect("template-fed fleet parses");
    let fleet = cfg.default_role_params();
    assert_eq!(fleet.num_ctx, 8192, "fleet inherits template launch knob");
    assert_eq!(fleet.batch_size, 2048);
    assert_eq!(
        fleet.sampling_value().as_ref().and_then(|p| p.get("temperature")),
        Some(&serde_json::json!(0.1)),
        "fleet inherits template sampling key"
    );
}

#[test]
fn group_duties_move_together_on_rebind() {
    // Classifier, ledger, and chart-selector duties all name groups: one
    // rebinding moves every duty together. The classifier duty is driven by
    // the classification tree's root group (pipelines carry no group key);
    // ledger and chart-selector duties name groups directly.
    fn duty_config(group: &str) -> RouterConfig {
        serde_json::from_value(serde_json::json!({
            "roles": {
                "default": {"models": {"svc-main": {}}},
                "code": {"models": {"code": {}}}
            },
            "models": {
                "svc-main": {
                    "endpoint": "http://x/v1/chat/completions",
                    "intelligence": 1,
                    "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                    "tok_s": 1
                },
                "code": {
                    "endpoint": "http://x/v1/chat/completions",
                    "intelligence": 4,
                    "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
                    "tok_s": 1
                }
            },
            "model_groups": {
                "default": ["default"],
                "code": ["code"]
            },
            "pipelines": {
                "default": {"classifier": true}
            },
            "classification": {"root": {
                "type": "classifier",
                "description": "duty router",
                "model_group": group,
                "children": [
                    {"key": "local", "description": "general",
                     "node": {"type": "terminal", "route": "local", "group": "default"}}
                ]
            }},
            "ledger": {"path": "data/l.sqlite", "group": group},
            "charts": {"selector_group": group}
        }))
        .expect("duty config parses")
    }
    for (group, head) in [("code", "code"), ("default", "svc-main")] {
        let cfg = duty_config(group);
        let params = cfg.pipelines["default"].clone();
        assert_eq!(
            crate::config::builder::resolve_classifier_model_key(&cfg, &params).as_deref(),
            Some(head),
            "classifier follows group {group}"
        );
        assert_eq!(
            cfg.ledger_head_key().as_deref(),
            Some(head),
            "ledger follows group {group}"
        );
        assert_eq!(
            cfg.chart_selector_key().as_deref(),
            Some(head),
            "selector follows group {group}"
        );
    }
}

#[test]
fn missing_endpoint_warns_only_for_external_models() {
    // Managed models (weights/hf_repo) omit `endpoint` — the supervisor
    // assigns it. Only external models without one warn at boot.
    use crate::test_support::capture_logs;
    fn entry(extra: serde_json::Value) -> ModelEntry {
        let mut base = serde_json::json!({
            "intelligence": 1,
            "cost_input": 0.0, "cost_output": 0.0, "cost_cached_read": 0.0,
            "tok_s": 1
        });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }
    let mut cfg = RouterConfig::default();
    cfg.models.insert(
        "managed".to_string(),
        entry(serde_json::json!({"weights": "/m.gguf"})),
    );
    cfg.models
        .insert("external".to_string(), entry(serde_json::json!({})));
    let (_, logs) = capture_logs(|| cfg.apply_defaults());
    let joined = logs.join("\n");
    assert!(
        joined.contains("external"),
        "external model without endpoint warns, logs:\n{joined}"
    );
    assert!(
        !joined.contains("managed"),
        "managed model without endpoint stays silent, logs:\n{joined}"
    );
}

// -- Single inference-point resolver -------------------------------
// (Covers the removed `pool_qualifier` / `default_dispatch_qualifier` pair:
// the work-pool intent now rides a role's instance point, the entry default
// is one step of the single precedence. The shipped-config equivalence is
// pinned by the role golden.)

/// The reference swarm entry: bare (models never name roles) — the count=3
/// non-default `swarm` work pool, the pinned `default: true` ledger, and the
/// non-default scratch profile live in the role pool below, which binds it.
fn reference_swarm_entry() -> ModelEntry {
    serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "name": "abiray/lfm2.5-2.6b-heretic-abliterated",
        "intelligence": 2,
        "cost_input": 1e-06, "cost_output": 6e-06, "cost_cached_read": 4e-07,
        "tok_s": 8
    }))
    .expect("reference swarm entry parses")
}

/// The reference swarm pool on the `work` role (roles carry no qualifier:
/// the point always resolves from the route-selected model's entry default).
/// The role binds the `swarm` model — that binding is what makes the pool
/// contribute to the entry.
fn swarm_pool_roles() -> std::collections::HashMap<String, crate::config::RoleEntry> {
    serde_json::from_value(serde_json::json!({
        "work": {
            "instances": {
                "swarm": profile_json("swarm", 3, "swarm", 16384),
                "ledger": { "num_ctx": 131072, "pinned": true, "default": true },
                "scratch": { "num_ctx": 131072, "sleep_idle_seconds": 30 }
            },
            "models": {"swarm": {}}
        }
    }))
    .expect("swarm pool roles parse")
}

fn reference_swarm_models(
    roles: &std::collections::HashMap<String, crate::config::RoleEntry>,
) -> std::collections::HashMap<String, ModelEntry> {
    let mut entry = reference_swarm_entry();
    materialize("swarm", &mut entry, roles);
    std::collections::HashMap::from([("swarm".to_string(), entry)])
}

fn work_role() -> std::collections::HashMap<String, crate::config::RoleEntry> {
    swarm_pool_roles()
}

fn no_roles() -> std::collections::HashMap<String, crate::config::RoleEntry> {
    std::collections::HashMap::new()
}

#[test]
fn inference_point_bare_key_answers_entry_default() {
    // One resolver, one answer for a bare key: the entry default (the
    // `default: true` profile's group, else the single shared group, else
    // bare). The pool answers without a role instance point — that intent
    // rides a role only when the role names it.
    let roles = swarm_pool_roles();
    let models = reference_swarm_models(&roles);
    assert_eq!(
        resolve_inference_point(&models, &roles, "swarm", None).as_deref(),
        Some("ledger"),
        "bare key serves the default instance"
    );
}

#[test]
fn inference_point_role_carries_no_qualifier() {
    // Roles carry no qualifier of their own: the point resolves from the
    // route-selected model. The work pool is reachable through the bound
    // model's bare key (its entry default), never through the bare role.
    let roles = work_role();
    let models = reference_swarm_models(&roles);
    assert_eq!(
        resolve_inference_point(&models, &roles, "work", None),
        None,
        "a bare role carries no qualifier"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "swarm", None).as_deref(),
        Some("ledger"),
        "the bound model's bare key serves its default"
    );
}

#[test]
fn inference_point_entry_default_shapes() {
    // The entry-default step across profile shapes (the kept rule, now one
    // step of the single precedence).
    let mut ledger_only = bare_entry();
    let ledger_roles = role_table(serde_json::json!({
        "work": {
            "instances": { "ledger": { "num_ctx": 131072, "default": true } },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut ledger_only, &ledger_roles);
    let models = std::collections::HashMap::from([("m".to_string(), ledger_only)]);
    assert_eq!(
        resolve_inference_point(&models, &ledger_roles, "m", None).as_deref(),
        Some("ledger")
    );

    let mut shared = bare_entry();
    let shared_roles = role_table(serde_json::json!({
        "work": {
            "instances": {
                "a": { "num_ctx": 8192, "group": "shared" },
                "b": { "num_ctx": 8192, "group": "shared" }
            },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut shared, &shared_roles);
    let models = std::collections::HashMap::from([("m".to_string(), shared)]);
    assert_eq!(
        resolve_inference_point(&models, &shared_roles, "m", None).as_deref(),
        Some("shared")
    );

    let bare = bare_entry();
    let models = std::collections::HashMap::from([("m".to_string(), bare)]);
    assert_eq!(
        resolve_inference_point(&models, &no_roles(), "m", None),
        None,
        "no pool stays bare"
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
        fleet.sampling_value().as_ref().and_then(|p| p.get("temperature")),
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
fn orchestrator_section_default_absent() {
    // Existing ledger configs without an `orchestrator` section keep the
    // coordinator disabled (opt-in) and today's defaults.
    let cfg: RouterConfig =
        serde_json::from_value(serde_json::json!({ "ledger": { "model": "swarm" } })).unwrap();
    let orch = &cfg.ledger.as_ref().unwrap().orchestrator;
    assert!(!orch.enabled, "coordinator is opt-in");
    assert_eq!(
        orch.kv_policy,
        crate::dag_session::KvSnapshotPolicy::RestoreIfSameModel
    );
    assert_eq!(orch.prompt_budget_chars, 32768);
    assert_eq!(orch.role, "agent");
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
    assert_eq!(orch.kv_policy, crate::dag_session::KvSnapshotPolicy::NeverRestore);
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
    let enc = fleet.get(fluent_llm::onnx_config::OnnxRole::Encoder).unwrap();
    assert_eq!(enc.model.model_path, std::path::PathBuf::from("/nested/encoder.onnx"),
        "nested onnx section wins over top-level key");
    // Top-level key still present (not consumed, just warned about)
    assert!(cfg.encoder.is_some(), "top-level key preserved (warning issued)");
}

#[test]
fn no_onnx_keys_yields_none_fleet() {
    let cfg: RouterConfig = serde_json::from_str(r#"{"server": {"bind_addr": "127.0.0.1:0"}}"#).unwrap();
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
    let enc = fleet.get(fluent_llm::onnx_config::OnnxRole::Encoder).unwrap();
    assert_eq!(enc.model.model_path, std::path::PathBuf::from("/models/encoder.onnx"),
        "weights alias maps to model_path");
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
    cfg.set_field("safety_threshold", "0.7").expect("set_field safety_threshold");
    assert_eq!(cfg.get_field("safety_threshold").unwrap(), "0.7");
    // skipped fields (e.g. models) are NotFound
    let err = cfg.set_field("models", "x").unwrap_err();
    assert!(matches!(err, fluent_wvr::FieldError::NotFound(_)), "models should be NotFound, got {:?}", err);
}

#[test]
fn boot_inherited_pool_with_colliding_group_fails_grammar() {
    // Boot-path regression for the startup fatal: a model that inherits a
    // role pool as authored receives every profile in it — when one
    // profile's group collides with a sibling's name, `validate_instances`
    // (the `build_instance_managers` gate) must reject the pool instead of
    // POSTing a grammar the fork refuses. The fork itself rejects the same
    // shape; the pool here is synthetic, never the shipped file.
    let colliding = default_role_with(fleet_defaults(serde_json::json!({
        "alpha": {"num_ctx": 8192, "default": true},
        "beta": {"num_ctx": 4096, "group": "alpha"}
    })));
    let mut entry = bare_entry();
    materialize("inheritor", &mut entry, &colliding);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 2, "pool inherited as authored");
    assert!(
        crate::instances::validate_instances(profiles).is_err(),
        "beta's group collides with sibling instance alpha"
    );
    // The same pool with each profile in its own group validates.
    let fixed = default_role_with(fleet_defaults(serde_json::json!({
        "alpha": {"num_ctx": 8192, "default": true},
        "beta": {"num_ctx": 4096}
    })));
    let mut entry = bare_entry();
    materialize("inheritor", &mut entry, &fixed);
    crate::instances::validate_instances(entry.effective_pool())
        .expect("namesake groups validate");
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
    assert!(!routing.system_prompt.is_empty(), "tree derives a prompt when none is explicit");
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
    // roles, a hoisted fleet-default profile, per-model selections, the
    // classifier head candidate, and groups built from roles + sentinels.
    // (Covers the R0 absence test alongside the role golden — on owned
    // data, never the operator-editable shipped file.)
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {
            "default": {
                "params": {"num_ctx": 8192},
                "instances": {
                    "default": {"num_ctx": 8192, "default": true},
                    "scout": {"num_ctx": 4096}
                },
                "models": {
                    "code": {"select": "default"},
                    "scout-svc": {"select": "scout"}
                }
            },
            "classifier": {"models": {"code": {}}},
            "code": {"models": {"code": {}}}
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
    assert_eq!(fleet.num_ctx, 8192, "fleet run block lives on the default role");
    let pool = &cfg.roles["default"].instances;
    let hoisted = pool.get("default").expect("fleet default profile");
    assert_eq!(hoisted.num_ctx, 8192);
    assert!(hoisted.default, "fleet profile is the default point");
    assert!(
        pool.contains_key("scout"),
        "the capped override lives in the default pool"
    );
    for role in ["classifier", "code"] {
        assert!(
            cfg.roles[role].instances.is_empty(),
            "role '{role}' defines no extra pool"
        );
    }
    let default_bindings = &cfg.roles["default"].models;
    assert_eq!(
        default_bindings.get("code").and_then(|s| s.select.as_deref()),
        Some("default"),
        "code binds the fleet default profile"
    );
    assert!(
        cfg.roles["code"].models.contains_key("code"),
        "code serves the code role"
    );
    assert!(
        cfg.roles["classifier"].models.contains_key("code"),
        "code serves the classifier role"
    );
    assert_eq!(
        default_bindings.get("scout-svc").and_then(|s| s.select.as_deref()),
        Some("scout"),
        "scout binds its capped override"
    );
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
        "roles": {"classifier": {"models": {"code": {}}}},
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
        "roles": {"classifier": {"models": {"code": {}}}},
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
    // A fixture carrying the role params/pools plus a role-side model
    // binding survives a serde round trip with every value intact (derived
    // pools never serialize).
    let cfg: RouterConfig = serde_json::from_value(serde_json::json!({
        "roles": {
            "code": {
                "params": {"num_ctx": 8192, "temperature": 0.6},
                "instances": {
                    "scratch": {"num_ctx": 4096, "session": true}
                },
                "models": {
                    "code": {"select": "scratch", "params": {"temperature": 0.4}}
                }
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
    let bindings = &cfg.roles["code"].models;
    assert!(bindings.contains_key("code"));
    assert_eq!(cfg.roles["code"].params.num_ctx, 8192);
    let scratch = &cfg.roles["code"].instances["scratch"];
    assert!(scratch.session, "session:true survives the round trip");
    let sel = &cfg.roles["code"].models["code"];
    assert_eq!(sel.select.as_deref(), Some("scratch"));

    let back: RouterConfig =
        serde_json::from_str(&serde_json::to_string(&cfg).expect("serialize"))
            .expect("round trip");
    assert_eq!(back.roles.len(), 2);
    assert_eq!(back.roles["code"].params.num_ctx, 8192);
    assert!(back.roles["code"].instances["scratch"].session);
    assert_eq!(
        back.roles["code"].models["code"]
            .select
            .as_deref(),
        Some("scratch")
    );
}

#[test]
fn instance_profile_session_defaults_to_one_shot() {
    let profile: InstanceProfile =
        serde_json::from_value(serde_json::json!({"num_ctx": 8192}))
            .expect("minimal profile");
    assert!(!profile.session, "absent session → one-shot");
    let explicit: InstanceProfile =
        serde_json::from_value(serde_json::json!({"num_ctx": 8192, "session": true}))
            .expect("session profile");
    assert!(explicit.session);
}

#[test]
fn one_shot_profile_forces_resume_false_at_materialization() {
    // A one-shot profile (`session` absent/false) carrying `resume: true`
    // materializes with `resume: false` — fail-open, never an error.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": { "scratch": { "num_ctx": 8192, "resume": true } },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 1);
    assert!(!profiles[0].session, "absent session → one-shot");
    assert!(
        !profiles[0].resume,
        "one-shot + resume:true materializes as resume:false"
    );
}

#[test]
fn session_profile_keeps_resume_semantics() {
    // `session: true` keeps today's `resume` semantics byte-identically.
    let mut entry = bare_entry();
    let roles = role_table(serde_json::json!({
        "work": {
            "instances": {
                "agent": { "num_ctx": 8192, "session": true, "resume": true },
                "plain": { "num_ctx": 8192, "session": true }
            },
            "models": {"m": {}}
        }
    }));
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 2);
    let agent = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("agent"))
        .expect("agent profile");
    assert!(agent.session);
    assert!(agent.resume, "session:true + resume:true unchanged");
    let plain = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("plain"))
        .expect("plain profile");
    assert!(plain.session);
    assert!(!plain.resume, "session:true without resume stays false");
}

#[test]
fn boot_materializes_one_shot_without_resume_by_default() {
    // A pool declaring no `session` flags materializes every profile
    // one-shot with `resume: false` (no snapshots flow); a `session: true`
    // profile keeps its declared `resume`. Synthetic pool — the shipped
    // file's contents are not a test oracle.
    let roles = role_table(serde_json::json!({
        "fleet": {
            "instances": {
                "work": {"num_ctx": 8192},
                "stateful": {"num_ctx": 8192, "session": true, "resume": true}
            },
            "models": {"m": {}}
        }
    }));
    let mut entry = bare_entry();
    materialize("m", &mut entry, &roles);
    let profiles = entry.effective_pool();
    assert_eq!(profiles.len(), 2);
    let work = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("work"))
        .expect("work profile");
    assert!(!work.session, "undeclared session defaults to one-shot");
    assert!(!work.resume, "one-shot materializes resume:false");
    let stateful = profiles
        .iter()
        .find(|p| p.name.as_deref() == Some("stateful"))
        .expect("session profile");
    assert!(stateful.session);
    assert!(stateful.resume, "session profile keeps declared resume");
}

fn inference_point_fixture() -> (
    std::collections::HashMap<String, ModelEntry>,
    std::collections::HashMap<String, crate::config::RoleEntry>,
) {
    let mut models: std::collections::HashMap<String, ModelEntry> =
        serde_json::from_value(serde_json::json!({
            "swarm": {
                "endpoint": "http://x/v1/chat/completions",
                "name": "workhorse",
                "intelligence": 2,
                "cost_input": 1.0, "cost_output": 6.0, "cost_cached_read": 0.4,
                "tok_s": 8
            },
            "plain": {
                "endpoint": "http://y/v1/chat/completions",
                "name": "plain",
                "intelligence": 1,
                "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0,
                "tok_s": 1
            }
        }))
        .expect("models parse");
    let roles: std::collections::HashMap<String, crate::config::RoleEntry> =
        serde_json::from_value(serde_json::json!({
            "work": {
                "instances": {
                    "swarm": { "count": 3, "group": "swarm", "num_ctx": 16384 },
                    "ledger": { "num_ctx": 131072, "pinned": true, "default": true }
                },
                "models": {"swarm": {}}
            },
            "bare-role": {"models": {"plain": {}}}
        }))
        .expect("roles parse");
    for (key, entry) in models.iter_mut() {
        materialize(key, entry, &roles);
    }
    (models, roles)
}

#[test]
fn inference_point_explicit_qualifier_wins() {
    // An explicit qualifier (embedded or parametric) beats the entry default —
    // and `latest` normalizes away so the remaining precedence applies.
    let (models, roles) = inference_point_fixture();
    assert_eq!(
        resolve_inference_point(&models, &roles, "swarm:scratch", None).as_deref(),
        Some("scratch"),
        "embedded qualifier pins the point"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "swarm", Some("scratch")).as_deref(),
        Some("scratch"),
        "parametric qualifier pins the point"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "swarm:latest", None).as_deref(),
        Some("ledger"),
        "latest falls through to the entry default"
    );
    // Roles carry no qualifier of their own: the point comes from the
    // route-selected model. An explicit qualifier still wins for a role key.
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
    let (models, roles) = inference_point_fixture();
    assert_eq!(
        resolve_inference_point(&models, &roles, "work", None),
        None,
        "a bare role carries no qualifier"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "bare-role", None),
        None,
        "a bare role carries no qualifier"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "swarm", None).as_deref(),
        Some("ledger"),
        "bare model key falls to the entry default"
    );
    assert_eq!(
        resolve_inference_point(&models, &roles, "plain", None),
        None,
        "model without a pool stays bare"
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
fn inference_point_inherits_fleet_default_map() {
    // An entry serving no role and declaring no selection inherits the
    // `default` role's pool through the same code path (the fleet-inherit
    // fallback): its bare key serves the fleet default point, and without
    // the default role the bare key stays bare.
    let mut bare = bare_entry();
    let roles = default_role_with(fleet_defaults(serde_json::json!({
        "default": {"num_ctx": 8192, "default": true}
    })));
    materialize("m", &mut bare, &roles);
    let models = std::collections::HashMap::from([("m".to_string(), bare)]);
    assert_eq!(
        resolve_inference_point(&models, &roles, "m", None).as_deref(),
        Some("default"),
        "fleet default supplies the point"
    );
    let bare = bare_entry();
    let models = std::collections::HashMap::from([("m".to_string(), bare)]);
    assert_eq!(
        resolve_inference_point(&models, &no_roles(), "m", None),
        None,
        "without the default role the bare key stays bare"
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
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/config_roles_split_fixture.json"),
    )
    .expect("split fixture readable")
}

#[test]
fn role_context_block_parses() {
    let cfg = RouterConfig::from_json_str(&roles_split_fixture_text()).expect("split shape parses");
    let quick = cfg.roles.get("quick").expect("quick role present");
    assert_eq!(quick.context.num_ctx, Some(8192));
    assert_eq!(quick.context.max_ctx, Some(262144));
    assert!(quick.context.pinned);
    assert_eq!(quick.context.count, 1);
    assert!(quick.models.contains_key("code"), "membership stays on the role side");
}

#[test]
fn flat_role_keys_rejected_with_migration_pointer() {
    let err = RouterConfig::from_json_str(
        r#"{"roles": {"quick": {"num_ctx": 8192, "pinned": true}}}"#,
    )
    .expect_err("flat residency keys at the role top level must not parse");
    let msg = err.to_string();
    assert!(
        msg.contains("roles.quick.context"),
        "migration error must point at the typed context block, got: {msg}"
    );
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
fn model_role_membership_keys_rejected() {
    let err = RouterConfig::from_json_str(
        r#"{"models": {"code": {"roles": {"quick": {"models": ["code"], "select": "default"}}}}}"#,
    )
    .expect_err("membership keys on the model side must not parse");
    let msg = err.to_string();
    assert!(
        msg.contains("roles.quick.models"),
        "migration error must point at the role-side membership map, got: {msg}"
    );
}

#[test]
fn role_max_parallel_rejected_with_limiter_pointer() {
    let err = RouterConfig::from_json_str(
        r#"{"roles": {"quick": {"max_parallel": 4}}}"#,
    )
    .expect_err("max_parallel is not a RoleEntry field");
    let msg = err.to_string();
    assert!(
        msg.contains("concurrency"),
        "migration error must point at the per-role limiter table, got: {msg}"
    );
}

// -- Five-leg answer params chain + additive thinking (M2) -------------------

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

#[test]
fn answer_chain_each_leg_wins_in_order() {
    let entry = chain_entry();
    let role_sampling = serde_json::json!({"b": "role", "c": "role"});
    let classification = serde_json::json!({"a": "classification", "d": "classification"});
    let out = entry
        .answer_params_for(
            Some("fast"),
            Some("quick"),
            Some(&role_sampling),
            Some(&classification),
        )
        .expect("profile matches");
    let get = |k: &str| out.get(k).and_then(|v| v.as_str()).unwrap_or("<missing>");
    assert_eq!(get("a"), "model", "model.params beats classification duty");
    assert_eq!(get("b"), "role", "role.params beats model.params");
    assert_eq!(get("c"), "override", "model role override is the final leg");
    assert_eq!(get("d"), "classification", "classification-only key survives");
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
            Some("quick"),
            Some(&serde_json::json!({"b": "role", "c": "role"})),
            Some(&serde_json::json!({"a": "classification", "d": "classification"})),
        )
        .expect("profile matches");
    let bare = entry
        .answer_params_for(Some("fast"), None, None, None)
        .expect("profile matches");
    assert_eq!(
        bare,
        entry.instance_params_for("fast").expect("legacy path agrees"),
        "absent legs behave exactly like the legacy qualifier path"
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
            Some("quick"),
            Some(&serde_json::json!({"parallel": 8})),
            Some(&serde_json::json!({"sleep_idle_seconds": 3})),
        )
        .expect("profile matches");
    for key in ["num_ctx", "parallel", "sleep_idle_seconds", "rope_freq_base"] {
        assert!(
            out.get(key).is_none(),
            "declaration key {key} must not reach the body"
        );
    }
    assert_eq!(out.get("temp").and_then(serde_json::Value::as_f64), Some(0.5));
}

#[test]
fn answer_chain_unknown_qualifier_falls_back() {
    let entry = chain_entry();
    assert!(
        entry
            .answer_params_for(Some("nope"), Some("quick"), None, None)
            .is_none(),
        "unknown qualifier returns None so callers fall back to bare params"
    );
}

#[test]
fn thinking_truth_table() {
    fn entry_with(filter: bool, thinking: Option<&str>) -> ModelEntry {
        let mut entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "filter_thinking": filter,
        }))
        .unwrap();
        entry.thinking = thinking.map(str::to_string);
        entry
    }
    // Absent everywhere: no level, no filtering (today's behavior).
    let e = entry_with(false, None);
    assert_eq!(e.resolve_thinking(None, None), None);
    assert!(!e.filter_thinking_for(None, None, None));
    // Bool alias: filter true with no level means off.
    let e = entry_with(true, None);
    assert_eq!(e.resolve_thinking(None, None), None);
    assert!(e.filter_thinking_for(None, None, None));
    // Explicit model level wins over the bool (warn path), filtering off.
    let e = entry_with(true, Some("high"));
    assert_eq!(
        e.resolve_thinking(None, None).as_deref(),
        Some("high"),
        "explicit level wins over the bool"
    );
    // Role leg beats the model leg; override leg beats the role leg.
    let e = entry_with(false, Some("low"));
    let role = serde_json::json!({"thinking": "high"});
    let over = serde_json::json!({"thinking": "max"});
    assert_eq!(
        e.resolve_thinking(Some(&role), None).as_deref(),
        Some("high")
    );
    assert_eq!(
        e.resolve_thinking(Some(&role), Some(&over)).as_deref(),
        Some("max")
    );
    // Explicit off filters.
    let e = entry_with(false, Some("off"));
    let resolved = e.resolve_thinking(None, None);
    assert_eq!(resolved.as_deref(), Some("off"));
    assert!(e.filter_thinking_for(resolved.as_deref(), None, None));
}

#[test]
fn thinking_enable_flag_maps_to_on_off() {
    fn entry_with(filter: bool, thinking: Option<&str>) -> ModelEntry {
        let mut entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "endpoint": "http://x/v1/chat/completions",
            "intelligence": 1,
            "filter_thinking": filter,
        }))
        .unwrap();
        entry.thinking = thinking.map(str::to_string);
        entry
    }
    let e = entry_with(false, None);
    let role = serde_json::json!({"enable_thinking": true});
    assert_eq!(
        e.resolve_thinking(Some(&role), None).as_deref(),
        Some("on"),
        "enable_thinking:true resolves to the on level"
    );
    let role = serde_json::json!({"enable_thinking": false});
    let resolved = e.resolve_thinking(Some(&role), None);
    assert_eq!(resolved.as_deref(), Some("off"));
    assert!(
        e.filter_thinking_for(resolved.as_deref(), Some(&role), None),
        "enable_thinking:false filters like off"
    );
    // Explicit `thinking` beats the bool inside one layer.
    let role = serde_json::json!({"thinking": "high", "enable_thinking": false});
    assert_eq!(
        e.resolve_thinking(Some(&role), None).as_deref(),
        Some("high")
    );
    // Override layer beats the role layer across vocabularies.
    let over = serde_json::json!({"enable_thinking": false});
    assert_eq!(
        e.resolve_thinking(Some(&serde_json::json!({"enable_thinking": true})), Some(&over))
            .as_deref(),
        Some("off")
    );
}

#[test]
fn thinking_effort_passes_through_verbatim() {
    let entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
    }))
    .unwrap();
    let role = serde_json::json!({"reasoning_effort": "high"});
    assert_eq!(
        entry.resolve_thinking(Some(&role), None).as_deref(),
        Some("high")
    );
    // The effort key itself forwards (it is a server knob); only the
    // synthesized `thinking` level is additive.
    let out = entry
        .answer_params_for(None, Some("reasoning"), Some(&role), None)
        .expect("composes");
    assert_eq!(
        out.get("reasoning_effort").and_then(|v| v.as_str()),
        Some("high")
    );
}

#[test]
fn preserve_thinking_beats_filter_bool_but_not_off() {
    let entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
        "filter_thinking": true,
    }))
    .unwrap();
    let role = serde_json::json!({"enable_thinking": true, "preserve_thinking": true});
    let resolved = entry.resolve_thinking(Some(&role), None);
    assert_eq!(resolved.as_deref(), Some("on"));
    assert!(
        !entry.filter_thinking_for(resolved.as_deref(), Some(&role), None),
        "preserve keeps blocks for a live level despite the entry bool"
    );
    // Entry-level preserve counts too (role-less paths consult it).
    let entry: ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://x/v1/chat/completions",
        "intelligence": 1,
        "filter_thinking": true,
        "params": {"preserve_thinking": true},
    }))
    .unwrap();
    let resolved = entry.resolve_thinking(None, None);
    assert!(!entry.filter_thinking_for(resolved.as_deref(), None, None));
    // Off wins over preserve (warn path): nothing generated to keep.
    let off = serde_json::json!({"thinking": "off", "preserve_thinking": true});
    let resolved = entry.resolve_thinking(Some(&off), None);
    assert!(entry.filter_thinking_for(resolved.as_deref(), Some(&off), None));
}

#[test]
fn preserve_thinking_never_reaches_the_body() {
    let mut entry = chain_entry();
    entry.params = Some(serde_json::json!({"preserve_thinking": true, "temp": 0.5}));
    let out = entry
        .answer_params_for(
            Some("fast"),
            Some("quick"),
            Some(&serde_json::json!({"preserve_thinking": true})),
            None,
        )
        .expect("profile matches");
    assert!(
        out.get("preserve_thinking").is_none(),
        "router-local key must not reach the wire"
    );
    assert_eq!(out.get("temp").and_then(serde_json::Value::as_f64), Some(0.5));
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
fn answer_chain_idempotent() {
    let entry = chain_entry();
    let role_sampling = serde_json::json!({"b": "role"});
    let classification = serde_json::json!({"d": "classification"});
    let once = entry
        .answer_params_for(
            Some("fast"),
            Some("quick"),
            Some(&role_sampling),
            Some(&classification),
        )
        .expect("profile matches");
    let folded = overlay_params(Some(&once), Some(&once));
    assert_eq!(folded, once, "re-applying the composition changes nothing");
    let twice = entry
        .answer_params_for(
            Some("fast"),
            Some("quick"),
            Some(&role_sampling),
            Some(&classification),
        )
        .expect("profile matches");
    assert_eq!(twice, once, "the chain is a pure function of its legs");
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

    // Bound models compose their role's pool: exactly the selected window.
    for member in ["m1", "m2"] {
        let names: Vec<&str> = cfg.models[member]
            .effective_pool()
            .iter()
            .map(|p| p.name.as_deref().unwrap())
            .collect();
        assert_eq!(names, vec!["fast"], "{member} serves its role window");
    }

    // Group duty resolves to the bare member key; the inference point
    // qualifies later at dispatch (the two-step duty rule).
    assert_eq!(
        crate::config::resolve_group_head_key(
            &cfg.models,
            &cfg.roles,
            &cfg.model_groups,
            "dev"
        )
        .as_deref(),
        Some("m1"),
        "dev duty is m1"
    );

    // Role-wins-last dispatch on the fixture chain: model, role, override.
    let params = cfg.models["m2"]
        .answer_params_for(
            Some("fast"),
            Some("quick"),
            Some(&serde_json::json!({"rq": "rv"})),
            None,
        )
        .expect("fast resolves");
    let get = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or("<missing>");
    assert_eq!(get("mp"), "mv", "model leg survives");
    assert_eq!(get("rq"), "rv", "role leg composes");
    assert_eq!(get("oq"), "ov", "model role override is final");
}

#[test]
fn fixture_tree_view_keys_and_flags() {
    // Tree terminals are the only routes; flags ride the tree.
    let cfg = ls_fixture_config();
    assert!(cfg.classification.is_some(), "fixture config is tree-only");
    cfg.validate_flat_tree_coherence().expect("fixture tree valid");
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
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
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
