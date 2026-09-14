//! Route resolution and routing configuration types.
//! `RoutingConfig` is the resolved routing table used by the classifier stage.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::ModelEntry;
use super::{InstanceProfile, ModelBinding, RoleParams};
use crate::config::split_model_key;
use crate::config::ModelGroup;
use crate::pipeline::RoutingTarget;
use crate::score_matrix::ScoreMatrix;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRef {
    pub group: String,
    /// Context-profile role for the answer target, subordinate to `group`
    /// (the group picks the weights, the role picks the window). `None`
    /// resolves the entry default point, exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default = "default_pipelines")]
    pub pipelines: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// Never let the classifier answer requests on this route directly: force
    /// dispatch to the route's group. For domains where the classifier model is
    /// overconfident (creative prose, code, translation, and specialized
    /// knowledge such as science/legal/medical), this guarantees the request
    /// reaches the route's model regardless of the classifier's own complexity
    /// judgment. `local`-style routes keep `always_route: false` so simple
    /// prompts are still answered directly.
    #[serde(default)]
    pub always_route: bool,
}

fn default_pipelines() -> Vec<String> {
    vec!["default".into()]
}

/// Reserved qualifier prefix addressing the models table directly, past any
/// route or group shadowing the same name (`model:<key>`, remainder possibly
/// qualified as `<base>:<point>`). Intercepted before the key grammar runs —
/// `split_model_key` would otherwise read `model` as the base.
pub const MODEL_PREFIX: &str = "model:";

/// Which namespace a bare requested name resolved in — the single ordered
/// lookup behind every request path (`resolve_route`, `routing_target`, the
/// server pipeline): routes, then model groups, then models. One classifier
/// so the three paths can never disagree about what a name means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedRef {
    /// `model:<remainder>`: address the models table directly (the remainder
    /// may itself be qualified). Bypasses the order below.
    DirectModel(String),
    /// Exact `routes` hit (route shadowing a group/model wins — warned).
    Route(String),
    /// Exact `model_groups` hit (group shadowing a model wins — warned).
    Group(String),
    /// Exact `models` hit.
    Model(String),
    /// No namespace hit (callers fall back to the default route, as before).
    Unknown(String),
}

/// Strip the [`MODEL_PREFIX`] hatch (`None` for bare names and for a lone
/// `model:` with an empty remainder — an explicit form misused, resolved as
/// the bare name through the normal order).
pub fn strip_model_prefix(name: &str) -> Option<&str> {
    name.strip_prefix(MODEL_PREFIX).filter(|rest| !rest.is_empty())
}

/// A role's KV-window/residency axis: the context sizes and residency
/// predicates shared by every model serving the role. All-optional: an
/// absent key contributes nothing, so a role without a `context` block
/// behaves exactly as before. Sampling never lives here — it composes
/// through `RoleEntry.params` (and the model-side override) instead, so a
/// residency knob and a sampling key can never silently diverge under the
/// same name. Defaults mirror [`InstanceProfile`] semantics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RoleContext {
    /// Context size in tokens both the supervisor and the sidecar read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u64>,
    /// Context-size cap; clamps the window at materialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ctx: Option<u64>,
    /// Exempt from auto-sleep and eviction; implies no_sleep.
    #[serde(default, skip_serializing_if = "RoleContext::is_false")]
    pub pinned: bool,
    /// Sibling instances sharing the profile's group (1 = single instance).
    #[serde(default = "default_role_count", skip_serializing_if = "RoleContext::is_one")]
    pub count: u32,
}

impl RoleContext {
    pub fn is_empty(&self) -> bool {
        self.num_ctx.is_none() && self.max_ctx.is_none() && !self.pinned && self.count == 1
    }

    // `skip_serializing_if` hands these a reference by contract; the
    // by-value lint does not apply to the serde call shape.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_false(b: &bool) -> bool {
        !b
    }

    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_one(n: &u32) -> bool {
        *n == 1
    }
}

const fn default_role_count() -> u32 {
    1
}

/// One entry of the routing-vocabulary table: a role names *what the request
/// needs* — its KV-window/residency axis (`context`), its run block
/// (`params`), its fleet named pool (`instances`), and the models that serve
/// it (`models`). Membership lives on the role side (`roles.<role>.models`
/// names the serving model keys); models never name roles for membership, so
/// the two tables stay separate and compose only through the binding.
/// Resolution to a concrete
/// `base:qualifier` target happens per request at dispatch time, never at
/// boot, so lazy models load correctly.
///
/// A role owns its run configuration: `params` is the role's full "how a
/// model is run" block (launch knobs plus flat sampling keys — never a
/// nested `params` object; the fleet block lives as `roles.default.params`),
/// and `instances` is the role's fleet named pool — the profiles a model's
/// role-side binding (`select`) resolves against. Sampling composes
/// `models.default` → model entry → role-base → pool profile → per-model
/// binding (the role side wins over the entry at dispatch), materialized
/// at boot into each model's effective pool. A role's qualifier
/// is never stored here: it resolves from the route, which selects a model
/// from a group, and the model's entry default supplies the point
/// (explicit `base:point` still wins).
/// Per-role admission table: how many simultaneous dispatches the role
/// admits, enforced per-role globally at the dispatch gate. Absent (the
/// default) leaves the role unbounded — today's behavior. `max_parallel`
/// must be at least 1 (a zero cap would admit nothing and wedge every
/// dispatch through the role, so it fails closed at parse).
#[derive(Debug, Clone, Serialize, Default)]
pub struct RoleConcurrency {
    /// Maximum simultaneous dispatches admitted for the role.
    pub max_parallel: usize,
}

