//! Route → tool-schema derivation — the single source of truth for the tool
//! catalogue handed to the Needle engine.
//!
//! Roadmap design decision 6: tool schemas are **derived** from the enriched
//! `routes` map (description + examples + parameters + intents) plus
//! `schema_overrides`. There is no parallel hand-maintained schema list — the
//! derivation is the only path that produces a tool schema, so the grammar the
//! engine constrains against can never drift from the routing table.
//!
//! Two surfaces are provided:
//!
//! - [`build_all_schemas`] — deterministic derivation of one
//!   [`NeedleRouteSchema`] per **non-general** route (route keys sorted,
//!   `schema_overrides` applied), the full enriched engine catalogue.
//!   General routes (`schema_overrides.<key>.general: true`) are **excluded by
//!   construction**: the engine grammar never contains a `general` route —
//!   general is classifier territory, and excluding it keeps the catalogue
//!   exactly the functionally-disjoint non-general tools (≤
//!   `candidates_per_rung`), so the engine's built-in contrastive head stays
//!   off and every tool is always grammar-reachable.
//! - [`build_candidate_schemas`] — the rung candidate set: the full catalogue
//!   when it fits within `candidates_per_rung` (every candidate grammar-
//!   rendered and reachable, O(1)); on overflow it is the seam where the
//!   Milestone-5 `ToolRetriever` shortlists to ≤ `candidates_per_rung` via
//!   HNSW. Until a retriever is injected, an overflowing catalogue is returned
//!   in full so the caller can detect the overflow and fall through to the
//!   classifier (never an arbitrary deterministic truncation — only the HNSW
//!   shortlister may reduce the set, design decision 4).
//!
//! - [`render_tools_json`] — the JSON array string handed to
//!   `needle_init` as `tools_json`.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::config::{NeedleConfig, NeedleRouteSchema, RouteRef};

/// The empty object schema used when a route declares no `parameters`
/// override. A valid object schema (`type: object`, no properties) keeps the
/// grammar well-formed while constraining nothing.
fn empty_object_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

/// Whether an override actually declares a parameter schema (a non-empty
/// object that is not the empty `{}` placeholder).
fn has_parameters(schema: &NeedleRouteSchema) -> bool {
    matches!(schema.parameters.as_object(), Some(map) if !map.is_empty())
}

/// Derive one enriched tool schema for a route key.
///
/// The single-source-of-truth rule: the route's `RouteRef` description is the
/// baseline, and a matching `schema_overrides` entry wins on every field it
/// declares (description, parameters, examples, intents). No field is ever
/// invented outside these two sources.
pub fn derive_route_schema(
    key: &str,
    route: &RouteRef,
    override_schema: Option<&NeedleRouteSchema>,
) -> NeedleRouteSchema {
    let description = override_schema
        .filter(|o| !o.description.is_empty())
        .map_or_else(|| route.description.clone(), |o| o.description.clone());
    let parameters = if override_schema.is_some_and(has_parameters) {
        override_schema.unwrap().parameters.clone()
    } else {
        empty_object_schema()
    };
    let examples = override_schema
        .map(|o| o.examples.clone())
        .unwrap_or_default();
    let intents = override_schema
        .map(|o| o.intents.clone())
        .unwrap_or_default();
    let output_template = override_schema.and_then(|o| o.output_template.clone());
    let general = override_schema.is_some_and(|o| o.general);
    NeedleRouteSchema {
        name: key.to_string(),
        description,
        examples,
        parameters,
        intents,
        output_template,
        general,
    }
}

/// Whether a tool is a `general` route — one Needle must never decide on its
/// own (it falls through to the classifier LLM). The single lookup used by
/// the `NeedlePreFilter` stage for the general-category gate; tools without
/// an override are treated as non-general.
pub fn is_general_route(config: &NeedleConfig, tool: &str) -> bool {
    schema_for(config, tool).is_some_and(|s| s.general)
}

/// Resolve the enriched schema for a tool name from `schema_overrides`.
///
/// Returns `None` for tools without an override — the caller treats such a
/// tool as non-general and template-less. This is the single lookup used by
/// the `NeedlePreFilter` stage to decide `output_template` (Milestone 2) and
/// `general` (Milestone 3) behavior from the tool name alone.
pub fn schema_for<'a>(config: &'a NeedleConfig, tool: &str) -> Option<&'a NeedleRouteSchema> {
    config.schema_overrides.get(tool)
}

