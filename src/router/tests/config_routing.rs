use super::*;

fn sample_config() -> RoutingConfig {
    serde_json::from_value(serde_json::json!({
        "routes": {
            "fast": {"role": "fast", "pipelines": ["default"], "description": "fast route"},
            "smart": {"role": "smart", "pipelines": ["default"], "always_route": true},
        },
        "models": {
            "tiny": {"endpoint": "http://a", "name": "tiny", "intelligence": 1,
                     "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
            "big": {"endpoint": "http://b", "name": "big", "intelligence": 5,
                    "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10},
            "huge": {"endpoint": "http://c", "name": "huge", "intelligence": 8,
                     "cost_input": 20.0, "cost_output": 20.0, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "roles": {"fast": {"models": ["tiny"]}, "smart": {"models": ["big", "huge"]}},
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "fast",
    }))
    .expect("valid routing config")
}

#[test]
fn route_ref_serde_round_trip() {
    let r: RouteRef = serde_json::from_value(serde_json::json!({
        "role": "q", "pipelines": ["a"], "description": "d", "always_route": true
    }))
    .expect("deserialize");
    assert_eq!(r.role, "q");
    assert_eq!(r.pipelines, vec!["a"]);
    assert!(r.always_route);
    let back: RouteRef =
        serde_json::from_str(&serde_json::to_string(&r).expect("serialize")).expect("round trip");
    assert_eq!(back.role, "q");
}

#[test]
fn route_ref_pipelines_defaults_to_default() {
    let r: RouteRef =
        serde_json::from_value(serde_json::json!({"role": "q"})).expect("deserialize");
    assert_eq!(r.pipelines, vec!["default"]);
    assert!(!r.always_route);
}

#[test]
fn route_role_resolves_route_or_default() {
    let c = sample_config();
    assert_eq!(c.route_role("fast"), Some("fast"));
    assert_eq!(c.route_role("smart"), Some("smart"));
    // Unknown route falls back to the default route's role.
    assert_eq!(c.route_role("nope"), Some("fast"));
}

#[test]
fn route_role_none_when_no_roles() {
    let c: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"r": {"role": "missing"}},
        "models": {},
        "roles": {},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "r",
    }))
    .expect("config");
    assert_eq!(c.route_role("r"), Some("missing"));
    // Unknown route and default route both roleless -> None.
    let c2: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {},
        "models": {},
        "roles": {},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "missing",
    }))
    .expect("config");
    assert_eq!(c2.route_role("r"), None);
}

#[test]
fn resolve_route_picks_cheapest_passing_complexity() {
    let c = sample_config();
    // smart role has big(int 5, cost 18) and huge(int 8, cost 40).
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
    // min_complexity 9 filters everything; cheapest in role wins.
    let (entry, _) = c.resolve_route("smart", Some(9)).expect("resolve");
    assert_eq!(entry.intelligence, 5, "big is cheapest in the smart role");
}

#[test]
fn resolve_route_unknown_route_uses_default_role() {
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
fn resolve_route_missing_role_returns_none() {
    let c: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"r": {"role": "missing"}},
        "models": {"m": {"endpoint": "http://a", "name": "m", "intelligence": 1,
                         "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10}},
        "roles": {},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "r",
    }))
    .expect("config");
    assert!(c.resolve_route("r", None).is_none());
}

#[test]
fn routing_target_attaches_role_target_and_fallbacks() {
    let c = sample_config();
    let rt = c.routing_target("smart", None).expect("routing target");
    assert_eq!(rt.model, "big");
    assert_eq!(rt.role.as_deref(), Some("smart"));
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
    assert_eq!(rt.role.as_deref(), Some("fast"));
}