impl<'de> Deserialize<'de> for RoleConcurrency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            max_parallel: usize,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.max_parallel == 0 {
            return Err(serde::de::Error::custom(
                "concurrency.max_parallel must be at least 1 (0 would admit nothing)",
            ));
        }
        Ok(Self {
            max_parallel: raw.max_parallel,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleEntry {
    /// The role's KV-window/residency axis (context sizes, pinning, sibling
    /// count). Absent (the default) contributes nothing — today's pool path
    /// is unchanged.
    #[serde(default, skip_serializing_if = "RoleContext::is_empty")]
    pub context: RoleContext,
    /// Admission cap for the role (see [`RoleConcurrency`]). Absent means
    /// unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<RoleConcurrency>,
    /// The role's run block: launch knobs plus flat sampling keys (no nested
    /// `params` object) every binding for this role composes over. Absent
    /// (the default) contributes no sampling base.
    #[serde(default)]
    pub params: RoleParams,
    /// The role's fleet named instance pool. Models serve the role through
    /// the `models` binding below; empty (the default) means the role
    /// defines no shared profiles.
    #[serde(default)]
    pub instances: HashMap<String, InstanceProfile>,
    /// The models serving this role, keyed by `models` key: the composition
    /// point where the separate tables meet. Each binding narrows the role's
    /// pool to the model's contribution (`select`), with a sparse sampling
    /// update (`params`) and embedding override. A model listed in no role's
    /// `models` map inherits the `default` role's pool (the fleet-inherit
    /// fallback, same code path).
    #[serde(default)]
    pub models: HashMap<String, ModelBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    pub routes: HashMap<String, RouteRef>,
    pub models: HashMap<String, ModelEntry>,
    pub model_groups: HashMap<String, ModelGroup>,
    pub system_prompt: String,
    pub safety_threshold: f64,
    pub default_route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_matrix: Option<ScoreMatrix>,
    /// Registry keys of the configured in-process onnx roles (e.g. `onnx/llm`).
    /// A `model_groups` member that names one of these is a valid dispatch
    /// target served by the onnx `ChatBackend` — it is not a `models` entry, so
    /// the route resolver treats it specially. Populated at config-build time
    /// from `RouterConfig.onnx`; empty for a config with no onnx fleet.
    #[serde(default)]
    pub onnx_keys: BTreeSet<String>,
    /// Routing-vocabulary table (mirrors `RouterConfig.roles`): role name →
    /// candidate model keys + the inference point each candidate serves.
    /// Absent (the default) leaves group members untouched.
    #[serde(default)]
    pub roles: HashMap<String, RoleEntry>,
}

impl RoutingConfig {
    /// Classify a bare requested name into its namespace — routes, then
    /// model groups, then models, first hit wins. The hatch bypasses the
    /// order; unknown names fall back to the default route at the call site
    /// (existing behavior, preserved).
    pub fn classify_requested(&self, name: &str) -> RequestedRef {
        if let Some(rest) = strip_model_prefix(name) {
            return RequestedRef::DirectModel(rest.to_string());
        }
        if self.routes.contains_key(name) {
            return RequestedRef::Route(name.to_string());
        }
        if self.model_groups.contains_key(name) {
            return RequestedRef::Group(name.to_string());
        }
        if self.models.contains_key(name) {
            return RequestedRef::Model(name.to_string());
        }
        RequestedRef::Unknown(name.to_string())
    }

    /// Every shadowed key from the ordered lookup, in sorted order (stable
    /// across runs): a route shadowing a group/model makes that group/model
    /// unreachable by bare name, and a group shadowing a model does the
    /// same. A shadowed model stays addressable via the [`MODEL_PREFIX`]
    /// hatch; a shadowed group has no bypass — rename the route or the
    /// group. Unshadowed names stay silent.
    pub fn lookup_collisions(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut group_keys: Vec<&String> = self.model_groups.keys().collect();
        group_keys.sort();
        for key in group_keys {
            if self.routes.contains_key(key) {
                out.push(format!(
                    "requested model '{key}' resolves as route (shadows model_group '{key}'): the group is unreachable by bare name — rename the route or the group"
                ));
            }
        }
        let mut model_keys: Vec<&String> = self.models.keys().collect();
        model_keys.sort();
        for key in model_keys {
            if self.routes.contains_key(key) {
                out.push(format!(
                    "requested model '{key}' resolves as route (shadows model '{key}'): address the model as '{MODEL_PREFIX}{key}'"
                ));
            } else if self.model_groups.contains_key(key) {
                out.push(format!(
                    "requested model '{key}' resolves as model_group (shadows model '{key}'): address the model as '{MODEL_PREFIX}{key}'"
                ));
            }
        }
        out
    }

    /// Boot warning over [`Self::lookup_collisions`] — one line per shadowed
    /// key. Called from the pipeline builder's derived view (boot-scoped, not
    /// per request).
    pub fn warn_on_lookup_collisions(&self) {
        for collision in self.lookup_collisions() {
            tracing::warn!(target: "router.config", collision = %collision, "requested-model shadowing");
        }
    }

    /// Boot cross-check: every capped role's `max_parallel` against the
    /// counted instance slots of its serving models (pool profiles'
    /// `parallel`, where a profile without an explicit `parallel` counts as
    /// 1 — the conservative floor, since unset means "inherit server
    /// global"). Warns when a cap admits more simultaneity than the counted
    /// slots (the tight-VRAM over-admit: requests queue at the gate while
    /// the server saturates). Serving set is the role's bound models plus,
    /// for `default`, the default-inherited models — the same membership
    /// rule boot materialization uses. Requires materialized pools. Called
    /// from the pipeline builder's derived view (boot-scoped, not per
    /// request).
    pub fn warn_on_role_capacity_mismatch(&self) {
        let mut names: Vec<&String> = self.roles.keys().collect();
        names.sort();
        for role in names {
            let cap = match self.roles[role].concurrency.as_ref() {
                Some(concurrency) => concurrency.max_parallel,
                None => continue,
            };
            let mut serving =
                crate::config::models_serving_role(&self.roles, &self.models, role);
            if role == "default" {
                let mut inherited: Vec<String> = self
                    .models
                    .keys()
                    .filter(|key| {
                        !self.roles.values().any(|r| r.models.contains_key(*key))
                            && !serving.iter().any(|s| s == *key)
                    })
                    .cloned()
                    .collect();
                serving.append(&mut inherited);
            }
            let slots: usize = serving
                .iter()
                .filter_map(|key| self.models.get(key))
                .flat_map(crate::config::ModelEntry::effective_pool)
                .map(|profile| {
                    profile
                        .parallel
                        .map_or(1, |slots| usize::try_from(slots).unwrap_or(1))
                })
                .sum();
            if cap > slots {
                tracing::warn!(
                    target: "router.config",
                    role = %role,
                    max_parallel = cap,
                    counted_slots = slots,
                    "role admits more simultaneous dispatches than counted instance slots; profiles without explicit parallel count as 1 — raise parallel or lower the cap if the server global differs",
                );
            }
        }
    }

    /// Expand a group's raw members through the roles table: a bare member
    /// naming a role fans out to the models the role names (sorted model-key
    /// order — see [`crate::config::models_serving_role`]); sentinel members
    /// (`last`/`any`), qualified members (`base:point`), and unknown literals
    /// pass through untouched for the downstream stage (sentinel expansion /
    /// explicit-point / fail-closed lookup) to own. Without roles the
    /// expansion is identity. A bare role name shadows a same-named model
    /// key — address the model directly with a qualified member when both
    /// exist.
    pub fn role_expanded_members(&self, group: &str) -> Vec<String> {
        expanded_group_members(&self.model_groups, &self.roles, &self.models, group)
    }
}

/// Ordered member expansion for one group over raw tables — the shared core
/// behind [`RoutingConfig::role_expanded_members`], so map-only call sites
/// (the server pipeline carries maps, not a `RoutingConfig`) expand
/// identically without restating the rule.
#[allow(clippy::implicit_hasher)]
pub fn expanded_group_members(
    groups: &HashMap<String, ModelGroup>,
    roles: &HashMap<String, RoleEntry>,
    models: &HashMap<String, ModelEntry>,
    group: &str,
) -> Vec<String> {
    let Some(group_cfg) = groups.get(group) else {
        return Vec::new();
    };
    let effective = group_cfg.effective_models();
    let mut out = Vec::with_capacity(effective.len());
    for member in &effective {
        if member.contains(':') || member == "last" || member == "any" {
            out.push(member.clone());
        } else if roles.contains_key(member.as_str()) {
            out.extend(crate::config::models_serving_role(
                roles,
                models,
                member.as_str(),
            ));
        } else {
            out.push(member.clone());
        }
    }
    out
}

/// Cheapest-passing member selection over expanded members — the shared core
/// behind the static group resolution: the cheapest member whose base
/// `models` entry's `intelligence` meets `min_complexity`, else the cheapest
/// member in the group. Availability sentinels (`last`/`any`) have no meaning
/// on the static path (no recency/liveness) and are skipped — the dispatch
/// climb owns them. Returns the full member key (possibly qualified).
#[allow(clippy::implicit_hasher)]
pub fn select_group_member(
    group: &str,
    members: &[String],
    models: &HashMap<String, ModelEntry>,
    min_complexity: Option<u8>,
) -> Option<String> {
    let model_keys: Vec<&String> = members
        .iter()
        .filter(|m| *m != "last" && *m != "any")
        .collect();

    tracing::debug!(target: "router.config",
        group = %group,
        model_count = model_keys.len(),
        min_complexity = ?min_complexity,
        "resolving group member"
    );

    let intelligence = |key: &String| {
        let (base, _) = split_model_key(key);
        models.get(base).map(|m| m.intelligence)
    };
    let passing: Vec<&&String> = model_keys
        .iter()
        .filter(|n| {
            intelligence(n).is_some_and(|i| i >= min_complexity.unwrap_or(0))
        })
        .collect();

    let cost = |key: &String| {
        let (base, _) = split_model_key(key);
        models
            .get(base)
            .map_or(f64::MAX, |m| m.cost_input + m.cost_output)
    };
    let cheapest = |a: &&&String, b: &&&String| {
        cost(a).partial_cmp(&cost(b)).unwrap_or(std::cmp::Ordering::Equal)
    };

    if passing.is_empty() {
        tracing::debug!(target: "router.config", group = %group, "no candidates passed complexity filter, falling back to cheapest in group");
        model_keys.iter().min_by(cheapest).map(|s| (*s).clone())
    } else {
        let entry_key = passing.into_iter().min_by(cheapest)?;
        tracing::info!(target: "router.config", group = %group, model = %entry_key, "group member resolved (complexity match)");
        Some((*entry_key).clone())
    }
}

/// The `model_group` a route resolves through — the route's own `RouteRef`
/// group, or the default route's group when the route is unknown (mirrors
/// the `resolve_route` lookup at the top of [`RoutingConfig::resolve_route`]).
///
/// The target-matching ladder uses this to find the ordered candidate
/// list for a resolved route; `None` when neither the route nor the
/// default route has a group entry.
impl RoutingConfig {
    pub fn route_group(&self, route: &str) -> Option<&str> {
        self.routes
            .get(route)
            .or_else(|| self.routes.get(&self.default_route))
            .map(|r| r.group.as_str())
    }

    /// Resolve a possibly-qualified model key (`base:qualifier`) to its
    /// `ModelEntry`. A bare key resolves directly; a qualified key strips the
    /// qualifier to find the owning model's entry. `None` when the base key has
    /// no `models` entry.
    pub fn entry_for_key(&self, key: &str) -> Option<&ModelEntry> {
        let (base, _) = split_model_key(key);
        self.models.get(base)
    }

    /// Build the dispatch `RoutingTarget` for a possibly-qualified model key
    /// (`base:qualifier`). A qualifier targets the named instance/group of the
    /// base model (`from_model_entry_instance`); `latest` and bare keys resolve
    /// to the entry's default dispatch point (`from_model_entry`). This is the
    /// canonical builder for `model_groups` members, so a group can pin a
    /// specific instance (e.g. `lfm2.5-2.6b:default`).
    pub fn target_for_key(&self, key: &str) -> Option<RoutingTarget> {
        let entry = self.entry_for_key(key)?;
        let (base, _) = split_model_key(key);
        Some(match split_model_key(key).1 {
            Some("latest") | None => RoutingTarget::from_model_entry(base, entry),
            Some(qualifier) => RoutingTarget::from_model_entry_instance(base, entry, qualifier),
        })
    }

    /// Resolve a route name to a fully-populated typed `RoutingTarget` —
    /// the same ordered lookup as [`Self::resolve_route`]: the hatch
    /// addresses the models table directly (fail-closed), then exact routes
    /// (with terminal role + fallbacks), then bare groups, then bare
    /// models, then the default route. Non-route hits carry no fallbacks
    /// (the direct-resolution precedent) and no terminal role.
    pub fn routing_target(&self, route: &str, min_complexity: Option<u8>) -> Option<RoutingTarget> {
        // The hatch addresses the models table directly (the remainder may
        // itself be qualified): explicit addressing never falls back.
        if let Some(rest) = strip_model_prefix(route) {
            let mut rt = self.target_for_key(rest)?;
            rt.group = None;
            rt.target_name = Some(rest.to_string());
            rt.role = None;
            rt.fallbacks = Vec::new();
            return Some(rt);
        }

        // Exact route (no default fallback here — it runs last, below).
        if self.routes.contains_key(route) {
            return self.route_target(route, min_complexity);
        }

        // Bare group name: cheapest-passing member serves, group attached.
        if self.model_groups.contains_key(route) {
            let member = self.resolve_group_member(route, min_complexity)?;
            let mut rt = self.target_for_key(&member)?;
            rt.group = Some(route.to_string());
            rt.target_name = Some(route.to_string());
            return Some(rt);
        }

        // Bare model name (bare or qualified through the key grammar).
        // `target_for_key` only resolves through a models entry — a miss
        // means the name is unknown, not a route/group/model.
        if let Some(mut rt) = self.target_for_key(route) {
            rt.group = None;
            rt.target_name = Some(route.to_string());
            return Some(rt);
        }

        // Default fallback (existing behavior): the requested name is
        // preserved on the default target.
        if route != self.default_route {
            let default = self.default_route.clone();
            if let Some(mut rt) = self.route_target(&default, min_complexity) {
                rt.target_name = Some(route.to_string());
                return Some(rt);
            }
        }
        None
    }

    /// The exact-route leg of [`Self::routing_target`]: onnx bypass, member
    /// resolution, terminal-role qualification, group/target attachment, and
    /// ordered fallbacks. `None` for route names with no `routes` entry.
    fn route_target(&self, route: &str, min_complexity: Option<u8>) -> Option<RoutingTarget> {
        if !self.routes.contains_key(route) {
            return None;
        }
        // A route whose group resolves to an in-process onnx role (e.g. the
        // generative `onnx/llm` routing model) dispatches to that role — it is
        // not a `models` entry, so it bypasses the ModelEntry ladder below.
        // The onnx path is unchanged by terminal roles.
        if let Some(rt) = self.resolve_onnx_route_target(route) {
            return Some(rt);
        }
        let member = self.resolve_route_member(route, min_complexity)?;
        let mut rt = self.target_for_key(&member)?;
        if let Some(role) = self
            .routes
            .get(route)
            .and_then(|r| r.role.clone())
        {
            self.apply_terminal_role(&mut rt, &member, &role);
        }
        let group = self
            .routes
            .get(route)
            .map_or(String::new(), |r| r.group.clone());
        rt.group = Some(group);
        rt.target_name = Some(route.to_string());
        rt.fallbacks = self
            .all_dispatch_targets(route, min_complexity)
            .into_iter()
            .skip(1) // skip the primary (already included)
            .filter_map(|(name, _)| self.target_for_key(&name))
            .collect();
        Some(rt)
    }

    /// Qualifier the terminal `role` selects inside model `base`'s
    /// boot-materialized pool: the role-side binding's `select` resolved by
    /// the `instance_params_for` name-or-group rule, or the entry default
    /// point for default-inherited models. The shared core behind dispatch
    /// ([`Self::apply_terminal_role`], graceful on error) and boot validation
    /// ([`Self::validate_terminal_roles`], fail-closed). Requires
    /// boot-materialized pools (`apply_defaults` first).
    fn terminal_role_qualifier(&self, base: &str, role: &str) -> Result<String, String> {
        let role_entry = self.roles.get(role).ok_or_else(|| {
            format!("unknown role '{role}' (no roles.{role} entry)")
        })?;
        let entry = self
            .models
            .get(base)
            .ok_or_else(|| format!("unknown model '{base}'"))?;
        let selected: Option<String> = if let Some(binding) = role_entry.models.get(base) {
            binding.select.clone().or_else(|| {
                // Bound without a select: the role pool's default-marked
                // profile, else its single profile — the documented
                // absent-select rule, at key level.
                Self::role_pool_default(&role_entry.instances)
            })
        } else {
            // No binding: only default-inherited models serve the role (a
            // model bound nowhere inherits the `default` pool).
            let bound_anywhere = self
                .roles
                .values()
                .any(|r| r.models.contains_key(base));
            if role != "default" || bound_anywhere {
                return Err(format!("model '{base}' serves no role '{role}'"));
            }
            crate::config::root::default_inference_point(entry)
        };
        let name = selected.ok_or_else(|| {
            format!("role '{role}' selects no dispatch point in model '{base}'")
        })?;
        let found = entry.effective_pool().iter().any(|p| {
            p.name.as_deref() == Some(name.as_str())
                || p.group.as_deref() == Some(name.as_str())
        });
        if !found {
            return Err(format!(
                "role '{role}' selects profile '{name}' with no pool contribution in model '{base}'"
            ));
        }
        Ok(name)
    }

    /// The role pool's default profile key: the `default: true` instance,
    /// else the single instance, else nothing — the key-level form of the
    /// absent-select rule.
    fn role_pool_default(
        instances: &std::collections::HashMap<String, crate::config::InstanceProfile>,
    ) -> Option<String> {
        if let Some((key, _)) = instances.iter().find(|(_, p)| p.default) {
            return Some(key.clone());
        }
        if instances.len() == 1 {
            return instances.keys().next().cloned();
        }
        None
    }

    /// Apply a terminal role to a resolved target, in place. The group has
    /// already picked the weights (`member` → `rt`); the role picks the
    /// window inside the winner's own pool and composes the role legs over
    /// its params (the M2 chain with the role sampling and the model-side
    /// override). An explicit member qualifier (`base:point`) wins for the
    /// where — the target keeps its qualifier and the role is only recorded
    /// for the audit trail. Unknown roles, unserved models, and missing
    /// profiles keep the target as built (graceful at dispatch — boot
    /// validation fails closed on the same shapes instead).
    fn apply_terminal_role(&self, rt: &mut RoutingTarget, member: &str, role: &str) {
        rt.role = Some(role.to_string());
        let (base, qualifier) = split_model_key(member);
        if qualifier.is_some() {
            return;
        }
        let Some(entry) = self.models.get(base) else {
            return;
        };
        let Ok(qualifier) = self.terminal_role_qualifier(base, role) else {
            return;
        };
        let mut qualified = RoutingTarget::from_model_entry_instance(base, entry, &qualifier);
        let Some(role_entry) = self.roles.get(role) else {
            return;
        };
        let role_sampling = role_entry.params.sampling_value();
        let override_params = entry
            .role_params
            .as_ref()
            .and_then(|overrides| overrides.get(role))
            .and_then(|o| o.params.as_ref());
        if let Some(params) =
            entry.answer_params_for(Some(&qualifier), Some(role), role_sampling.as_ref(), None)
        {
            qualified.params = Some(params);
        }
        qualified.filter_thinking = entry
            .filter_thinking_for(entry.resolve_thinking(role_sampling.as_ref(), override_params).as_deref(), role_sampling.as_ref(), override_params);
        qualified.role = Some(role.to_string());
        *rt = qualified;
    }

    /// Boot reference check for terminal roles (wired into the startup gate;
    /// unit-tested here): every terminal declaring `role` names a `roles`
    /// entry, and every bare model member of its group serves the role with
    /// a resolvable profile. Requires boot-materialized pools. Deterministic:
    /// routes visit in sorted order, so the first reported failure is stable.
    /// Explicitly qualified members, sentinels, onnx members, and unknown
    /// model keys are skipped here — owned by the explicit-point rule, the
    /// stateful climb, the onnx path, and the reference check respectively.
    pub fn validate_terminal_roles(
        &self,
        tree: &crate::config::ClassificationTree,
    ) -> Result<(), String> {
        let mut views = tree.terminal_views();
        views.sort_by(|a, b| a.0.cmp(&b.0));
        for (route, group, _) in views {
            let Some(role) = tree.terminal_role(&route) else {
                continue;
            };
            if !self.roles.contains_key(&role) {
                return Err(format!(
                    "terminal route '{route}' names unknown role '{role}' (no roles.{role} entry)"
                ));
            }
            let group_name = group.unwrap_or_else(|| route.clone());
            let mut members = self.role_expanded_members(&group_name);
            members.sort();
            for member in members {
                if member == "last" || member == "any" {
                    continue;
                }
                let (base, qualifier) = split_model_key(&member);
                if qualifier.is_some() || self.onnx_keys.contains(base) {
                    continue;
                }
                if !self.models.contains_key(base) {
                    continue;
                }
                if let Err(reason) = self.terminal_role_qualifier(base, &role) {
                    return Err(format!(
                        "terminal route '{route}' role '{role}': {reason}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Boot reference gate: every route, group member, and role reference
    /// resolves, and every route dry-run terminates with a target — no model
    /// in the loop. Deterministic: routes and groups visit in sorted order,
    /// so the first reported failure is stable across runs. Fail-closed:
    /// any `Err` must stop boot (the caller prints it as `FATAL` with the
    /// config path). Passes, in order:
    /// (a) every route's group names a `model_groups` entry and every
    /// route's role names a `roles` entry;
    /// (b) every group member names a `models` entry, an onnx key, or a
    /// sentinel (role members fan out first — unknown literals fail);
    /// (c) terminal roles validate (when a tree is present);
    /// (d) every route resolves to a target (onnx-resolved routes pass by
    /// construction — they carry no `ModelEntry` by design).
    pub fn validate_for_boot(
        &self,
        tree: Option<&crate::config::ClassificationTree>,
    ) -> Result<(), String> {
        let mut route_names: Vec<&String> = self.routes.keys().collect();
        route_names.sort();
        for route in &route_names {
            let rref = &self.routes[*route];
            if !self.model_groups.contains_key(&rref.group) {
                return Err(format!(
                    "route '{route}' names unknown model_group '{}'",
                    rref.group
                ));
            }
            if let Some(role) = rref.role.as_deref() {
                if !self.roles.contains_key(role) {
                    return Err(format!(
                        "route '{route}' names unknown role '{role}' (no roles.{role} entry)"
                    ));
                }
            }
        }
        let mut group_names: Vec<&String> = self.model_groups.keys().collect();
        group_names.sort();
        for group in &group_names {
            for member in self.role_expanded_members(group) {
                if member == "last" || member == "any" {
                    continue;
                }
                let (base, _) = split_model_key(&member);
                if self.models.contains_key(base) || self.onnx_keys.contains(base) {
                    continue;
                }
                return Err(format!(
                    "model_group '{group}' names unknown model '{member}' (no models entry or onnx key)"
                ));
            }
        }
        if let Some(tree) = tree {
            self.validate_terminal_roles(tree)?;
        }
        for route in &route_names {
            if self.resolve_onnx_route_target(route).is_some() {
                continue;
            }
            if self.resolve_route(route, None).is_none() {
                return Err(format!("route '{route}' resolves to no target"));
            }
        }
        Ok(())
    }

    /// Whether the route's group resolves to a configured in-process onnx role,
    /// and if so build the onnx `RoutingTarget` for it. `None` when the route
    /// (or its group) does not name an onnx role key. The route's `group` and
    /// `target_name` are attached so downstream validation (intent→model_group,
    /// route name) sees the same shape as a ModelEntry-resolved target.
    fn resolve_onnx_route_target(&self, route: &str) -> Option<RoutingTarget> {
        let route_ref = self
            .routes
            .get(route)
            .or_else(|| self.routes.get(&self.default_route))?;
        let key = self
            .role_expanded_members(&route_ref.group)
            .into_iter()
            .find(|k| {
                let (base, _) = split_model_key(k);
                self.onnx_keys.contains(base)
            })?;
        let (base, qualifier) = split_model_key(&key);
        let mut rt = RoutingTarget::from_onnx_role(base);
        if let Some(q) = qualifier {
            rt.instance = Some(q.to_string());
        }
        rt.group = Some(route_ref.group.clone());
        rt.target_name = Some(route.to_string());
        Some(rt)
    }

    /// Resolve a requested name to its serving entry — the ordered lookup:
    /// the [`MODEL_PREFIX`] hatch addresses the models table directly, then
    /// exact routes, then model groups, then models, then the default route.
    /// First hit wins (a route shadowing a group/model always wins —
    /// [`Self::lookup_collisions`] names every shadow). The hatch and the
    /// total miss are fail-closed (no default fallback); all other misses
    /// fall back to the default route, as before.
    pub fn resolve_route(
        &self,
        route_name: &str,
        min_complexity: Option<u8>,
    ) -> Option<(&ModelEntry, String)> {
        // The hatch addresses the models table directly (the remainder may
        // itself be qualified): explicit addressing never falls back.
        if let Some(rest) = strip_model_prefix(route_name) {
            return self.entry_for_key(rest).map(|entry| {
                let name = entry.name.clone().unwrap_or_else(|| split_model_key(rest).0.to_string());
                tracing::info!(target: "router.config", route = %route_name, model = %name,
                    "route resolved as direct model (explicit hatch)"
                );
                (entry, name)
            }).or_else(|| {
                tracing::warn!(target: "router.config", route = %route_name,
                    "model hatch names no models entry"
                );
                None
            });
        }

        // Exact route (no default fallback here — it runs last, below).
        if let Some(route_ref) = self.routes.get(route_name) {
            if self.resolve_onnx_route_target(route_name).is_some() {
                tracing::warn!(target: "router.config", route = %route_name, group = %route_ref.group, "route resolved to an onnx role (no ModelEntry)");
                return None;
            }

            let member = self.resolve_route_member(route_name, min_complexity)?;
            let entry = self.entry_for_key(&member)?;
            let name = entry.name.clone().unwrap_or_else(|| split_model_key(&member).0.to_string());
            tracing::info!(target: "router.config", route = %route_name, model = %name, member = %member, "route resolved");
            return Some((entry, name));
        }

        // Bare group name: cheapest-passing member serves.
        if self.model_groups.contains_key(route_name) {
            let member = self.resolve_group_member(route_name, min_complexity)?;
            let entry = self.entry_for_key(&member).or_else(|| {
                tracing::warn!(target: "router.config", route = %route_name, member = %member, "group resolved to an onnx role (no ModelEntry)");
                None
            })?;
            let name = entry.name.clone().unwrap_or_else(|| split_model_key(&member).0.to_string());
            tracing::info!(target: "router.config", route = %route_name, model = %name, member = %member, "group resolved");
            return Some((entry, name));
        }

        // Bare model name.
        if let Some(entry) = self.models.get(route_name) {
            let name = entry.name.clone().unwrap_or_else(|| route_name.to_string());
            tracing::info!(target: "router.config", route = %route_name, model = %name,
                "route resolved as direct model"
            );
            return Some((entry, name));
        }

        // Default fallback (existing behavior), then the fail-closed warning.
        if route_name != self.default_route {
            tracing::debug!(target: "router.config", route = %route_name,
                default = %self.default_route, "falling back to the default route");
            return self.resolve_route(&self.default_route, min_complexity);
        }
        tracing::warn!(target: "router.config", route = %route_name,
            default = %self.default_route,
            "no route or model found for target"
        );
        None
    }

    /// Choose the `model_groups` member a route dispatches to: the cheapest
    /// member whose base `models` entry's `intelligence` meets `min_complexity`,
    /// else the cheapest member in the group. Returns the full member key
    /// (possibly qualified, e.g. `lfm2.5-2.6b:default`) so the caller can
    /// preserve any instance qualifier. Role members fan out to their
    /// candidate keys first; availability sentinels (`last`/`any`) have no
    /// meaning on this static path (no recency/liveness) and are skipped —
    /// the dispatch climb owns them.
    fn resolve_route_member(
        &self,
        route_name: &str,
        min_complexity: Option<u8>,
    ) -> Option<String> {
        let route_ref = self
            .routes
            .get(route_name)
            .or_else(|| self.routes.get(&self.default_route))?;
        self.resolve_group_member(&route_ref.group, min_complexity)
    }

    /// Choose the member a model group dispatches to — the static half of
    /// [`Self::resolve_route_member`] parameterized by group, so bare group
    /// names resolve through the same rule. `None` for unknown groups.
    pub fn resolve_group_member(
        &self,
        group: &str,
        min_complexity: Option<u8>,
    ) -> Option<String> {
        if !self.model_groups.contains_key(group) {
            tracing::warn!(target: "router.config", group = %group, "model group not found");
            return None;
        }
        let expanded = self.role_expanded_members(group);
        select_group_member(group, &expanded, &self.models, min_complexity)
    }

    /// Return ALL available dispatch targets across all model groups ordered by
    /// dispatch preference:
    ///
    /// 1. Models from the resolved route's own group (cheapest first)
    /// 2. Models from other groups sorted by intelligence proximity to the
    ///    target complexity (closest first, cheapest tie-break)
    ///
    /// When the primary target fails (rate-limited, timeout, etc.) the caller
    /// can iterate this list to find a working model.
    pub fn all_dispatch_targets(
        &self,
        route_name: &str,
        min_complexity: Option<u8>,
    ) -> Vec<(String, ModelEntry)> {
        // Resolve the route to find its group
        let route_ref = self
            .routes
            .get(route_name)
            .or_else(|| self.routes.get(&self.default_route));

        let primary_group = route_ref.map(|r| r.group.as_str());

        // Collect all (model_key, model_entry) with resolved names.
        // Group iteration is sorted (primary first, then key order) so the
        // dedup below — and hence fallback order — is deterministic across
        // runs (HashMap iteration order is random per process).
        let mut group_keys: Vec<&String> = self.model_groups.keys().collect();
        group_keys.sort();
        group_keys.sort_by_key(|g| primary_group != Some(g.as_str()));
        let mut seen = std::collections::HashSet::new();
        let mut entries: Vec<(String, ModelEntry, f64)> = Vec::new();

        let target_intelligence = f64::from(min_complexity.unwrap_or(0));

        for group_key in group_keys {
            let is_primary = primary_group == Some(group_key.as_str());
            // Role members fan out to candidate keys; sentinels and unknown
            // literals fall out below through the entry lookup, as today.
            for model_key in self.role_expanded_members(group_key) {
                if !seen.insert(model_key.clone()) {
                    continue;
                }
                if let Some(entry) = self.entry_for_key(&model_key) {
                    // Keep the full (possibly-qualified) member key so the
                    // caller can preserve the instance qualifier when building
                    // the fallback target.
                    let name = model_key.clone();
                    // Compute distance from target intelligence for cross-group sorting
                    let dist = if is_primary {
                        -f64::from(entry.intelligence) // primary group: negative so they sort first
                    } else {
                        (f64::from(entry.intelligence) - target_intelligence).abs()
                    };
                    entries.push((name, entry.clone(), dist));
                }
            }
        }

        // Sort: primary group first (dist < 0), then by intelligence proximity,
        // then by cost, then by key — the key tie-break keeps fallback order
        // deterministic when two candidates cost the same.
        entries.sort_by(|a, b| {
            let a_primary = a.2 < 0.0;
            let b_primary = b.2 < 0.0;
            match (a_primary, b_primary) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => {
                    let d = a.2.partial_cmp(&b.2).unwrap_or(Ordering::Equal);
                    if d == Ordering::Equal {
                        let cost_a = a.1.cost_input + a.1.cost_output;
                        let cost_b = b.1.cost_input + b.1.cost_output;
                        cost_a
                            .partial_cmp(&cost_b)
                            .unwrap_or(Ordering::Equal)
                            .then_with(|| a.0.cmp(&b.0))
                    } else {
                        d
                    }
                }
            }
        });

        entries
            .into_iter()
            .map(|(name, entry, _)| (name, entry))
            .collect()
    }
}
#[cfg(test)]
#[path = "../../tests/config_routing.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/config_boot_gate.rs"]
mod boot_gate_tests;

#[cfg(test)]
#[path = "../../tests/routing_role_golden.rs"]
mod routing_role_golden_tests;
