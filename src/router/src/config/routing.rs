//! Route resolution and routing configuration types.
//! `RoutingConfig` is the resolved routing table used by the classifier stage.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::ModelEntry;
use super::RoleParams;
use crate::config::split_model_key;
use crate::config::EscalationLadderConfig;
use crate::pipeline::RoutingTarget;
use crate::score_matrix::ScoreMatrix;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRef {
    /// Role this route dispatches through: the role's `models` list is the
    /// dispatch candidate set (the role picks the weights AND the window).
    pub role: String,
    /// Pipelines this route may run (kept for pipeline selection; the
    /// shipped tree uses `default` everywhere).
    #[serde(default = "default_pipelines")]
    pub pipelines: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// Never let the classifier answer requests on this route directly: force
    /// dispatch to the route's role. For domains where the classifier model is
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
/// server pipeline): routes, then roles, then models. One classifier
/// so the three paths can never disagree about what a name means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedRef {
    /// `model:<remainder>`: address the models table directly (the remainder
    /// may itself be qualified). Bypasses the order below.
    DirectModel(String),
    /// Exact `routes` hit (route shadowing a role/model wins — warned).
    Route(String),
    /// Exact `roles` hit (role shadowing a model wins — warned).
    Role(String),
    /// Exact `models` hit.
    Model(String),
    /// No namespace hit (callers fall back to the default route, as before).
    Unknown(String),
}

/// Strip the [`MODEL_PREFIX`] hatch (`None` for bare names and for a lone
/// `model:` with an empty remainder — an explicit form misused, resolved as
/// the bare name through the normal order).
pub fn strip_model_prefix(name: &str) -> Option<&str> {
    name.strip_prefix(MODEL_PREFIX)
        .filter(|rest| !rest.is_empty())
}

/// One entry of the routing-vocabulary table: a role names *what the request
/// needs* — its residency axis (`pinned`), its admission cap
/// (`max_parallel`), its thinking policy, its run block (`params`), and the
/// models that serve it (`models`). Membership lives on the role side
/// (`roles.<role>.models` names the serving model keys); models never name
/// roles for membership, so the two tables stay separate and compose only
/// through the binding. Resolution to a concrete `base:qualifier` target
/// happens per request at dispatch time, never at boot, so lazy models load
/// correctly.
///
/// A role owns its run configuration: `params` is the role's "how a model
/// is run" block (launch knobs plus flat sampling keys — never a nested
/// `params` object; the fleet block lives as `roles.default.params`).
/// Sampling composes `models.default` → model entry → role params →
/// `models.<model>.roles.<role>.params` (each sparser layer wins; the
/// model-side override wins at dispatch), materialized at boot into each
/// model's effective pool. A role's qualifier is never stored here: it
/// resolves from the route, which selects a model from the role's models,
/// and the model's entry default supplies the point (explicit `base:point`
/// still wins).
///
/// `max_parallel` is the per-role admission cap: how many simultaneous
/// dispatches the role admits, enforced per-role globally at the dispatch
/// gate. `None` (the default) leaves the role unbounded. A zero cap would
/// admit nothing and wedge every dispatch through the role, so it fails
/// closed at parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleEntry {
    /// Maximum simultaneous dispatches admitted for the role. `None` =
    /// unbounded; `0` fails closed at parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<usize>,
    /// Boot the role's serving models at startup and exempt their windows
    /// from auto-sleep and eviction. Composes with the model entry's own
    /// `pinned` (either true pins the synthesized profile).
    #[serde(default)]
    pub pinned: bool,
    /// Whether dispatches through this role emit thinking blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    /// Whether thinking blocks are stripped for dispatches through this
    /// role (the model-side override wins when present).
    #[serde(default)]
    pub filter_thinking: bool,
    /// Keep generated thinking blocks for dispatches through this role
    /// (router-local; never forwarded to the server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_thinking: Option<bool>,
    /// The role's run block: launch knobs plus flat sampling keys (no
    /// nested `params` object) every dispatch through this role composes
    /// over. Absent (the default) contributes no sampling base.
    #[serde(default)]
    pub params: RoleParams,
    /// The models serving this role, keyed by `models` key. A model listed
    /// in no role's `models` list serves only its entry default. Empty
    /// (the default) dispatches through the `last` availability sentinel —
    /// see [`RoleEntry::effective_models`].
    #[serde(default)]
    pub models: Vec<String>,
    /// Snapshot save mode for the role's serving models
    /// (`snapshot: {mode: manual|auto}`). `None` contributes nothing; a
    /// model's own entry wins over every role binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::config::SnapshotConfig>,
    /// Escalation ladder attached to this role (`escalation: {...}`).
    /// `None` (the default) leaves dispatch falling back to
    /// `fallback_completion` as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<EscalationLadderConfig>,
}

