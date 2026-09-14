use super::*;

fn sample_config() -> RoutingConfig {
    serde_json::from_value(serde_json::json!({
        "routes": {
            "fast": {"group": "fast", "pipelines": ["default"], "description": "fast route"},
            "smart": {"group": "smart", "pipelines": ["default"], "always_route": true},
        },
        "models": {
            "tiny": {"endpoint": "http://a", "name": "tiny", "intelligence": 1,
                     "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
            "big": {"endpoint": "http://b", "name": "big", "intelligence": 5,
                    "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10},
            "huge": {"endpoint": "http://c", "name": "huge", "intelligence": 8,
                     "cost_input": 20.0, "cost_output": 20.0, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "model_groups": {"fast": ["tiny"], "smart": ["big", "huge"]},
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "fast",
    }))
    .expect("valid routing config")
}

#[test]
fn route_ref_serde_round_trip() {
    let r: RouteRef = serde_json::from_value(serde_json::json!({
        "group": "g", "pipelines": ["a"], "description": "d", "always_route": true
    }))
    .expect("deserialize");
    assert_eq!(r.group, "g");
    assert_eq!(r.pipelines, vec!["a"]);
    assert!(r.always_route);
    let back: RouteRef =
        serde_json::from_str(&serde_json::to_string(&r).expect("serialize")).expect("round trip");
    assert_eq!(back.group, "g");
}

#[test]
fn route_ref_pipelines_defaults_to_default() {
    let r: RouteRef = serde_json::from_value(serde_json::json!({"group": "g"})).expect("deserialize");
    assert_eq!(r.pipelines, vec!["default"]);
    assert!(!r.always_route);
}

#[test]
fn route_group_resolves_route_or_default() {
    let c = sample_config();
    assert_eq!(c.route_group("fast"), Some("fast"));
    assert_eq!(c.route_group("smart"), Some("smart"));
    // Unknown route falls back to the default route's group.
    assert_eq!(c.route_group("nope"), Some("fast"));
}

#[test]
fn route_group_none_when_no_groups() {
    let c: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"r": {"group": "missing"}},
        "models": {},
        "model_groups": {},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "r",
    }))
    .expect("config");
    assert_eq!(c.route_group("r"), Some("missing"));
    // Unknown route and default route both groupless -> None.
    let c2: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {},
        "models": {},
        "model_groups": {},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "missing",
    }))
    .expect("config");
    assert_eq!(c2.route_group("r"), None);
}

#[test]
fn resolve_route_picks_cheapest_passing_complexity() {
    let c = sample_config();
    // smart group has big(int 5, cost 18) and huge(int 8, cost 40).
    let (entry, name) = c.resolve_route("smart", Some(6)).expect("resolve");
    // big is int 5 < 6 so filtered out; huge (8 >= 6) is the only candidate.
    assert_eq!(name, "huge");
    assert_eq!(entry.intelligence, 8);
    // No min_complexity -> cheapest eligible (big, cost 18).
    let (_, name) = c.resolve_route("smart", None).expect("resolve");
    assert_eq!(name, "big");
}

#[test]
fn resolve_route_falls_back_to_cheapest_when_none_pass() {
    let c = sample_config();
    // min_complexity 9 filters everything; cheapest in group wins.
    let (entry, _) = c.resolve_route("smart", Some(9)).expect("resolve");
    assert_eq!(entry.intelligence, 5, "big is cheapest in the smart group");
}

#[test]
fn resolve_route_unknown_route_uses_default_group() {
    let c = sample_config();
    let (_, name) = c.resolve_route("nope", None).expect("resolve via default");
    assert_eq!(name, "tiny");
}