/// A route whose role `models` member is a configured in-process onnx
/// role (e.g. the generative `onnx/llm` routing model) resolves to an onnx
/// `RoutingTarget` — not a `models` entry — so the dispatch layer serves it
/// through the onnx `ChatBackend` (is_onnx, no HTTP url).
#[test]
fn routing_target_resolves_onnx_role_member() {
    let mut c: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"local": {"role": "default"}},
        "models": {},
        "roles": {"default": {"models": ["onnx/llm"]}},
        "system_prompt": "s",
        "safety_threshold": 0.5,
        "default_route": "local",
    }))
    .expect("config");
    c.onnx_keys.insert("onnx/llm".into());

    let rt = c.routing_target("local", None).expect("routing target");
    assert!(
        rt.is_onnx,
        "onnx target must be flagged for the onnx backend"
    );
    assert_eq!(rt.model, "onnx/llm");
    assert!(rt.url.is_empty(), "onnx targets have no HTTP url");
    assert_eq!(rt.role.as_deref(), Some("default"));
    assert_eq!(rt.target_name.as_deref(), Some("local"));
}

#[test]
fn role_entry_deserializes_flat_fields() {
    // Roles are flat: params, model list, residency and admission fields.
    // Unknown role fields fail loudly.
    let role: RoleEntry = serde_json::from_value(serde_json::json!({
        "params": {"num_ctx": 8192},
        "models": ["m"],
        "pinned": true,
        "max_parallel": 2
    }))
    .expect("role entry");
    assert_eq!(role.params.num_ctx, 8192);
    assert_eq!(role.models, vec!["m".to_string()]);
    assert!(role.pinned);
    assert_eq!(role.max_parallel, Some(2));

    let bare: RoleEntry = serde_json::from_value(serde_json::json!({})).expect("bare role");
    assert!(bare.models.is_empty());
    assert!(bare.max_parallel.is_none());

    let back: RoleEntry = serde_json::from_str(&serde_json::to_string(&role).expect("serialize"))
        .expect("round trip");
    assert_eq!(back.params.num_ctx, role.params.num_ctx);
    assert_eq!(back.models, role.models);

    for key in ["context", "concurrency", "instances", "instance"] {
        let mut obj = serde_json::Map::new();
        obj.insert(key.to_string(), serde_json::json!([]));
        assert!(
            serde_json::from_value::<RoleEntry>(serde_json::Value::Object(obj)).is_err(),
            "retired role key '{key}' must not parse"
        );
    }
}

#[test]
fn all_dispatch_targets_orders_primary_role_first() {
    let c = sample_config();
    let targets = c.all_dispatch_targets("smart", None);
    let names: Vec<&str> = targets.iter().map(|(n, _)| n.as_str()).collect();
    // Primary role (smart) first, ordered by intelligence descending
    // (huge int 8 before big int 5), then other roles (tiny).
    assert_eq!(&names[..2], &["huge", "big"]);
    assert!(names.contains(&"tiny"));
}