/// Deterministic derivation of every **non-general** route's tool schema.
///
/// Route keys are sorted so the catalogue is independent of `HashMap`
/// iteration order — the engine and the HNSW shortlister both see a stable,
/// reproducible set.
///
/// The invariant: **the engine grammar never contains a `general` route.**
/// Routes whose `schema_overrides` entry carries `general: true` (e.g. the
/// `local` general Q&A route) are excluded from the catalogue, so the engine
/// can never name one — general is classifier territory by construction. The
/// `NeedlePreFilter` stage keeps `is_general_route` as defense-in-depth, but it
/// is unreachable in practice.
#[allow(clippy::implicit_hasher)]
pub fn build_all_schemas(
    config: &NeedleConfig,
    routes: &HashMap<String, RouteRef>,
) -> Vec<NeedleRouteSchema> {
    let mut keys: Vec<&String> = routes
        .keys()
        .filter(|key| !is_general_route(config, key))
        .collect();
    keys.sort_unstable();
    keys.into_iter()
        .map(|key| {
            derive_route_schema(key, &routes[key], config.schema_overrides.get(key))
        })
        .collect()
}

/// The rung candidate set for a query, at most `candidates_per_rung` schemas.
///
/// - Catalogue ≤ `candidates_per_rung` → the full catalogue (all candidates
///   grammar-rendered and reachable, no index needed).
/// - Catalogue > `candidates_per_rung` and a `ToolRetriever` is injected →
///   shortlisted to `candidates_per_rung` via HNSW (Milestone 5).
/// - Catalogue > `candidates_per_rung` and no retriever → the full catalogue
///   is returned; the caller (the `NeedlePreFilter` stage) observes the
///   overflow and **falls through to the classifier** — the degraded path is
///   never a silently truncated tool set.
///
/// `query` is the request text the shortlister embeds; it is unused until a
/// retriever is injected.
#[allow(clippy::implicit_hasher)]
#[allow(clippy::needless_pass_by_value)]
pub fn build_candidate_schemas(
    config: &NeedleConfig,
    routes: &HashMap<String, RouteRef>,
    retriever: Option<&dyn super::retriever::ToolRetriever>,
    query: &str,
) -> Vec<NeedleRouteSchema> {
    let all = build_all_schemas(config, routes);
    if all.len() <= config.candidates_per_rung {
        return all;
    }
    match retriever {
        Some(r) => r.shortlist(query, &all, config.candidates_per_rung),
        None => all,
    }
}

/// Render one enriched schema as the engine's tool-schema JSON object.
///
/// The engine schema is the **standard OpenAI tool format** (`name`,
/// `description`, `parameters`) — the distribution the base model was trained
/// on. `examples`/`intents` are deliberately **not** rendered here: measured
/// against the real engine, adding them to the engine schema regressed routing
/// coverage and calibration (the base model treats the custom keys as noise).
/// They remain first-class fields on `NeedleRouteSchema` for retrieval — the
/// HNSW shortlister embeds them via `schema_doc_text` (retriever.rs) — and for
/// the config-synced tests, but the engine only ever sees the plain format.
pub fn render_tool_schema(schema: &NeedleRouteSchema) -> Value {
    json!({
        "name": schema.name,
        "description": schema.description,
        "parameters": schema.parameters,
    })
}

/// Render a candidate set as the `tools_json` array string handed to
/// `needle_init`. Deterministic: candidates arrive in a stable order from
/// [`build_all_schemas`] / [`build_candidate_schemas`].
pub fn render_tools_json(schemas: &[NeedleRouteSchema]) -> String {
    let tools: Vec<Value> = schemas.iter().map(render_tool_schema).collect();
    serde_json::to_string(&tools).unwrap_or_else(|_| "[]".to_string())
}

