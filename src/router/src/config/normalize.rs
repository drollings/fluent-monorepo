//! Pre-typing config normalizer legs: raw-JSON passes that run inside
//! [`super::root::RouterConfig::from_json_value`], before typing, in
//! constructor order (`reject_legacy_role_shapes` → `normalize_model_defaults`
//! → `normalize_fleet_role_params`, which hoists nested role params per role).
//! The byte-parity oracle (`config_normalize_parity` tests) pins their exact
//! output; keep these passes shallow and dependency-free.

use super::root::{overlay_params, RouterConfig};

/// Flat residency keys that belong inside a role's typed `context` block,
/// never at the role top level. A role entry carrying one is always the
/// legacy flat shape — rejected with a migration pointer, never hoisted
/// silently (a silent hoist would let a `num_ctx` typo inside `params`
/// diverge from the top-level value with no error).
const LEGACY_ROLE_CONTEXT_KEYS: [&str; 14] = [
    "num_ctx",
    "ctx_size",
    "max_ctx",
    "pinned",
    "count",
    "parallel",
    "no_sleep",
    "warm",
    "sleep_idle_seconds",
    "resume",
    "session",
    "default",
    "group",
    "name",
];

/// Membership keys that belong on the role-side binding
/// (`roles.<role>.models.<model>`), never on a model-side per-role override
/// (`models.<model>.roles.<role>`). A model-side override carrying one is
/// always a membership declaration in the wrong table — rejected with a
/// migration pointer instead of being dropped by `deny_unknown_fields`
/// with no guidance.
const LEGACY_MODEL_ROLE_MEMBERSHIP_KEYS: [&str; 3] = ["models", "select", "instance"];

/// Reject legacy role shapes on the raw JSON, before typing, with migration
/// pointers at the new locations. Runs first in
/// [`RouterConfig::from_json_value`], so the error names the intended shape
/// instead of surfacing as a bare `deny_unknown_fields` unknown-field error:
///
/// - `roles.<role>` carrying flat residency keys → `roles.<role>.context`;
/// - `roles.<role>.max_parallel` → the per-role limiter table
///   (`roles.<role>.concurrency.max_parallel`);
/// - `models.<model>.roles.<role>` carrying membership keys →
///   `roles.<role>.models`.
///
/// Deterministic: roles and models are visited in sorted key order, so the
/// first reported shape is stable across runs.
pub(crate) fn reject_legacy_role_shapes(value: &serde_json::Value) -> Result<(), String> {
    let mut role_names: Vec<&str> = value
        .get("roles")
        .and_then(|roles| roles.as_object())
        .map(|roles| roles.keys().map(String::as_str).collect())
        .unwrap_or_default();
    role_names.sort_unstable();
    for role in role_names {
        let entry = &value["roles"][role];
        let Some(obj) = entry.as_object() else {
            continue;
        };
        if obj.contains_key("max_parallel") {
            return Err(format!(
                "roles.{role}.max_parallel is not a role field; declare admission caps as roles.{role}.concurrency.max_parallel"
            ));
        }
        let mut flat: Vec<&str> = LEGACY_ROLE_CONTEXT_KEYS
            .iter()
            .filter(|k| obj.contains_key(**k))
            .copied()
            .collect();
        flat.sort_unstable();
        if !flat.is_empty() {
            return Err(format!(
                "roles.{role} carries flat key(s) [{}]; move residency keys into roles.{role}.context (num_ctx/max_ctx/pinned/count)",
                flat.join(", ")
            ));
        }
    }
    let mut model_names: Vec<&str> = value
        .get("models")
        .and_then(|models| models.as_object())
        .map(|models| models.keys().map(String::as_str).collect())
        .unwrap_or_default();
    model_names.sort_unstable();
    for model in model_names {
        let mut override_roles: Vec<&str> = value["models"][model]["roles"]
            .as_object()
            .map(|overrides| overrides.keys().map(String::as_str).collect())
            .unwrap_or_default();
        override_roles.sort_unstable();
        for role in override_roles {
            let entry = &value["models"][model]["roles"][role];
            let Some(obj) = entry.as_object() else {
                continue;
            };
            let mut membership: Vec<&str> = LEGACY_MODEL_ROLE_MEMBERSHIP_KEYS
                .iter()
                .filter(|k| obj.contains_key(**k))
                .copied()
                .collect();
            membership.sort_unstable();
            if !membership.is_empty() {
                return Err(format!(
                    "models.{model}.roles.{role} carries membership key(s) [{}]; membership lives on the role side in roles.{role}.models (models.{model}.roles.{role} is a params-only override)",
                    membership.join(", ")
                ));
            }
        }
    }
    Ok(())
}