#[test]
fn resolve_route_direct_model_by_name() {
    // A model name that is not a route resolves as a direct model only
    // when neither the route nor the default route exists.
    let c: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {},
        "models": {
            "big": {"endpoint": "http://b", "name": "big", "intelligence": 5,
                    "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "model_groups": {},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "missing",
    }))
    .expect("config");
    let (entry, name) = c.resolve_route("big", None).expect("resolve direct");
    assert_eq!(name, "big");
    assert_eq!(entry.intelligence, 5);
    // A completely unknown name with no default route -> None.
    assert!(c.resolve_route("nope", None).is_none());
}

#[test]
fn resolve_route_missing_group_returns_none() {
    let c: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"r": {"group": "missing"}},
        "models": {"m": {"endpoint": "http://a", "name": "m", "intelligence": 1,
                         "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10}},
        "model_groups": {},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "r",
    }))
    .expect("config");
    assert!(c.resolve_route("r", None).is_none());
}

#[test]
fn routing_target_attaches_group_target_and_fallbacks() {
    let c = sample_config();
    let rt = c.routing_target("smart", None).expect("routing target");
    assert_eq!(rt.model, "big");
    assert_eq!(rt.group.as_deref(), Some("smart"));
    assert_eq!(rt.target_name.as_deref(), Some("smart"));
    // Primary (big) is skipped; fallbacks are the remaining dispatch
    // targets in preference order.
    let fallback_models: Vec<&str> = rt.fallbacks.iter().map(|f| f.model.as_str()).collect();
    assert!(fallback_models.contains(&"big"));
    assert!(fallback_models.contains(&"tiny"));
}

#[test]
fn routing_target_unknown_route_uses_default() {
    let c = sample_config();
    let rt = c.routing_target("nope", None).expect("routing target");
    assert_eq!(rt.model, "tiny");
    assert_eq!(rt.group.as_deref(), Some("fast"));
}

/// A route whose `model_groups` member is a configured in-process onnx
/// role (e.g. the generative `onnx/llm` routing model) resolves to an onnx
/// `RoutingTarget` — not a `models` entry — so the dispatch layer serves it
/// through the onnx `ChatBackend` (is_onnx, no HTTP url).
#[test]
fn routing_target_resolves_onnx_role_group_member() {
    let mut c: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"local": {"group": "default"}},
        "models": {},
        "model_groups": {"default": ["onnx/llm"]},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "local",
    }))
    .expect("config");
    c.onnx_keys.insert("onnx/llm".into());

    let rt = c.routing_target("local", None).expect("routing target");
    assert!(rt.is_onnx, "onnx target must be flagged for the onnx backend");
    assert_eq!(rt.model, "onnx/llm");
    assert!(rt.url.is_empty(), "onnx targets have no HTTP url");
    assert_eq!(rt.group.as_deref(), Some("default"));
    assert_eq!(rt.target_name.as_deref(), Some("local"));
}

#[test]
fn role_entry_deserializes_pool_and_params() {
    // Roles carry no candidate list and no qualifier: membership lives on
    // the model side, and the point resolves from the route-selected model.
    // Unknown role fields fail loudly.
    let role: RoleEntry = serde_json::from_value(serde_json::json!({
        "params": {"num_ctx": 8192},
        "instances": {"default": {"num_ctx": 8192}}
    }))
    .expect("role entry");
    assert_eq!(role.params.num_ctx, 8192);
    assert!(role.instances.contains_key("default"));

    let bare: RoleEntry = serde_json::from_value(serde_json::json!({})).expect("bare role");
    assert!(bare.instances.is_empty());

    let back: RoleEntry = serde_json::from_str(&serde_json::to_string(&role).expect("serialize"))
        .expect("round trip");
    assert_eq!(back.params.num_ctx, role.params.num_ctx);
    assert!(back.instances.contains_key("default"));

    for key in ["models", "instance"] {
        let mut obj = serde_json::Map::new();
        obj.insert(key.to_string(), serde_json::json!([]));
        assert!(
            serde_json::from_value::<RoleEntry>(serde_json::Value::Object(obj)).is_err(),
            "retired role key '{key}' must not parse"
        );
    }
}