/// Whether the non-general route catalogue overflows the rung cap (and
/// therefore requires shortlisting or a fall-through). The stage uses this to
/// decide between "every candidate is reachable" and "the tool set must be
/// reduced or skipped". General routes are excluded — they are never part of
/// the engine catalogue, so a config that fits within `candidates_per_rung`
/// once general is excluded keeps every reachable tool grammar-rendered.
#[allow(clippy::implicit_hasher)]
pub fn overflows_rung(config: &NeedleConfig, routes: &HashMap<String, RouteRef>) -> bool {
    let non_general = routes
        .keys()
        .filter(|key| !is_general_route(config, key))
        .count();
    non_general > config.candidates_per_rung
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_ref(description: &str) -> RouteRef {
        RouteRef {
            group: "g".into(),
            pipelines: vec!["default".into()],
            description: description.into(),
            always_route: false,
        }
    }

    fn routes() -> HashMap<String, RouteRef> {
        let mut m = HashMap::new();
        m.insert("zeta".into(), route_ref("z route"));
        m.insert("alpha".into(), route_ref("a route"));
        m.insert("mid".into(), route_ref("m route"));
        m
    }

    fn config() -> NeedleConfig {
        NeedleConfig::default()
    }

    #[test]
    fn derivation_is_deterministic() {
        let c = config();
        let r = routes();
        let first = build_all_schemas(&c, &r);
        let second = build_all_schemas(&c, &r);
        assert_eq!(first, second);
        // Sorted by route key regardless of HashMap insertion order.
        let names: Vec<&str> = first.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn derivation_uses_route_description_baseline() {
        let c = config();
        let r = routes();
        let schemas = build_all_schemas(&c, &r);
        let alpha = schemas.iter().find(|s| s.name == "alpha").unwrap();
        assert_eq!(alpha.description, "a route");
        assert_eq!(alpha.name, "alpha");
        // No override → empty object parameter schema.
        assert_eq!(alpha.parameters, empty_object_schema());
        assert!(alpha.examples.is_empty());
        assert!(alpha.intents.is_empty());
    }

    #[test]
    fn schema_overrides_win() {
        let mut c = config();
        c.schema_overrides.insert(
            "alpha".into(),
            NeedleRouteSchema {
                name: "alpha".into(),
                description: "overridden".into(),
                examples: vec!["do alpha now".into()],
                parameters: json!({
                    "type": "object",
                    "properties": {"target": {"type": "string"}},
                }),
                intents: vec!["alpha".into()],
                output_template: None,
                general: false,
            },
        );
        let r = routes();
        let schemas = build_all_schemas(&c, &r);
        let alpha = schemas.iter().find(|s| s.name == "alpha").unwrap();
        assert_eq!(alpha.description, "overridden");
        assert_eq!(
            alpha.parameters["properties"]["target"]["type"],
            "string"
        );
        assert_eq!(alpha.examples, vec!["do alpha now"]);
        assert_eq!(alpha.intents, vec!["alpha"]);
        // Non-overridden routes keep their route-derived description.
        let mid = schemas.iter().find(|s| s.name == "mid").unwrap();
        assert_eq!(mid.description, "m route");
    }

    #[test]
    fn empty_override_fields_do_not_override_route_baseline() {
        // An override present in the map but with an empty description must
        // not blank out the route's own description.
        let mut c = config();
        c.schema_overrides.insert(
            "alpha".into(),
            NeedleRouteSchema {
                name: "alpha".into(),
                description: String::new(),
                examples: vec![],
                parameters: json!({}),
                intents: vec![],
                output_template: None,
                general: false,
            },
        );
        let r = routes();
        let schemas = build_all_schemas(&c, &r);
        let alpha = schemas.iter().find(|s| s.name == "alpha").unwrap();
        assert_eq!(alpha.description, "a route", "route baseline preserved");
        assert_eq!(alpha.parameters, empty_object_schema());
    }

    #[test]
    fn candidates_never_exceed_rung_cap_when_catalogue_fits() {
        let c = config(); // candidates_per_rung defaults to 5
        let r = routes(); // 3 routes
        let candidates = build_candidate_schemas(&c, &r, None, "do alpha");
        assert_eq!(candidates.len(), 3);
        assert!(candidates.len() <= c.candidates_per_rung);
        assert!(!overflows_rung(&c, &r));
    }

    #[test]
    fn overflowing_catalogue_without_retriever_returns_full_set() {
        // The degraded path: no shortlister injected → the full catalogue is
        // returned so the stage can detect the overflow and fall through to
        // the classifier (never an arbitrary truncation).
        let mut r = routes();
        for i in 0..10 {
            r.insert(format!("extra{i}"), route_ref(&format!("route {i}")));
        }
        let c = config();
        assert!(overflows_rung(&c, &r));
        let candidates = build_candidate_schemas(&c, &r, None, "query");
        assert_eq!(candidates.len(), r.len(), "no shortlisting without a retriever");
    }

    #[test]
    fn general_flag_is_propagated_from_override() {
        let mut c = config();
        c.schema_overrides.insert(
            "alpha".into(),
            NeedleRouteSchema {
                name: "alpha".into(),
                description: String::new(),
                examples: vec![],
                parameters: json!({}),
                intents: vec![],
                output_template: None,
                general: true,
            },
        );
        let r = routes();
        // General routes are excluded from the engine catalogue by
        // construction — the grammar never contains a general tool.
        let schemas = build_all_schemas(&c, &r);
        assert!(
            schemas.iter().all(|s| s.name != "alpha"),
            "general routes must never enter the engine catalogue"
        );
        assert_eq!(
            schemas.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["mid", "zeta"]
        );
        let mid = schemas.iter().find(|s| s.name == "mid").unwrap();
        assert!(!mid.general, "non-overridden routes default to non-general");

        // `is_general_route` looks up the override (not the derived schema) —
        // the defense-in-depth check the stage keeps even though the grammar
        // can no longer name a general route.
        assert!(is_general_route(&c, "alpha"));
        assert!(!is_general_route(&c, "mid"));
        assert!(!is_general_route(&c, "unknown-tool"));
    }

    #[test]
    fn general_routes_do_not_count_toward_rung_overflow() {
        // A catalogue of 5 non-general tools + 1 general route fits the rung
        // (candidates_per_rung = 5): general is excluded from both the
        // catalogue and the overflow count, so every reachable tool stays
        // grammar-rendered and the contrastive head never engages.
        let mut c = config();
        let mut r = HashMap::new();
        for i in 0..5 {
            r.insert(format!("tool{i}"), route_ref(&format!("tool {i}")));
        }
        r.insert("local".into(), route_ref("general q&a"));
        c.schema_overrides.insert(
            "local".into(),
            NeedleRouteSchema {
                name: "local".into(),
                description: "general q&a".into(),
                examples: vec![],
                parameters: json!({}),
                intents: vec!["general".into()],
                output_template: None,
                general: true,
            },
        );
        assert!(!overflows_rung(&c, &r), "6 routes, 5 non-general -> fits the rung");
        let candidates = build_candidate_schemas(&c, &r, None, "query");
        assert_eq!(candidates.len(), 5, "exactly the 5 non-general tools");
        let names: Vec<&str> = candidates.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"local"), "general route never in the catalogue");

        // Adding a sixth non-general tool overflows the rung again.
        r.insert("tool5".into(), route_ref("tool 5"));
        assert!(overflows_rung(&c, &r));
    }

    #[test]
    fn render_tool_schema_shape() {
        let schema = derive_route_schema(
            "alpha",
            &route_ref("a route"),
            Some(&NeedleRouteSchema {
                name: "alpha".into(),
                description: "overridden".into(),
                examples: vec!["do alpha".into()],
                parameters: json!({"type": "object", "properties": {}}),
                intents: vec!["alpha".into()],
                output_template: None,
                general: false,
            }),
        );
        let tool = render_tool_schema(&schema);
        assert_eq!(tool["name"], "alpha");
        assert_eq!(tool["description"], "overridden");
        assert_eq!(tool["parameters"]["type"], "object");
        // The engine schema is the plain OpenAI tool format: examples and
        // intents stay retrieval-side (HNSW schema_doc_text), never in the
        // grammar the engine sees (measured routing regression otherwise).
        assert!(tool.get("examples").is_none(), "examples are not engine context");
        assert!(tool.get("intents").is_none(), "intents are not engine context");
    }

    #[test]
    fn render_tool_schema_emits_plain_engine_format() {
        // Even a schema with rich examples/intents renders the plain format:
        // name + description + parameters only (the base model's training
        // distribution). Retrieval context stays on the schema itself.
        let schema = derive_route_schema(
            "alpha",
            &route_ref("a route"),
            Some(&NeedleRouteSchema {
                name: "alpha".into(),
                description: String::new(),
                examples: vec!["do alpha now".into()],
                parameters: json!({"type": "object", "properties": {}}),
                intents: vec!["alpha".into(), "beta".into()],
                output_template: None,
                general: false,
            }),
        );
        let tool = render_tool_schema(&schema);
        let keys: Vec<&str> = tool.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 3);
        for k in ["name", "description", "parameters"] {
            assert!(keys.contains(&k), "engine schema must carry {k}, got {keys:?}");
        }
    }

    #[test]
    fn render_tools_json_is_stable_array() {
        let c = config();
        let r = routes();
        let schemas = build_all_schemas(&c, &r);
        let json = render_tools_json(&schemas);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // Deterministic ordering: alpha, mid, zeta.
        assert_eq!(arr[0]["name"], "alpha");
        assert_eq!(arr[1]["name"], "mid");
        assert_eq!(arr[2]["name"], "zeta");
    }
}