fn role_config() -> RoutingConfig {
    serde_json::from_value(serde_json::json!({
        "routes": {
            "fast": {"role": "fast", "pipelines": ["default"], "description": "fast route"},
        },
        "models": {
            "tiny": {"endpoint": "http://a", "name": "tiny", "intelligence": 1,
                     "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
            "big": {"endpoint": "http://b", "name": "big", "intelligence": 5,
                    "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "roles": {
            "fast": {"models": ["quick", "last", "tiny", "any", "unknown-key"]},
            "smart": {"models": ["big"]},
            "quick": {"models": ["tiny", "big"]}
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
        c.role_dispatch_members("fast"),
        vec!["big", "tiny", "last", "tiny", "any", "unknown-key"],
    );
    assert_eq!(c.role_dispatch_members("smart"), vec!["big"]);
    assert!(c.role_dispatch_members("missing").is_empty());
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
    assert_eq!(rt.role.as_deref(), Some("fast"));
}

#[test]
fn unlisted_roles_ride_last_sentinel() {
    // No roles table entry at all: expansion is empty (fail-closed), while
    // a declared-but-empty role rides `["last"]` (see
    // `RoleEntry::effective_models`).
    let c = sample_config();
    assert!(c.role_dispatch_members("missing").is_empty());
    let bare: RoleEntry = serde_json::from_value(serde_json::json!({})).expect("bare role");
    assert_eq!(bare.effective_models(), vec!["last".to_string()]);
}

#[test]
fn unbound_role_expands_to_nothing() {
    // A role no model binds to fans out to nothing — the sentinels still
    // pass through, so the role degrades to availability ordering.
    let routing: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {},
        "models": {
            "svc": {"endpoint": "http://a", "name": "svc", "intelligence": 1,
                    "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10}
        },
        "roles": {
            "default": {"models": ["svc"]},
            "spare": {},
            "g": {"models": ["spare", "last", "any"]}
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "fast",
    }))
    .expect("valid routing config");
    assert_eq!(
        routing.role_dispatch_members("g"),
        vec!["last", "any"],
        "unbound role contributes no candidates"
    );
}

fn role_routing_config() -> RoutingConfig {
    let mut cfg: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {
            "code": {"role": "quick", "description": "code"},
            "plain": {"role": "dev", "description": "plain"},
            "pinned": {"role": "pinned", "description": "pinned"},
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
        "roles": {
            "default": {"params": {"duty": "duty-only"}},
            "quick": {
                "params": {"rk": "rv"},
                "models": ["big", "tiny"],
            },
            "dev": {"models": ["tiny", "big"]},
            "pinned": {"models": ["big:fast"]},
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "plain",
    }))
    .expect("role routing config parses");
    // Materialize pools the way boot does so role profiles resolve.
    let keys: Vec<String> = cfg.models.keys().cloned().collect();
    for key in &keys {
        let entry = cfg.models.get(key).cloned().expect("member");
        let pool = crate::config::materialize_effective_pool(key, &entry, &cfg.roles);
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
    assert_eq!(
        rt.model, "big:quick",
        "role selects the profile, not a new model"
    );
    let params = rt.params.as_ref().expect("params");
    let get = |k: &str| {
        params
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>")
    };
    assert_eq!(get("mk"), "mv", "model leg survives");
    assert_eq!(get("rk"), "rv", "role leg composes");
    assert_eq!(get("ok"), "ov", "model role override is final");
    assert!(
        params.get("duty").is_none(),
        "classifier-duty params never leak into the answer target"
    );
}

#[test]
fn routing_target_role_serves_own_window() {
    // Route "plain" names role "dev", which binds big — dispatch serves
    // big through the dev window with the role recorded.
    let cfg = role_routing_config();
    let rt = cfg.routing_target("plain", Some(5)).expect("target");
    assert_eq!(rt.role.as_deref(), Some("dev"), "role recorded");
    assert_eq!(rt.model, "big:dev", "role serves its own window");
    let params = rt.params.as_ref().expect("params");
    assert_eq!(
        params.get("mk").and_then(|v| v.as_str()),
        Some("mv"),
        "model leg composes as before"
    );
}

#[test]
fn explicit_qualifier_wins_over_terminal_role() {
    // The role would force its own window on a bare member (see
    // `routing_target_applies_terminal_role`), but an explicit member
    // qualifier is kept verbatim with the role recorded for the audit
    // trail.
    let mut cfg = role_routing_config();
    cfg.roles.get_mut("quick").expect("role").models = vec!["tiny:ledger".into(), "big".into()];
    let rt = cfg.routing_target("code", None).expect("target");
    assert_eq!(rt.model, "tiny:ledger", "explicit member qualifier kept");
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
                "role": "nope", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    let err = cfg
        .validate_terminal_roles(&tree)
        .expect_err("unknown role must fail validation");
    assert!(
        err.contains("nope"),
        "error names the missing role, got: {err}"
    );
    assert!(
        err.contains("code"),
        "error names the terminal route, got: {err}"
    );
}

/// Pinned-role fixture: same shape as [`role_routing_config`] with `quick`
/// pinned, and pools deliberately NOT materialized — dispatch must resolve
/// the qualifier identically pre/post boot for pinned roles.
fn terminal_role_pinned_config() -> RoutingConfig {
    serde_json::from_value(serde_json::json!({
        "routes": {
            "code": {"role": "quick", "description": "code"},
            "pinned": {"role": "pinned", "description": "pinned"},
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
        "roles": {
            "default": {"params": {"duty": "duty-only"}},
            "quick": {
                "pinned": true,
                "params": {"rk": "rv"},
                "models": ["big", "tiny"],
            },
            "pinned": {"models": ["big:fast"]},
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "code",
    }))
    .expect("pinned role config parses")
}

#[test]
fn terminal_role_pinned_forces_qualifier_without_materialized_pool() {
    let cfg = terminal_role_pinned_config();
    // Complexity 5 rules out tiny: big wins, and the pinned role forces the
    // qualifier even though no pool was materialized (pre-boot parity).
    let rt = cfg.routing_target("code", Some(5)).expect("target");
    assert_eq!(rt.role.as_deref(), Some("quick"));
    assert_eq!(rt.model, "big:quick", "pinned role forces its window");
}

#[test]
fn terminal_role_pinned_explicit_point_still_wins() {
    // The pinned fixture's "pinned" route already carries an explicit
    // qualifier in its role members: it is kept verbatim, role recorded.
    let cfg = terminal_role_pinned_config();
    let rt = cfg.routing_target("pinned", None).expect("target");
    assert_eq!(rt.model, "big:fast", "explicit member qualifier kept");
    assert_eq!(
        rt.role.as_deref(),
        Some("pinned"),
        "role recorded for audit"
    );
}

#[test]
fn terminal_role_unpinned_unserved_dispatch_falls_back_to_default() {
    let mut cfg = role_routing_config();
    // lonely is bound to no role; the unpinned terminal role cannot qualify
    // it, so dispatch serves the entry default point (graceful, by design).
    cfg.models.insert(
        "lonely".into(),
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://d", "name": "lonely", "intelligence": 9,
            "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10,
        }))
        .expect("member"),
    );
    cfg.roles.insert(
        "solo".into(),
        serde_json::from_value(serde_json::json!({"models": ["lonely"]})).expect("role"),
    );
    // Point the solo route at unpinned quick, which does not bind lonely:
    // the member resolves through quick's list below.
    cfg.roles
        .get_mut("quick")
        .expect("role")
        .models
        .push("lonely".into());
    cfg.routes.insert(
        "solo".into(),
        serde_json::from_value(serde_json::json!({"role": "quick", "description": "solo"}))
            .expect("route"),
    );
    let rt = cfg.routing_target("solo", None).expect("target");
    // `lonely` was inserted after pool materialization, so its entry default
    // is the bare wire id (unmaterialized entries resolve bare).
    assert_eq!(rt.model, "lonely", "unbound member serves entry default");
    assert!(
        !rt.model.contains(":quick"),
        "no forced qualifier without binding"
    );
    assert_eq!(rt.role.as_deref(), Some("quick"), "role still recorded");
}

#[test]
fn terminal_role_validation_rejects_unserved_member() {
    let mut cfg = role_routing_config();
    // Pin quick: a pinned role must be fillable, and a bare-only group with
    // no serving member and no runtime flexibility fails boot. (Unpinned
    // roles fall back to the entry default — see the accepts test below.)
    cfg.roles.get_mut("quick").expect("role").pinned = true;
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
    // A bare-only role member that serves no role, with no runtime
    // flexibility (no `last`/`any` sentinel): a pinned role naming it
    // through fan-out names a window that can never exist at runtime.
    cfg.roles.insert(
        "mixed".into(),
        serde_json::from_value(serde_json::json!({"models": ["lonely"]})).expect("bare-only role"),
    );
    cfg.roles.get_mut("quick").expect("role").models = vec!["mixed".into()];
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "m", "description": "", "node": {
                "type": "terminal", "route": "m",
                "role": "quick", "description": "",
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
fn terminal_role_validation_accepts_unpinned_unserved_member() {
    // Same lonely setup, but quick is unpinned (the shared helper default):
    // dispatch falls back to the entry default, so boot passes.
    let mut cfg = role_routing_config();
    cfg.models.insert(
        "lonely".into(),
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://d", "name": "lonely", "intelligence": 9,
            "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10,
        }))
        .expect("member"),
    );
    cfg.roles.insert(
        "mixed".into(),
        serde_json::from_value(serde_json::json!({"models": ["lonely"]})).expect("role"),
    );
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "m", "description": "", "node": {
                "type": "terminal", "route": "m",
                "role": "mixed", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    cfg.validate_terminal_roles(&tree)
        .expect("unpinned role with graceful fallback validates");
}

#[test]
fn terminal_role_validation_accepts_pinned_with_sentinel_flexibility() {
    // Pinned quick, but the role carries `last` alongside a fanned-out
    // unbound member: recency may fill the role at runtime, so boot passes
    // (the winner still qualifies only if it binds the role — force never
    // invents a window).
    let mut cfg = role_routing_config();
    cfg.roles.get_mut("quick").expect("role").pinned = true;
    cfg.models.insert(
        "lonely".into(),
        serde_json::from_value(serde_json::json!({
            "endpoint": "http://d", "name": "lonely", "intelligence": 9,
            "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10,
        }))
        .expect("member"),
    );
    cfg.roles.insert(
        "flex".into(),
        serde_json::from_value(serde_json::json!({"models": ["lonely"]})).expect("role"),
    );
    cfg.roles.get_mut("quick").expect("role").models = vec!["last".into(), "flex".into()];
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "m", "description": "", "node": {
                "type": "terminal", "route": "m",
                "role": "quick", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    cfg.validate_terminal_roles(&tree)
        .expect("sentinel-bearing role may fill the pinned role at runtime");
}

#[test]
fn terminal_role_validation_accepts_served_role() {
    let cfg = role_routing_config();
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "code", "description": "", "node": {
                "type": "terminal", "route": "code",
                "role": "quick", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    cfg.validate_terminal_roles(&tree)
        .expect("served role validates");
}

fn lookup_config() -> RoutingConfig {
    serde_json::from_value(serde_json::json!({
        "routes": {
            "shared": {"role": "g1", "description": "route wins"},
            "plain": {"role": "g1", "description": "plain"},
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
        "roles": {
            "g1": {"models": ["m1"]},
            "shared": {"models": ["m2"]},
            "duo": {"models": ["m2"]},
            "dev": {"models": ["m1", "m2"]},
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
        "route wins over role and model"
    );
    assert_eq!(
        cfg.classify_requested("duo"),
        crate::config::RequestedRef::Role("duo".into()),
        "role wins over model"
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
fn route_shadows_role_and_model() {
    let cfg = lookup_config();
    // "shared" names a route (→ g1 → m1), a role, and a model.
    let (_, name) = cfg.resolve_route("shared", None).expect("resolves");
    assert_eq!(name, "m1", "bare name serves the route's role member");
}

#[test]
fn role_wins_over_model() {
    let cfg = lookup_config();
    // "duo" names a role ([m2]) and a model; the role wins.
    let (_, name) = cfg.resolve_route("duo", None).expect("resolves");
    assert_eq!(name, "m2", "role member served");
    let rt = cfg.routing_target("dev", None).expect("role target");
    assert_eq!(rt.role.as_deref(), Some("dev"));
    assert_eq!(rt.target_name.as_deref(), Some("dev"));
}

#[test]
fn model_hatch_bypasses_route() {
    let cfg = lookup_config();
    let (_, name) = cfg
        .resolve_route("model:shared", None)
        .expect("hatch resolves");
    assert_eq!(
        name, "shared-model",
        "hatch serves the models entry directly"
    );
    let rt = cfg.routing_target("model:shared", None).expect("target");
    assert_eq!(rt.model, "shared-model");
    assert_eq!(
        rt.role, None,
        "hatch is a direct address, like direct models"
    );
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
    assert_eq!(
        name, "m1",
        "unknown names still resolve via the default route"
    );
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
        collisions
            .iter()
            .any(|m| m.contains("shared") && m.contains("route")),
        "route shadow named: {collisions:?}"
    );
    assert!(
        !collisions.iter().any(|m| m.contains("m1")),
        "unshadowed names stay silent: {collisions:?}"
    );
}

fn concurrency_config() -> RoutingConfig {
    let mut cfg: RoutingConfig = serde_json::from_value(serde_json::json!({
        "routes": {"code": {"role": "quick", "description": "code"}},
        "models": {
            "m": {"endpoint": "http://a", "name": "m", "intelligence": 1,
                  "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
        },
        "roles": {
            "quick": {
                "models": ["m"],
                "max_parallel": 8,
            },
            "plain": {},
        },
        "system_prompt": "sys",
        "safety_threshold": 0.5,
        "default_route": "code",
    }))
    .expect("concurrency config parses");
    let entry = cfg.models.get("m").cloned().expect("member");
    let pool = crate::config::materialize_effective_pool("m", &entry, &cfg.roles);
    cfg.models.get_mut("m").expect("member").effective_profiles = Some(pool);
    cfg
}

#[test]
fn role_concurrency_parses_and_defaults_unbounded() {
    let cfg = concurrency_config();
    assert_eq!(cfg.roles["quick"].max_parallel, Some(8));
    assert!(
        cfg.roles["plain"].max_parallel.is_none(),
        "absent cap means unbounded"
    );
}

#[test]
fn role_concurrency_zero_rejected() {
    let err = crate::config::RouterConfig::from_json_value(serde_json::json!({
        "roles": {"quick": {"max_parallel": 0}}
    }))
    .expect_err("a zero cap admits nothing and must not parse");
    assert!(
        err.to_string().contains("at least 1"),
        "rejection says the floor, got: {err}"
    );
}

#[test]
fn capacity_mismatch_warns_with_counts() {
    // A role whose pool was never materialized counts zero slots under its
    // cap: the pre-boot fail-open signal still warns with role + cap.
    let mut cfg = concurrency_config();
    cfg.models.get_mut("m").expect("member").effective_profiles = None;
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
fn capacity_materialized_role_stays_silent() {
    // Sibling materialization (`count = max_parallel`) makes a materialized
    // role coherent by construction: 8 windows under a cap of 8 admit
    // exactly what the gate enforces, so no warning fires.
    let cfg = concurrency_config();
    let ((), logs) = crate::test_support::capture_logs(|| {
        cfg.warn_on_role_capacity_mismatch();
    });
    assert!(
        !logs.join("\n").contains("quick"),
        "materialized cap stays silent, logs:\n{}",
        logs.join("\n")
    );
}

fn viability_models() -> std::collections::HashMap<String, crate::config::ModelEntry> {
    serde_json::from_value(serde_json::json!({
        "aaa_first": {"endpoint": "http://a", "name": "aaa_first", "intelligence": 1,
                      "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10},
        "mmm_mid": {"endpoint": "http://m", "name": "mmm_mid", "intelligence": 5,
                    "cost_input": 5.0, "cost_output": 5.0, "cost_cached_read": 0.0, "tok_s": 10},
        "zzz_big": {"endpoint": "http://z", "name": "zzz_big", "intelligence": 8,
                    "cost_input": 9.0, "cost_output": 9.0, "cost_cached_read": 0.0, "tok_s": 10},
    }))
    .expect("viability models")
}

#[test]
fn viability_all_resident_passing_matches_legacy() {
    let models = viability_models();
    let members = vec![
        "aaa_first".to_string(),
        "mmm_mid".to_string(),
        "zzz_big".to_string(),
    ];
    let all_resident = |_: &str| true;
    let viable = super::select_viable_member(
        "g",
        &members,
        &models,
        Some(1),
        Some(&all_resident as &dyn Fn(&str) -> bool),
    );
    let legacy = super::select_role_member("g", &members, &models, Some(1));
    assert_eq!(
        viable, legacy,
        "all-resident-passing must stay byte-identical"
    );
    assert_eq!(viable.as_deref(), Some("aaa_first"));
}

#[test]
fn viability_resident_passing_beats_lexicographic_first() {
    let models = viability_models();
    // Lexicographic-first key fails complexity and is cold; viable pick skips it.
    let members = vec![
        "aaa_first".to_string(),
        "mmm_mid".to_string(),
        "zzz_big".to_string(),
    ];
    let resident = |key: &str| key != "aaa_first";
    let picked = super::select_viable_member(
        "g",
        &members,
        &models,
        Some(5),
        Some(&resident as &dyn Fn(&str) -> bool),
    );
    assert_eq!(picked.as_deref(), Some("mmm_mid"));
}

#[test]
fn viability_none_passing_returns_cheapest() {
    let models = viability_models();
    let members = vec!["mmm_mid".to_string(), "zzz_big".to_string()];
    let picked = super::select_viable_member("g", &members, &models, Some(9), None);
    assert_eq!(
        picked.as_deref(),
        Some("mmm_mid"),
        "nothing passes: cheapest wins"
    );
}

#[test]
fn viability_never_returns_non_member() {
    let models = viability_models();
    let groups = [
        vec!["aaa_first".to_string()],
        vec!["mmm_mid".to_string(), "zzz_big".to_string()],
        vec!["last".to_string()],
        vec![
            "aaa_first".to_string(),
            "last".to_string(),
            "zzz_big".to_string(),
        ],
    ];
    for (i, members) in groups.iter().enumerate() {
        for complexity in [None, Some(1), Some(5), Some(9)] {
            let picked = super::select_viable_member("g", members, &models, complexity, None);
            if let Some(key) = picked {
                let (base, _) = fluent_types::model_key::split_model_key(&key);
                assert!(
                    members.iter().any(|m| m == base || m == &key) || models.contains_key(base),
                    "case {i} complexity {complexity:?}: {key} must be a member or known model"
                );
            }
        }
    }
}

#[test]
fn viability_sentinel_only_resolves_viable_not_lexicographic() {
    let models = viability_models();
    // Sentinel-only group: lexicographic-first (`aaa_first`, int 1) fails the
    // bar, so viability must pick the cheapest passing model instead.
    let picked = super::select_viable_member("g", &["last".to_string()], &models, Some(5), None);
    assert_eq!(picked.as_deref(), Some("mmm_mid"));
}

/// Calibration for the viability threshold (measurement only).
///
/// Hand-computed matrix over resident × passing (non-resident stands in for
/// unfittable here; true VRAM-fittability plumbing arrives with admission
/// wiring). Control group (all-resident-passing) must NOT switch — it is the
/// byte-parity anchor to pre-viability behavior. Every other row asserts the
/// exact expected pick, so the cost-ascending tie-break is locked before any
/// decision caching.
#[test]
fn viability_calibration_matrix_locks_threshold() {
    let models = viability_models();
    struct Case {
        members: Vec<String>,
        complexity: Option<u8>,
        resident: Vec<String>,
        probe: bool,
        expected_viable: &'static str,
        expected_legacy: &'static str,
        control: bool,
    }
    let s = |v: &[&str]| {
        v.iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    };
    let cases = vec![
        Case {
            members: s(&["aaa_first", "mmm_mid", "zzz_big"]),
            complexity: Some(1),
            resident: s(&["aaa_first", "mmm_mid", "zzz_big"]),
            probe: true,
            expected_viable: "aaa_first",
            expected_legacy: "aaa_first",
            control: true,
        },
        Case {
            members: s(&["mmm_mid", "zzz_big"]),
            complexity: Some(5),
            resident: s(&["mmm_mid", "zzz_big"]),
            probe: true,
            expected_viable: "mmm_mid",
            expected_legacy: "mmm_mid",
            control: true,
        },
        Case {
            members: s(&["zzz_big"]),
            complexity: Some(8),
            resident: s(&["zzz_big"]),
            probe: true,
            expected_viable: "zzz_big",
            expected_legacy: "zzz_big",
            control: true,
        },
        Case {
            members: s(&["aaa_first", "mmm_mid", "zzz_big"]),
            complexity: Some(5),
            resident: s(&["zzz_big"]),
            probe: true,
            expected_viable: "zzz_big",
            expected_legacy: "mmm_mid",
            control: false,
        },
        Case {
            members: s(&["mmm_mid", "zzz_big"]),
            complexity: Some(5),
            resident: s(&["zzz_big"]),
            probe: true,
            expected_viable: "zzz_big",
            expected_legacy: "mmm_mid",
            control: false,
        },
        Case {
            members: s(&["aaa_first", "mmm_mid", "zzz_big"]),
            complexity: Some(5),
            resident: s(&[]),
            probe: true,
            expected_viable: "mmm_mid",
            expected_legacy: "mmm_mid",
            control: false,
        },
        // Note: `select_role_member` now delegates to the viable rule, so its
        // sentinel-only answer is viable too (the M1 behavior change). The
        // pre-M1 lexicographic answer would have been `aaa_first`.
        Case {
            members: s(&["last"]),
            complexity: Some(5),
            resident: s(&[]),
            probe: false,
            expected_viable: "mmm_mid",
            expected_legacy: "mmm_mid",
            control: false,
        },
        Case {
            members: s(&["last"]),
            complexity: Some(9),
            resident: s(&[]),
            probe: false,
            expected_viable: "aaa_first",
            expected_legacy: "aaa_first",
            control: false,
        },
        Case {
            members: s(&["mmm_mid", "zzz_big"]),
            complexity: Some(9),
            resident: s(&["mmm_mid", "zzz_big"]),
            probe: true,
            expected_viable: "mmm_mid",
            expected_legacy: "mmm_mid",
            control: false,
        },
    ];
    let mut control_total = 0;
    let mut control_unchanged = 0;
    let mut switches = 0;
    let mut switch_hits = 0;
    let mut mismatches = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let viable = if case.probe {
            let resident = case.resident.clone();
            let probe = move |key: &str| resident.iter().any(|r| r == key);
            super::select_viable_member(
                "g",
                &case.members,
                &models,
                case.complexity,
                Some(&probe as &dyn Fn(&str) -> bool),
            )
        } else {
            super::select_viable_member("g", &case.members, &models, case.complexity, None)
        };
        let legacy = super::select_role_member("g", &case.members, &models, case.complexity);
        if viable.as_deref() != Some(case.expected_viable) {
            mismatches.push(format!(
                "case {i}: viable {:?}, expected {}",
                viable, case.expected_viable
            ));
        }
        if legacy.as_deref() != Some(case.expected_legacy) {
            mismatches.push(format!(
                "case {i}: legacy {:?}, expected {}",
                legacy, case.expected_legacy
            ));
        }
        if case.control {
            control_total += 1;
            if viable == legacy {
                control_unchanged += 1;
            }
        }
        if viable != legacy {
            switches += 1;
            if viable.as_deref() == Some(case.expected_viable) {
                switch_hits += 1;
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "calibration mismatches:\n{}",
        mismatches.join("\n")
    );
    assert_eq!(
        (control_unchanged, control_total),
        (3, 3),
        "control group (all-resident-passing) must never switch"
    );
    // Only the residency-probed rows switch against the no-signal wrapper:
    // rows 4-5 (cold cheapest-passing yields to resident). Sentinel rows use
    // the same `None` signal on both sides, so they agree by construction.
    assert_eq!(switches, 2, "exactly the two residency rows switch");
    assert_eq!(
        switch_hits, switches,
        "switch precision must be 1.0: every switch lands on the viable pick"
    );
}

#[test]
fn sibling_rr_count_materializes_from_max_parallel() {
    let entry: crate::config::ModelEntry = serde_json::from_value(serde_json::json!({
        "endpoint": "http://a", "name": "m", "intelligence": 2,
        "cost_input": 1.0, "cost_output": 1.0, "cost_cached_read": 0.0, "tok_s": 10,
    }))
    .expect("entry parses");
    let roles: std::collections::HashMap<String, crate::config::RoleEntry> =
        serde_json::from_value(serde_json::json!({
            "quick": {"max_parallel": 2, "pinned": true, "models": ["m"], "params": {}},
            "scratch": {"models": ["m"], "params": {}},
        }))
        .expect("roles parse");
    let pool = crate::config::materialize_effective_pool("m", &entry, &roles);
    let mut quick: Vec<_> = pool
        .iter()
        .filter(|p| p.group.as_deref() == Some("quick"))
        .collect();
    quick.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(quick.len(), 2, "max_parallel 2 materializes two windows");
    assert_eq!(
        quick
            .iter()
            .filter_map(|p| p.name.clone())
            .collect::<Vec<_>>(),
        vec!["quick-0".to_string(), "quick-1".to_string()]
    );
    assert!(
        quick.iter().all(|p| p.pinned),
        "role pinned composes onto every sibling"
    );
    let scratch: Vec<_> = pool
        .iter()
        .filter(|p| p.group.as_deref() == Some("scratch"))
        .collect();
    assert_eq!(scratch.len(), 1, "unset max_parallel stays a single window");
    assert_eq!(scratch[0].name.as_deref(), Some("scratch"));
}

#[test]
fn capacity_match_stays_silent() {
    let mut cfg = concurrency_config();
    cfg.roles.get_mut("quick").expect("role").max_parallel = Some(1);
    let ((), logs) = crate::test_support::capture_logs(|| {
        cfg.warn_on_role_capacity_mismatch();
    });
    assert!(
        !logs.join("\n").contains("quick"),
        "no warning when the cap fits the counted slots"
    );
}