#[test]
fn all_dispatch_targets_orders_primary_group_first() {
    let c = sample_config();
    let targets = c.all_dispatch_targets("smart", None);
    let names: Vec<&str> = targets.iter().map(|(n, _)| n.as_str()).collect();
    // Primary group (smart) first, ordered by intelligence descending
    // (huge int 8 before big int 5), then other groups (tiny).
    assert_eq!(&names[..2], &["huge", "big"]);
    assert!(names.contains(&"tiny"));
}

fn role_config() -> RoutingConfig {
    serde_json::from_value(serde_json::json!({
        "routes": {
            "fast": {"group": "fast", "pipelines": ["default"], "description": "fast route"},
        },
        "models": {
            "tiny": {"endpoint": "http://a", "name": "tiny", "intelligence": 1,
                     "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
            "big": {"endpoint": "http://b", "name": "big", "intelligence": 5,
                    "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "model_groups": {
            "fast": ["quick", "last", "tiny", "any", "unknown-key"],
            "smart": ["big"]
        },
        "roles": {
            "quick": {"models": {"tiny": {}, "big": {}}}
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "fast",
    }))
    .expect("valid routing config")
}

#[test]
fn role_members_expand_to_bound_models_in_order() {
    // A member naming a role fans out to the models the role names (sorted
    // model-key order); sentinels and unknown literals pass through for the
    // downstream stage (sentinel expansion / fail-closed lookup) to own.
    let c = role_config();
    assert_eq!(
        c.role_expanded_members("fast"),
        vec!["big", "tiny", "last", "tiny", "any", "unknown-key"],
    );
    assert_eq!(c.role_expanded_members("smart"), vec!["big", "last", "any"]);
    assert!(c.role_expanded_members("missing").is_empty());
}

#[test]
fn static_route_resolution_reaches_through_roles() {
    // The static path (no recency/liveness) resolves through role candidates
    // and skips availability sentinels: the cheapest intelligence-passing
    // candidate answers.
    let c = role_config();
    let rt = c
        .routing_target("fast", Some(1))
        .expect("role resolves to a target");
    assert_eq!(rt.model, "tiny", "cheapest candidate meeting complexity");
    assert_eq!(rt.group.as_deref(), Some("fast"));
}

#[test]
fn roles_absent_leaves_groups_untouched() {
    // No roles table: expansion is identity apart from the implicit
    // `last`/`any` suffix, which rides along exactly like a declared one.
    let c = sample_config();
    assert_eq!(c.role_expanded_members("fast"), vec!["tiny", "last", "any"]);
    assert_eq!(
        c.role_expanded_members("smart"),
        vec!["big", "huge", "last", "any"]
    );
}

#[test]
fn unbound_role_expands_to_nothing() {
    // A role no model binds to fans out to nothing — the sentinels still
    // pass through, so the group degrades to availability ordering.
    let routing: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {},
        "models": {
            "svc": {"endpoint": "http://a", "name": "svc", "intelligence": 1,
                    "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10}
        },
        "model_groups": {"g": ["spare", "last", "any"]},
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "fast",
        "roles": {
            "default": {"models": {"svc": {}}},
            "spare": {}
        }
    }))
    .expect("valid routing config");
    assert_eq!(
        routing.role_expanded_members("g"),
        vec!["last", "any"],
        "unbound role contributes no candidates"
    );
}

fn role_routing_config() -> RoutingConfig {
    let mut cfg: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {
            "code": {"group": "dev", "role": "quick", "description": "code"},
            "plain": {"group": "dev", "description": "plain"},
            "pinned": {"group": "pinned", "description": "pinned"},
        },
        "models": {
            "tiny": {"endpoint": "http://a", "name": "tiny", "intelligence": 1,
                     "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10,
                     "params": {"mk": "mv"},
                     "roles": {"quick": {"params": {"ok": "ov"}}}},
            "big": {"endpoint": "http://b", "name": "big", "intelligence": 5,
                    "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10,
                    "params": {"mk": "mv"},
                    "roles": {"quick": {"params": {"ok": "ov"}}}},
        },
        "model_groups": {"dev": ["tiny", "big"], "pinned": ["big:fast"]},
        "roles": {
            "default": {"params": {"duty": "duty-only"}},
            "quick": {
                "params": {"rk": "rv"},
                "instances": {"fast": {"num_ctx": 8192}},
                "models": {"big": {"select": "fast"}, "tiny": {"select": "fast"}},
            },
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "plain",
    }))
    .expect("role routing config parses");
    // Materialize pools the way boot does so role profiles resolve.
    let keys: Vec<String> = cfg.models.keys().cloned().collect();
    for key in &keys {
        let pool = crate::config::materialize_effective_pool(key, &cfg.roles);
        cfg.models.get_mut(key).expect("member").effective_profiles = Some(pool);
    }
    cfg
}