/// Sparse-inherit the `models.default` template over every sibling model
/// entry on the raw JSON, before typing. Models declare only deltas: an
/// absent top-level key inherits the template value, a declared key wins;
/// when both template and sibling declare a `params` object, the template's
/// missing sampling keys fill the sibling's (shallowly — nested objects are
/// never recursed into). Runs before [`normalize_fleet_role_params`] so the
/// fleet run block below observes the template-fed `roles.default.params`.
/// No `models.default` object leaves the document untouched.
pub(crate) fn normalize_model_defaults(value: &mut serde_json::Value) {
    // NOTE: a typed `BTreeMap<String, RawModelEntry>` merge was attempted and
    // reverted: the shallow-merge semantics resist typing. The merge
    // distinguishes absent `params` (filled from the template) from
    // explicit-`null` `params` (present — never filled), but serde_json maps
    // JSON null to `None` for both `Option<Value>` and `Option<Option<Value>>`,
    // collapsing the two states. A faithful typed leg would need a bespoke
    // three-state enum with custom serde — more machinery than this raw pass
    // for zero behavioral gain — so the raw pass is retained. The
    // `explicit_null_params_never_filled` parity case pins the distinction.
    let Some(template) = value
        .get("models")
        .and_then(|models| models.get(RouterConfig::DEFAULT_MODEL_KEY))
        .and_then(|default| default.as_object())
        .cloned()
    else {
        return;
    };
    let Some(models) = value
        .as_object_mut()
        .and_then(|root| root.get_mut("models"))
        .and_then(|models| models.as_object_mut())
    else {
        return;
    };
    for (name, entry) in models.iter_mut() {
        if name == RouterConfig::DEFAULT_MODEL_KEY {
            continue;
        }
        let Some(entry_obj) = entry.as_object_mut() else {
            continue;
        };
        for (key, template_value) in &template {
            match entry_obj.get(key) {
                None => {
                    entry_obj.insert(key.clone(), template_value.clone());
                }
                Some(entry_value) if key == "params" => {
                    // Both sides declare sampling maps: the template fills
                    // only the sibling's missing keys, shallowly.
                    if let (Some(base), Some(over)) =
                        (template_value.as_object(), entry_value.as_object())
                    {
                        let mut merged = base.clone();
                        for (k, v) in over {
                            merged.insert(k.clone(), v.clone());
                        }
                        entry_obj.insert(key.clone(), serde_json::Value::Object(merged));
                    }
                }
                Some(_) => {}
            }
        }
    }
}

/// Hoist a legacy nested `params`-inside-`params` sampling object one level
/// up, in place. The nested form (`roles.<role>.params.params: {...}`) is no
/// longer a composition layer: its keys move into the enclosing run block
/// (enclosing keys win on collision) and the nested object is dropped. A
/// non-object `params` value is left for the boot-time
/// [`warn_on_non_object_params`] pass to report.
pub(crate) fn hoist_nested_role_params(params: &mut serde_json::Value) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    if let Some(nested) = obj.remove("params") {
        if let Some(nested_obj) = nested.as_object() {
            for (key, val) in nested_obj {
                obj.entry(key.clone()).or_insert(val.clone());
            }
        } else {
            obj.insert("params".to_string(), nested);
        }
    }
}

