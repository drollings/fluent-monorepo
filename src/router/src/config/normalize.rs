//! Pre-typing config normalizer legs: raw-JSON passes that run inside
//! [`super::root::RouterConfig::from_json_value`], before typing, in
//! constructor order (`normalize_model_defaults` →
//! `normalize_fleet_role_params`). The byte-parity oracle
//! (`config_normalize_parity` tests) pins their exact output; keep these
//! passes shallow and dependency-free.

use super::overlay::overlay_params;
use super::root::RouterConfig;

/// Sparse-inherit the `models.default` template over every sibling model
/// entry on the raw JSON, before typing. Models declare only deltas: an
/// absent top-level key inherits the template value, a declared key wins;
/// when both template and sibling declare a `params` object, the template's
/// missing keys fill the sibling's (shallowly — nested objects are never
/// recursed into). No `models.default` object leaves the document untouched.
pub(crate) fn normalize_model_defaults(value: &mut serde_json::Value) {
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
                    // Both sides declare params maps: the template fills
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

/// Sparse-merge the fleet run block (`roles.default.params`) over every
/// other role's `params` on the raw JSON, before typing — then fill the
/// fleet block's own absent keys from `models.default.params`, so the models
/// template is the fleet base even when `roles.default` is `{}`. Roles
/// declare only deltas: absent keys inherit the fleet value, declared keys
/// win, shallowly. No `default` role, or no `params` on it, leaves sibling
/// roles untouched (the models-template leg above still applies).
pub(crate) fn normalize_fleet_role_params(value: &mut serde_json::Value) {
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
                    if let Some(fleet_params) = params.as_object_mut() {
                        for (key, val) in &template_params {
                            fleet_params.entry(key.clone()).or_insert(val.clone());
                        }
                    }
                }
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
/// object. `params` is valid on model entries and model-side per-role
/// overrides, and every layer composes through [`overlay_params`], which
/// silently drops non-object sides. A string/number/array in either
/// position is therefore always a config bug that would otherwise vanish
/// without a trace; the merge itself is unchanged (fail-open, non-objects
/// contribute nothing).
pub fn warn_on_non_object_params(cfg: &RouterConfig) {
    fn check(value: Option<&serde_json::Value>, path: &str) {
        if let Some(v) = value {
            if !v.is_object() {
                tracing::warn!(
                    target: "router.config",
                    path = %path,
                    "params must be a JSON object; non-object value ignored \
                     (params are supplemented or overwritten per key, \
                     never replaced wholesale)",
                );
            }
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

/// Declaration-only request-body keys the fork ignores: the instance grammar
/// owns them (`ctx`/`parallel`/`sleep`), so they must not leak into the body.
pub const DECLARATION_PARAM_KEYS: [&str; 4] = [
    "num_ctx",
    "parallel",
    "sleep_idle_seconds",
    "rope_freq_base",
];

/// Router-local params key selecting the response-side keep behavior for
/// thinking blocks (see [`crate::config::ModelEntry::preserve_thinking`]). Never a server
/// knob — the body-boundary stripper below removes it, so it never reaches
/// the wire (llama-server knows no such field).
pub const PRESERVE_THINKING_KEY: &str = "preserve_thinking";

/// Remove declaration-only keys from a params object, keeping sampling params
/// (`temperature`, `repeat_penalty`, `chat_template_kwargs`, ...). Non-object
/// params are returned unchanged. Router-local control keys (currently only
/// [`PRESERVE_THINKING_KEY` — consumed by the router, unknown to every
/// server) are removed here too: this is the single body boundary every
/// params path funnels through, including the bare-entry fallbacks.
pub fn strip_declaration_params(params: serde_json::Value) -> serde_json::Value {
    let Some(obj) = params.as_object() else {
        return params;
    };
    let mut out = obj.clone();
    for k in DECLARATION_PARAM_KEYS {
        out.remove(k);
    }
    out.remove(PRESERVE_THINKING_KEY);
    serde_json::Value::Object(out)
}

#[cfg(test)]
#[path = "../../tests/config_normalize_parity.rs"]
mod config_normalize_parity_tests;