#[test]
fn routing_target_applies_terminal_role() {
    let cfg = role_routing_config();
    // Complexity 5 rules out tiny (int 1): big wins, then the terminal role
    // qualifies it to the role profile inside big's own pool.
    let rt = cfg.routing_target("code", Some(5)).expect("target");
    assert_eq!(rt.role.as_deref(), Some("quick"));
    assert_eq!(rt.model, "big:fast", "role selects the profile, not a new model");
    let params = rt.params.as_ref().expect("params");
    let get = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or("<missing>");
    assert_eq!(get("mk"), "mv", "model leg survives");
    assert_eq!(get("rk"), "rv", "role leg composes");
    assert_eq!(get("ok"), "ov", "model role override is final");
    assert!(
        params.get("duty").is_none(),
        "classifier-duty params never leak into the answer target"
    );
}

#[test]
fn routing_target_without_role_unchanged() {
    let cfg = role_routing_config();
    let rt = cfg.routing_target("plain", Some(5)).expect("target");
    assert_eq!(rt.role, None, "no terminal role means no role");
    assert_eq!(
        rt.model, "big:fast",
        "entry default point for a pooled model, role or not"
    );
    let params = rt.params.as_ref().expect("params");
    assert!(
        params.get("ok").is_none(),
        "no terminal role means the model-side override leg stays out"
    );
    assert_eq!(
        params.get("mk").and_then(|v| v.as_str()),
        Some("mv"),
        "model leg composes as before"
    );
}

#[test]
fn explicit_qualifier_wins_over_terminal_role() {
    let mut cfg = role_routing_config();
    cfg.routes.get_mut("pinned").expect("route").role = Some("quick".into());
    let rt = cfg.routing_target("pinned", None).expect("target");
    assert_eq!(rt.model, "big:fast", "explicit member qualifier kept");
    assert_eq!(
        rt.role.as_deref(),
        Some("quick"),
        "role still recorded for the audit trail"
    );
}

#[test]
fn terminal_role_validation_rejects_unknown_role() {
    let cfg = role_routing_config();
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "code", "description": "", "node": {
                "type": "terminal", "route": "code",
                "group": "dev", "role": "nope", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    let err = cfg
        .validate_terminal_roles(&tree)
        .expect_err("unknown role must fail validation");
    assert!(err.contains("nope"), "error names the missing role, got: {err}");
    assert!(err.contains("code"), "error names the terminal route, got: {err}");
}

#[test]
fn terminal_role_validation_rejects_unserved_member() {
    let mut cfg = role_routing_config();
    // lonely serves no role at all and is not the default-inherit case for
    // quick (it is bound nowhere, but quick is not default).
    cfg.models.insert(
        "lonely".into(),
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://d", "name": "lonely", "intelligence": 9,
            "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10,
        }))
        .expect("member"),
    );
    cfg.model_groups.insert(
        "mixed".into(),
        crate::config::ModelGroup::Array(vec!["lonely".into()]),
    );
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "m", "description": "", "node": {
                "type": "terminal", "route": "m",
                "group": "mixed", "role": "quick", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    let err = cfg
        .validate_terminal_roles(&tree)
        .expect_err("unserved member must fail validation");
    assert!(err.contains("lonely"), "error names the member, got: {err}");
    assert!(err.contains("quick"), "error names the role, got: {err}");
}