/// Sparse-merge the fleet run block (`roles.default.params`) over every
/// other role's `params` on the raw JSON, before typing — then fill the
/// fleet block's own absent launch knobs from `models.default.params`, so
/// the models template is the fleet base even when `roles.default` is `{}`.
/// Roles declare only deltas: absent keys inherit the fleet value, declared
/// keys win, shallowly (nested objects are replaced wholesale, never
/// deep-merged). A role without a `params` block inherits the fleet block
/// whole. No `default` role, or no `params` on it, leaves sibling roles
/// untouched (the models-template leg above still applies).
pub(crate) fn normalize_fleet_role_params(value: &mut serde_json::Value) {
    // The models template feeds the fleet block's absent launch knobs first.
    if let Some(template_params) = value
        .get("models")
        .and_then(|models| models.get(RouterConfig::DEFAULT_MODEL_KEY))
        .and_then(|default| default.get("params"))
        .and_then(|params| params.as_object())
        .cloned()
    {
        if let Some(default_role) = value
            .as_object_mut()
            .and_then(|root| root.get_mut("roles"))
            .and_then(|roles| roles.as_object_mut())
            .and_then(|roles| roles.get_mut("default"))
            .and_then(|default| default.as_object_mut())
        {
            match default_role.get_mut("params") {
                Some(params) => {
                    // Declared run block: the template fills only absent keys.
                    if let Some(fleet_params) = params.as_object_mut() {
                        for (key, val) in &template_params {
                            fleet_params.entry(key.clone()).or_insert(val.clone());
                        }
                    }
                }
                // No run block at all (`"default": {}`): the template's run
                // block IS the fleet block.
                None => {
                    default_role.insert(
                        "params".to_string(),
                        serde_json::Value::Object(template_params),
                    );
                }
            }
        }
    }
    let Some(roles) = value
        .as_object_mut()
        .and_then(|root| root.get_mut("roles"))
        .and_then(|roles| roles.as_object_mut())
    else {
        return;
    };
    // Legacy nesting hoists before any merge, per role: a role's own nested
    // keys land in its own top level first (its own top-level keys win ties),
    // so the fleet merge below cannot bury a declared nested key under a
    // fleet value.
    for role in roles.values_mut() {
        if let Some(params) = role.get_mut("params") {
            hoist_nested_role_params(params);
        }
    }
    let Some(fleet) = roles
        .get("default")
        .and_then(|default| default.get("params"))
        .cloned()
    else {
        return;
    };
    for (name, role) in roles.iter_mut() {
        if name == "default" {
            continue;
        }
        match role.get("params") {
            None => {
                if let Some(obj) = role.as_object_mut() {
                    obj.insert("params".to_string(), fleet.clone());
                }
            }
            Some(role_params) => {
                let merged = overlay_params(Some(&fleet), Some(role_params));
                role["params"] = merged;
            }
        }
    }
}

/// Warn loudly over every `params` map in the config that is not a JSON
/// object. `params` is valid on pool profiles, role-side model bindings, and
/// model entries — role run blocks carry no nested `params` object (their
/// sampling keys are flattened into `roles.<role>.params` itself) — and
/// every layer composes through [`overlay_params`], which silently drops
/// non-object sides. A string/number/array in any of these positions is
/// therefore always a config bug that would otherwise vanish without a
/// trace; the merge itself is unchanged (fail-open, non-objects contribute
/// nothing).
pub fn warn_on_non_object_params(cfg: &RouterConfig) {
    fn check(value: Option<&serde_json::Value>, path: &str) {
        if let Some(v) = value {
            if !v.is_object() {
                tracing::warn!(
                    target: "router.config",
                    path = %path,
                    "params must be a JSON object; non-object value ignored \
                     (role params are supplemented or overwritten per key, \
                     never replaced wholesale)",
                );
            }
        }
    }
    let mut role_names: Vec<&String> = cfg.roles.keys().collect();
    role_names.sort();
    for role_name in role_names {
        let role = &cfg.roles[role_name];
        let mut profile_names: Vec<&String> = role.instances.keys().collect();
        profile_names.sort();
        for profile_name in profile_names {
            check(
                role.instances[profile_name].params.as_ref(),
                &format!("roles.{role_name}.instances.{profile_name}.params"),
            );
        }
        let mut bound_models: Vec<&String> = role.models.keys().collect();
        bound_models.sort();
        for model_key in bound_models {
            check(
                role.models[model_key].params.as_ref(),
                &format!("roles.{role_name}.models.{model_key}.params"),
            );
        }
    }
    let mut model_keys: Vec<&String> = cfg.models.keys().collect();
    model_keys.sort();
    for model_key in model_keys {
        let entry = &cfg.models[model_key];
        check(entry.params.as_ref(), &format!("models.{model_key}.params"));
        if let Some(overrides) = entry.role_params.as_ref() {
            let mut override_roles: Vec<&String> = overrides.keys().collect();
            override_roles.sort();
            for role in override_roles {
                check(
                    overrides[role].params.as_ref(),
                    &format!("models.{model_key}.roles.{role}.params"),
                );
            }
        }
    }
}