impl RoleEntry {
    /// Whether `model_key` serves this role.
    pub fn serves(&self, model_key: &str) -> bool {
        self.models.iter().any(|m| m == model_key)
    }

    /// The member list this role dispatches through: the declared `models`,
    /// or `["last"]` when the role declares none — an unlisted role rides
    /// recency over the fleet instead of resolving to nothing (the same
    /// sentinel-only rule the retired groups used).
    pub fn effective_models(&self) -> Vec<String> {
        if self.models.is_empty() {
            vec!["last".to_string()]
        } else {
            self.models.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    pub routes: HashMap<String, RouteRef>,
    pub models: HashMap<String, ModelEntry>,
    pub system_prompt: String,
    pub safety_threshold: f64,
    pub default_route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_matrix: Option<ScoreMatrix>,
    /// Registry keys of the configured in-process onnx roles (e.g. `onnx/llm`).
    /// A role `models` member that names one of these is a valid dispatch
    /// target served by the onnx `ChatBackend` — it is not a `models` entry, so
    /// the route resolver treats it specially. Populated at config-build time
    /// from `RouterConfig.onnx`; empty for a config with no onnx fleet.
    #[serde(default)]
    pub onnx_keys: BTreeSet<String>,
    /// Routing-vocabulary table (mirrors `RouterConfig.roles`): role name →
    /// candidate model keys + the inference point each candidate serves.
    /// The role's `models` list (or `["last"]` when unlisted — see
    /// [`RoleEntry::effective_models`]) is the dispatch candidate set.
    #[serde(default)]
    pub roles: HashMap<String, RoleEntry>,
}

impl RoutingConfig {
    /// Classify a bare requested name into its namespace — routes, then
    /// roles, then models, first hit wins. The hatch bypasses the
    /// order; unknown names fall back to the default route at the call site
    /// (existing behavior, preserved).
    pub fn classify_requested(&self, name: &str) -> RequestedRef {
        if let Some(rest) = strip_model_prefix(name) {
            return RequestedRef::DirectModel(rest.to_string());
        }
        if self.routes.contains_key(name) {
            return RequestedRef::Route(name.to_string());
        }
        if self.roles.contains_key(name) {
            return RequestedRef::Role(name.to_string());
        }
        if self.models.contains_key(name) {
            return RequestedRef::Model(name.to_string());
        }
        RequestedRef::Unknown(name.to_string())
    }

    /// Every shadowed key from the ordered lookup, in sorted order (stable
    /// across runs): a route shadowing a role/model makes that role/model
    /// unreachable by bare name, and a role shadowing a model does the
    /// same. A shadowed model stays addressable via the [`MODEL_PREFIX`]
    /// hatch; a shadowed role has no bypass — rename the route or the
    /// role. Unshadowed names stay silent.
    pub fn lookup_collisions(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut role_keys: Vec<&String> = self.roles.keys().collect();
        role_keys.sort();
        for key in role_keys {
            if self.routes.contains_key(key) {
                out.push(format!(
                    "requested model '{key}' resolves as route (shadows role '{key}'): the role is unreachable by bare name — rename the route or the role"
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
            } else if self.roles.contains_key(key) {
                out.push(format!(
                    "requested model '{key}' resolves as role (shadows model '{key}'): address the model as '{MODEL_PREFIX}{key}'"
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
            let Some(cap) = self.roles[role].max_parallel else {
                continue;
            };
            let mut serving = crate::config::models_serving_role(&self.roles, &self.models, role);
            if role == "default" {
                let mut inherited: Vec<String> = self
                    .models
                    .keys()
                    .filter(|key| {
                        !self
                            .roles
                            .values()
                            .any(|r| r.models.iter().any(|m| m == *key))
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

    /// Expand a role's effective members through the roles table: a bare
    /// member naming another role fans out to the models that role names
    /// (sorted model-key order — see [`crate::config::models_serving_role`]);
    /// sentinel members (`last`/`any`), qualified members (`base:point`), and
    /// unknown literals pass through untouched for the downstream stage
    /// (sentinel expansion / explicit-point / fail-closed lookup) to own.
    /// A role declaring no `models` list rides `["last"]` (see
    /// [`RoleEntry::effective_models`]). A bare role name shadows a
    /// same-named model key — address the model directly with a qualified
    /// member when both exist.
    pub fn role_dispatch_members(&self, role: &str) -> Vec<String> {
        expanded_role_members(&self.roles, &self.models, role)
    }
}

/// Ordered member expansion for one role over raw tables — the shared core
/// behind [`RoutingConfig::role_dispatch_members`], so map-only call sites
/// (the server pipeline carries maps, not a `RoutingConfig`) expand
/// identically without restating the rule.
#[allow(clippy::implicit_hasher)]
pub fn expanded_role_members(
    roles: &HashMap<String, RoleEntry>,
    models: &HashMap<String, ModelEntry>,
    role: &str,
) -> Vec<String> {
    let Some(role_entry) = roles.get(role) else {
        return Vec::new();
    };
    let effective = role_entry.effective_models();
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

/// The static fallback member for a group whose members are all availability
/// sentinels (`last`/`any`): the first declared model key in sorted order
/// (the `default` template key excluded — it is never servable). `last`
/// means "work with whatever model is loaded rather than load another"; with
/// no dispatch history the lightest-declared model is the fleet default. The
/// runtime target-match expansion supersedes this with true recency whenever
/// the ladder runs.
#[allow(clippy::implicit_hasher)]
pub fn fleet_default_member(models: &HashMap<String, ModelEntry>) -> Option<String> {
    let mut keys: Vec<&String> = models.keys().filter(|k| k.as_str() != "default").collect();
    keys.sort();
    keys.first().map(|k| (*k).clone())
}

/// Viability-ordered member selection over expanded members — the shared core
/// behind static role resolution and every availability-sentinel fallback.
///
/// Ordering (task-value, never producer confidence): intelligence-passing
/// members first (else the whole pool when nothing passes), then resident
/// members first (via `is_resident` over the base key; `None` treats every
/// candidate as resident, which reduces exactly to the legacy
/// cheapest-passing rule), then cheapest `cost_input + cost_output` with a
/// key-ascending tie-break for determinism. `scope` is the audit label only
/// (role name or `classifier`). Availability sentinels
/// (`last`/`any`) carry no static meaning: they are skipped when literal
/// members exist, and a sentinel-only role resolves over every servable
/// model (the `default` template excluded) — the runtime recency expansion
/// supersedes that whenever the target-match ladder runs. Returns the full
/// member key (possibly qualified).
#[allow(clippy::implicit_hasher)]
pub fn select_viable_member(
    scope: &str,
    members: &[String],
    models: &HashMap<String, ModelEntry>,
    min_complexity: Option<u8>,
    is_resident: Option<&dyn Fn(&str) -> bool>,
) -> Option<String> {
    // Sentinel-only group (no literal members): every servable model is a
    // candidate, key-sorted for a deterministic tie-break base; the viability
    // ordering below decides. Otherwise the literal members compete.
    let sentinel_only = !members.iter().any(|m| m != "last" && m != "any");
    let pool: Vec<&String> = if sentinel_only {
        let mut fleet: Vec<&String> = models.keys().filter(|k| k.as_str() != "default").collect();
        fleet.sort();
        fleet
    } else {
        members
            .iter()
            .filter(|m| *m != "last" && *m != "any")
            .collect()
    };
    if pool.is_empty() {
        return None;
    }

    // Passing subset (intelligence bar); when nothing passes the whole pool
    // competes so dispatch still has a cheapest target. Refs borrow from
    // `members` / `models` — no allocation.
    let passing: Vec<&String> = pool
        .iter()
        .copied()
        .filter(|key| {
            let (base, _) = split_model_key(key);
            models
                .get(base)
                .map(|m| m.intelligence)
                .is_some_and(|i| i >= min_complexity.unwrap_or(0))
        })
        .collect();
    let pool: Vec<&String> = if passing.is_empty() { pool } else { passing };

    let is_resident_key = |key: &String| {
        let (base, _) = split_model_key(key);
        is_resident.is_none_or(|probe| probe(base))
    };
    let cost_of = |key: &String| {
        let (base, _) = split_model_key(key);
        models
            .get(base)
            .map_or(f64::MAX, |m| m.cost_input + m.cost_output)
    };
    // Inline comparator (not a `let` binding): `min_by` needs a higher-ranked
    // `FnMut(&Item, &Item)`, which only infers at the call site.
    let entry_key = pool.iter().min_by(|a, b| {
        is_resident_key(a)
            .cmp(&is_resident_key(b))
            .reverse()
            .then_with(|| {
                cost_of(a)
                    .partial_cmp(&cost_of(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.cmp(b))
    })?;
    if sentinel_only {
        tracing::info!(target: "router.config",
            scope = %scope,
            model = %entry_key,
            "sentinel-only role resolves to the viable fleet member statically; runtime recency supersedes",
        );
    } else {
        tracing::info!(target: "router.config", scope = %scope, model = %entry_key, "role member resolved (viability match)");
    }
    Some((*entry_key).clone())
}

/// Cheapest-passing member selection over expanded members — kept as the
/// signature-compatible wrapper over [`select_viable_member`] with no
/// residency signal, so literal roles with no liveness input resolve
/// byte-identically to before. Sentinel-only roles now resolve by viability
/// (intended behavior change: lexicographic accident → viable member).
#[allow(clippy::implicit_hasher)]
pub fn select_role_member(
    scope: &str,
    members: &[String],
    models: &HashMap<String, ModelEntry>,
    min_complexity: Option<u8>,
) -> Option<String> {
    select_viable_member(scope, members, models, min_complexity, None)
}

/// The role a route resolves through — the route's own `RouteRef`
/// role, or the default route's role when the route is unknown (mirrors
/// the `resolve_route` lookup at the top of [`RoutingConfig::resolve_route`]).
///
/// The target-matching ladder uses this to find the ordered candidate
/// list for a resolved route; `None` when neither the route nor the
/// default route names a role.
impl RoutingConfig {
    pub fn route_role(&self, route: &str) -> Option<&str> {
        self.routes
            .get(route)
            .or_else(|| self.routes.get(&self.default_route))
            .map(|r| r.role.as_str())
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
    /// (`base:qualifier`). A qualifier targets the named instance/role of the
    /// base model (`from_model_entry_instance`); `latest` and bare keys resolve
    /// to the entry's default dispatch point (`from_model_entry`). This is the
    /// canonical builder for role members, so a role can pin a
    /// specific instance (e.g. `qwen3.5-9b:ledger`).
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
    /// (with terminal role + fallbacks), then bare roles, then bare
    /// models, then the default route. Non-route hits carry no fallbacks
    /// (the direct-resolution precedent) and no terminal role.
    pub fn routing_target(&self, route: &str, min_complexity: Option<u8>) -> Option<RoutingTarget> {
        // The hatch addresses the models table directly (the remainder may
        // itself be qualified): explicit addressing never falls back.
        if let Some(rest) = strip_model_prefix(route) {
            let mut rt = self.target_for_key(rest)?;
            rt.target_name = Some(rest.to_string());
            rt.role = None;
            rt.fallbacks = Vec::new();
            return Some(rt);
        }

        // Exact route (no default fallback here — it runs last, below).
        if self.routes.contains_key(route) {
            return self.route_target(route, min_complexity);
        }

        // Bare role name: cheapest-passing member serves, role attached.
        if self.roles.contains_key(route) {
            let member = self.resolve_role_member(route, min_complexity)?;
            let mut rt = self.target_for_key(&member)?;
            rt.role = Some(route.to_string());
            rt.target_name = Some(route.to_string());
            return Some(rt);
        }

        // Bare model name (bare or qualified through the key grammar).
        // `target_for_key` only resolves through a models entry — a miss
        // means the name is unknown, not a route/role/model.
        if let Some(mut rt) = self.target_for_key(route) {
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
        if let Some(route_ref) = self.routes.get(route) {
            let role = route_ref.role.clone();
            self.apply_terminal_role(&mut rt, &member, &role);
        }
        rt.target_name = Some(route.to_string());
        rt.fallbacks = self
            .all_dispatch_targets(route, min_complexity)
            .into_iter()
            .skip(1) // skip the primary (already included)
            .filter_map(|(name, _)| self.target_for_key(&name))
            .collect();
        Some(rt)
    }

    /// Whether model `base` serves `role`: the role binds it in its
    /// `models` list, or the role is the fleet `default` and the model is
    /// bound to no other role (the fleet-inherit rule). The single
    /// serve-check behind dispatch (`apply_terminal_role`) and boot
    /// validation (`validate_terminal_roles`).
    fn model_serves_role(&self, base: &str, role: &str) -> Result<(), String> {
        let role_entry = self
            .roles
            .get(role)
            .ok_or_else(|| format!("unknown role '{role}' (no roles.{role} entry)"))?;
        if role_entry.serves(base) {
            return Ok(());
        }
        let bound_anywhere = self.roles.values().any(|r| r.serves(base));
        if role == "default" && !bound_anywhere {
            return Ok(());
        }
        Err(format!("model '{base}' serves no role '{role}'"))
    }

    /// Apply a terminal role to a resolved target, in place. The role's
    /// `models` list has already picked the weights (`member` → `rt`); the
    /// role composes its params legs over the entry (the role run block,
    /// then the model-side override) and picks the window: a role that binds
    /// the model dispatches through the role's window (`<base>:<role>`); an
    /// unbound member serves through the entry default. A **pinned** role forces its
    /// qualifier wherever it binds — the profile is boot-materialized, so
    /// the target resolves identically pre/post boot. An unpinned role
    /// qualifies through a materialized window — matching by instance name
    /// or by group (sibling-expanded roles like `quick-0`/`quick-1` share
    /// the `quick` group the fork routes). Best effort: a role with no
    /// window in the pool leaves the entry default in place. An explicit
    /// member qualifier (`base:point`) wins for the where — the target
    /// keeps its qualifier and the role is only recorded for the audit
    /// trail. Unknown roles and unserved models keep the target as built
    /// (graceful at dispatch — boot validation fails closed on the
    /// unfillable pinned shapes instead).
    fn apply_terminal_role(&self, rt: &mut RoutingTarget, member: &str, role: &str) {
        rt.role = Some(role.to_string());
        let (base, qualifier) = split_model_key(member);
        if qualifier.is_some() {
            return;
        }
        let Some(entry) = self.models.get(base) else {
            return;
        };
        if self.model_serves_role(base, role).is_err() {
            return;
        }
        let Some(role_entry) = self.roles.get(role) else {
            return;
        };
        // A bound role dispatches through its window. Forcing a qualifier
        // the pool never materialized would target a nonexistent fork
        // instance, so the force applies only where the role binds the
        // model and the pool carries the window — matched by instance name
        // or by group (sibling-expanded roles share the group the fork
        // routes, e.g. `quick-0`/`quick-1` serve the `quick` qualifier).
        let role_profile = role_entry.serves(base)
            && (role_entry.pinned
                || entry
                    .effective_pool()
                    .iter()
                    .any(|p| p.name.as_deref() == Some(role) || p.group.as_deref() == Some(role)));
        let mut qualified = if role_profile {
            RoutingTarget::from_model_entry_instance(base, entry, role)
        } else {
            RoutingTarget::from_model_entry(base, entry)
        };
        if let Some(params) =
            entry.answer_params_for(role_profile.then_some(role), Some((role, role_entry)), None)
        {
            qualified.params = Some(params);
        }
        let over = entry.role_params.as_ref().and_then(|o| o.get(role));
        qualified.filter_thinking = entry.filter_thinking_for(
            entry.resolve_thinking(Some(role_entry), over).as_deref(),
            Some(role_entry),
            over,
        );
        qualified.role = Some(role.to_string());
        *rt = qualified;
    }

    /// Boot reference check for terminal roles (wired into the startup gate;
    /// unit-tested here): every terminal declaring `role` names a `roles`
    /// entry. An unserved bare member fails boot only when the pinned role
    /// can never fill it — a role with no runtime flexibility (no `last` /
    /// `any` sentinel, which recency may resolve to a serving model) names
    /// a window that will never exist. Sentinel-bearing roles may fill a
    /// pinned role at runtime, and unpinned roles fall back to the entry
    /// default at dispatch, so both pass. Deterministic: routes visit in
    /// sorted order, so the first reported failure is stable. Explicitly
    /// qualified members, sentinels, onnx members, and unknown model keys
    /// are skipped here — owned by the explicit-point rule, the stateful
    /// climb, the onnx path, and the reference check respectively.
    pub fn validate_terminal_roles(
        &self,
        tree: &crate::config::ClassificationTree,
    ) -> Result<(), String> {
        let mut views = tree.terminal_views();
        views.sort_by(|a, b| a.0.cmp(&b.0));
        for (route, _) in views {
            let Some(role) = tree.terminal_role(&route) else {
                continue;
            };
            if !self.roles.contains_key(&role) {
                return Err(format!(
                    "terminal route '{route}' names unknown role '{role}' (no roles.{role} entry)"
                ));
            }
            let mut members = self.role_dispatch_members(&role);
            members.sort();
            let runtime_flexible = members.iter().any(|m| m == "last" || m == "any");
            for member in &members {
                if member == "last" || member == "any" {
                    continue;
                }
                let (base, qualifier) = split_model_key(member);
                if qualifier.is_some() || self.onnx_keys.contains(base) {
                    continue;
                }
                if !self.models.contains_key(base) {
                    continue;
                }
                if self.model_serves_role(base, &role).is_ok() {
                    continue;
                }
                let pinned = self.roles.get(&role).is_some_and(|r| r.pinned);
                if pinned && !runtime_flexible {
                    let reason = self
                        .model_serves_role(base, &role)
                        .expect_err("serves check just failed");
                    return Err(format!("terminal route '{route}' role '{role}': {reason}"));
                }
            }
        }
        Ok(())
    }

    /// Boot reference gate: every route and role reference resolves, and
    /// every route dry-run terminates with a target — no model in the loop.
    /// Deterministic: routes and roles visit in sorted order, so the first
    /// reported failure is stable across runs. Fail-closed: any `Err` must
    /// stop boot (the caller prints it as `FATAL` with the config path).
    /// Passes, in order:
    /// (a) every route's role names a `roles` entry;
    /// (b) every role member names a `models` entry, an onnx key, another
    /// role, or a sentinel (role members fan out first — unknown literals
    /// fail);
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
            if !self.roles.contains_key(&rref.role) {
                return Err(format!(
                    "route '{route}' names unknown role '{}'",
                    rref.role
                ));
            }
        }
        let mut role_names: Vec<&String> = self.roles.keys().collect();
        role_names.sort();
        for role in &role_names {
            for member in self.role_dispatch_members(role) {
                if member == "last" || member == "any" {
                    continue;
                }
                let (base, _) = split_model_key(&member);
                if self.models.contains_key(base)
                    || self.onnx_keys.contains(base)
                    || self.roles.contains_key(base)
                {
                    continue;
                }
                return Err(format!(
                    "role '{role}' names unknown model '{member}' (no models entry, role, or onnx key)"
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

    /// Whether the route's role resolves to a configured in-process onnx role,
    /// and if so build the onnx `RoutingTarget` for it. `None` when the route
    /// (or its role) does not name an onnx role key. The route's role and
    /// `target_name` are attached so downstream validation (intent→role,
    /// route name) sees the same shape as a ModelEntry-resolved target.
    fn resolve_onnx_route_target(&self, route: &str) -> Option<RoutingTarget> {
        let route_ref = self
            .routes
            .get(route)
            .or_else(|| self.routes.get(&self.default_route))?;
        let key = self
            .role_dispatch_members(&route_ref.role)
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
        rt.role = Some(route_ref.role.clone());
        rt.target_name = Some(route.to_string());
        Some(rt)
    }

    /// Resolve a requested name to its serving entry — the ordered lookup:
    /// the [`MODEL_PREFIX`] hatch addresses the models table directly, then
    /// exact routes, then roles, then models, then the default route.
    /// First hit wins (a route shadowing a role/model always wins —
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
            return self
                .entry_for_key(rest)
                .map(|entry| {
                    let name = entry
                        .name
                        .clone()
                        .unwrap_or_else(|| split_model_key(rest).0.to_string());
                    tracing::info!(target: "router.config", route = %route_name, model = %name,
                        "route resolved as direct model (explicit hatch)"
                    );
                    (entry, name)
                })
                .or_else(|| {
                    tracing::warn!(target: "router.config", route = %route_name,
                        "model hatch names no models entry"
                    );
                    None
                });
        }

        // Exact route (no default fallback here — it runs last, below).
        if let Some(route_ref) = self.routes.get(route_name) {
            if self.resolve_onnx_route_target(route_name).is_some() {
                tracing::warn!(target: "router.config", route = %route_name, role = %route_ref.role, "route resolved to an onnx role (no ModelEntry)");
                return None;
            }

            let member = self.resolve_route_member(route_name, min_complexity)?;
            let entry = self.entry_for_key(&member)?;
            let name = entry
                .name
                .clone()
                .unwrap_or_else(|| split_model_key(&member).0.to_string());
            tracing::info!(target: "router.config", route = %route_name, model = %name, member = %member, "route resolved");
            return Some((entry, name));
        }

        // Bare role name: cheapest-passing member serves.
        if self.roles.contains_key(route_name) {
            let member = self.resolve_role_member(route_name, min_complexity)?;
            let entry = self.entry_for_key(&member).or_else(|| {
                tracing::warn!(target: "router.config", route = %route_name, member = %member, "role resolved to an onnx role (no ModelEntry)");
                None
            })?;
            let name = entry
                .name
                .clone()
                .unwrap_or_else(|| split_model_key(&member).0.to_string());
            tracing::info!(target: "router.config", route = %route_name, model = %name, member = %member, "role resolved");
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

    /// Choose the role `models` member a route dispatches to: the cheapest
    /// member whose base `models` entry's `intelligence` meets `min_complexity`,
    /// else the cheapest member in the role. Returns the full member key
    /// (possibly qualified, e.g. `qwen3.5-9b:ledger`) so the caller can
    /// preserve any instance qualifier. Availability sentinels (`last`/`any`)
    /// have no meaning on this static path (no recency/liveness) and are
    /// skipped — the dispatch climb owns them.
    fn resolve_route_member(&self, route_name: &str, min_complexity: Option<u8>) -> Option<String> {
        let route_ref = self
            .routes
            .get(route_name)
            .or_else(|| self.routes.get(&self.default_route))?;
        self.resolve_role_member(&route_ref.role, min_complexity)
    }

    /// Choose the member a role dispatches to — the static half of
    /// [`Self::resolve_route_member`] parameterized by role, so bare role
    /// names resolve through the same rule. `None` for unknown roles.
    pub fn resolve_role_member(&self, role: &str, min_complexity: Option<u8>) -> Option<String> {
        if !self.roles.contains_key(role) {
            tracing::warn!(target: "router.config", role = %role, "role not found");
            return None;
        }
        let expanded = self.role_dispatch_members(role);
        select_role_member(role, &expanded, &self.models, min_complexity)
    }

    /// Return ALL available dispatch targets across all roles ordered by
    /// dispatch preference:
    ///
    /// 1. Models from the resolved route's own role (cheapest first)
    /// 2. Models from other roles sorted by intelligence proximity to the
    ///    target complexity (closest first, cheapest tie-break) — the
    ///    fallback-to-more-capable walk over role `models` lists.
    ///
    /// When the primary target fails (rate-limited, timeout, etc.) the caller
    /// can iterate this list to find a working model.
    pub fn all_dispatch_targets(
        &self,
        route_name: &str,
        min_complexity: Option<u8>,
    ) -> Vec<(String, ModelEntry)> {
        // Resolve the route to find its role
        let route_ref = self
            .routes
            .get(route_name)
            .or_else(|| self.routes.get(&self.default_route));

        let primary_role = route_ref.map(|r| r.role.as_str());

        // Collect all (model_key, model_entry) with resolved names.
        // Role iteration is sorted (primary first, then key order) so the
        // dedup below — and hence fallback order — is deterministic across
        // runs (HashMap iteration order is random per process).
        let mut role_keys: Vec<&String> = self.roles.keys().collect();
        role_keys.sort();
        role_keys.sort_by_key(|g| primary_role != Some(g.as_str()));
        let mut seen = std::collections::HashSet::new();
        let mut entries: Vec<(String, ModelEntry, f64)> = Vec::new();

        let target_intelligence = f64::from(min_complexity.unwrap_or(0));

        for role_key in role_keys {
            let is_primary = primary_role == Some(role_key.as_str());
            // Role members fan out to candidate keys; sentinels and unknown
            // literals fall out below through the entry lookup, as today.
            for model_key in self.role_dispatch_members(role_key) {
                if !seen.insert(model_key.clone()) {
                    continue;
                }
                if let Some(entry) = self.entry_for_key(&model_key) {
                    // Keep the full (possibly-qualified) member key so the
                    // caller can preserve the instance qualifier when building
                    // the fallback target.
                    let name = model_key.clone();
                    // Compute distance from target intelligence for cross-role sorting
                    let dist = if is_primary {
                        -f64::from(entry.intelligence) // primary role: negative so they sort first
                    } else {
                        (f64::from(entry.intelligence) - target_intelligence).abs()
                    };
                    entries.push((name, entry.clone(), dist));
                }
            }
        }

        // Sort: primary role first (dist < 0), then by intelligence proximity,
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