#[test]
fn terminal_role_validation_accepts_served_group() {
    let cfg = role_routing_config();
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "code", "description": "", "node": {
                "type": "terminal", "route": "code",
                "group": "dev", "role": "quick", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    cfg.validate_terminal_roles(&tree)
        .expect("served group validates");
}

fn lookup_config() -> RoutingConfig {
    serde_json::from_value(serde_json::json!({
        "routes": {
            "shared": {"group": "g1", "description": "route wins"},
            "plain": {"group": "g1", "description": "plain"},
        },
        "models": {
            "m1": {"endpoint": "http://a", "name": "m1", "intelligence": 1,
                   "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
            "m2": {"endpoint": "http://b", "name": "m2", "intelligence": 5,
                   "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10},
            "shared": {"endpoint": "http://c", "name": "shared-model", "intelligence": 3,
                       "cost_input": 2.0, "cost_output": 2.0, "cost_cached_read": 0.0, "tok_s": 10},
            "duo": {"endpoint": "http://d", "name": "duo-model", "intelligence": 2,
                    "cost_input": 1.5, "cost_output": 1.5, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "model_groups": {
            "g1": ["m1"],
            "shared": ["m2"],
            "duo": ["m2"],
            "dev": ["m1", "m2"],
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "plain",
    }))
    .expect("lookup config parses")
}

#[test]
fn requested_name_classification_order() {
    let cfg = lookup_config();
    assert_eq!(
        cfg.classify_requested("shared"),
        crate::config::RequestedRef::Route("shared".into()),
        "route wins over group and model"
    );
    assert_eq!(
        cfg.classify_requested("duo"),
        crate::config::RequestedRef::Group("duo".into()),
        "group wins over model"
    );
    assert_eq!(
        cfg.classify_requested("m1"),
        crate::config::RequestedRef::Model("m1".into()),
        "model resolves when nothing shadows it"
    );
    assert_eq!(
        cfg.classify_requested("model:shared"),
        crate::config::RequestedRef::DirectModel("shared".into()),
        "hatch addresses the models table past the route"
    );
    assert_eq!(
        cfg.classify_requested("zzz"),
        crate::config::RequestedRef::Unknown("zzz".into()),
    );
}

#[test]
fn route_shadows_group_and_model() {
    let cfg = lookup_config();
    // "shared" names a route (→ g1 → m1), a group, and a model.
    let (_, name) = cfg.resolve_route("shared", None).expect("resolves");
    assert_eq!(name, "m1", "bare name serves the route's group member");
}

#[test]
fn group_wins_over_model() {
    let cfg = lookup_config();
    // "duo" names a group ([m2]) and a model; the group wins.
    let (_, name) = cfg.resolve_route("duo", None).expect("resolves");
    assert_eq!(name, "m2", "group member served");
    let rt = cfg.routing_target("dev", None).expect("group target");
    assert_eq!(rt.group.as_deref(), Some("dev"));
    assert_eq!(rt.target_name.as_deref(), Some("dev"));
    assert_eq!(rt.role, None, "group hits carry no terminal role");
}

#[test]
fn model_hatch_bypasses_route() {
    let cfg = lookup_config();
    let (_, name) = cfg
        .resolve_route("model:shared", None)
        .expect("hatch resolves");
    assert_eq!(name, "shared-model", "hatch serves the models entry directly");
    let rt = cfg.routing_target("model:shared", None).expect("target");
    assert_eq!(rt.model, "shared-model");
    assert_eq!(rt.group, None, "hatch is a direct address, like direct models");
    assert_eq!(rt.target_name.as_deref(), Some("shared"));
}

#[test]
fn hatch_unknown_fails_closed() {
    let cfg = lookup_config();
    assert!(
        cfg.resolve_route("model:nope", None).is_none(),
        "explicit hatch never falls back to the default route"
    );
    assert!(
        cfg.routing_target("model:nope", None).is_none(),
        "explicit hatch never fabricates a target"
    );
}

#[test]
fn unknown_keeps_default_fallback() {
    let cfg = lookup_config();
    let (_, name) = cfg.resolve_route("zzz", None).expect("default serves");
    assert_eq!(name, "m1", "unknown names still resolve via the default route");
    let rt = cfg.routing_target("zzz", None).expect("default target");
    assert_eq!(
        rt.target_name.as_deref(),
        Some("zzz"),
        "requested name preserved on the default target"
    );
}

#[test]
fn lookup_collisions_name_every_shadow() {
    let cfg = lookup_config();
    let collisions = cfg.lookup_collisions();
    for key in ["shared", "duo"] {
        assert!(
            collisions.iter().any(|m| m.contains(key)),
            "collision report names {key}: {collisions:?}"
        );
    }
    assert!(
        collisions.iter().any(|m| m.contains("shared") && m.contains("route")),
        "route shadow named: {collisions:?}"
    );
    assert!(
        !collisions.iter().any(|m| m.contains("m1")),
        "unshadowed names stay silent: {collisions:?}"
    );
}

fn concurrency_config() -> RoutingConfig {
    let mut cfg: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"code": {"group": "dev", "description": "code"}},
        "models": {
            "m": {"endpoint": "http://a", "name": "m", "intelligence": 1,
                  "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "model_groups": {"dev": ["m"]},
        "roles": {
            "quick": {
                "instances": {"fast": {"num_ctx": 8192}},
                "models": {"m": {"select": "fast"}},
                "concurrency": {"max_parallel": 8},
            },
            "plain": {},
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "code",
    }))
    .expect("concurrency config parses");
    let pool = crate::config::materialize_effective_pool("m", &cfg.roles);
    cfg.models.get_mut("m").expect("member").effective_profiles = Some(pool);
    cfg
}

#[test]
fn role_concurrency_parses_and_defaults_unbounded() {
    let cfg = concurrency_config();
    assert_eq!(
        cfg.roles["quick"]
            .concurrency
            .as_ref()
            .expect("cap present")
            .max_parallel,
        8
    );
    assert!(
        cfg.roles["plain"].concurrency.is_none(),
        "absent table means unbounded"
    );
}

#[test]
fn role_concurrency_zero_rejected() {
    let err = serde_json::from_value::<RoutingConfig>(serde_json::json!({
        "routes": {}, "models": {}, "model_groups": {},
        "roles": {"quick": {"concurrency": {"max_parallel": 0}}},
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "code",
    }))
    .expect_err("a zero cap admits nothing and must not parse");
    assert!(
        err.to_string().contains("at least 1"),
        "rejection says the floor, got: {err}"
    );
}

#[test]
fn capacity_mismatch_warns_with_counts() {
    let cfg = concurrency_config();
    let ((), logs) = crate::test_support::capture_logs(|| {
        cfg.warn_on_role_capacity_mismatch();
    });
    let joined = logs.join("\n");
    assert!(
        joined.contains("quick") && joined.contains('8'),
        "warning names the role and the cap, logs:\n{joined}"
    );
}

#[test]
fn capacity_match_stays_silent() {
    let mut cfg = concurrency_config();
    cfg.roles.get_mut("quick").expect("role").concurrency =
        Some(crate::config::RoleConcurrency { max_parallel: 1 });
    let ((), logs) = crate::test_support::capture_logs(|| {
        cfg.warn_on_role_capacity_mismatch();
    });
    assert!(
        !logs.join("\n").contains("quick"),
        "no warning when the cap fits the counted slots"
    );
}
