//! Config-synced routing integration tests.
//!
//! These replace the former `bin/router-mock-tests.sh` shell smoke suite,
//! which drifted from `env/coral-router.json` (it hardcoded model names and
//! route expectations that no longer exist). The assertions here are *derived
//! from* the config at runtime — every route (intent) declared there is probed
//! and the expected outcome is read from the config's `routes` → `model_groups`
//! mapping — so the tests cannot fall out of sync with the config. The
//! protocol-level checks (health, stats, 404, streaming, malformed input,
//! commands, PII) reproduce the retired script's breadth.
//!
//! Coverage:
//! 1. **Config sanity** (`config_route_groups_resolve_to_models`): every
//!    route's `group` names a non-empty `model_groups` ladder of declared
//!    models; `default_route` is declared.
//! 2. **Intent → model_group** (`route_intents_dispatch_to_their_model_groups`):
//!    every route is probed (multiple phrasings); the router's own route +
//!    group validation records zero mismatches and each probe is answered.
//! 3. **Direct model dispatch** (`every_declared_model_answers_directly`):
//!    every declared model answers when requested by key.
//! 4. **`always_route` semantics** (`always_route_routes_force_dispatch_over_classifier_respond`):
//!    `always_route: true` routes dispatch even when the classifier wants to
//!    answer directly.
//! 5. **Deterministic pre-filter** (`deterministic_commands_dispatch`,
//!    `pii_requests_are_blocked`): commands and PII are intercepted.
//! 6. **Protocol** (`health_and_stats_endpoints_report_ok`,
//!    `unknown_path_returns_404`, `streaming_flag_returns_sse_chunks`,
//!    `malformed_requests_are_rejected`).
//! 7. **Fixture sync** (`mock_transcript_fixture_stays_synced_with_config`,
//!    `mock_transcript_entries_serve_their_expected_answers`): the `--mock`
//!    binary's fixture stays consistent with the config and serves its
//!    declared answers end-to-end.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::config::{ClassifierOutput, ModelGroup, RouteRef, RouterConfig};
use crate::needle::backend::NeedleBackend;
use crate::needle::template::{render_output_template, template_placeholders};
use crate::server::serve_http;
use crate::testing::mock::{
    load_transcript_file, needle_call_envelope, needle_call_envelope_with_args,
    needle_provider_from_entries, transcript_provider_from_entries, MockDispatchContext,
    MockTranscriptEntry,
};
use crate::tests::common::{get, post_chat, TestServer};
use fluent_llm::client::ChatBackend;

/// `env/coral-router.json` — the single source of truth. Resolved relative to
/// the crate manifest (cargo runs tests with the package dir as CWD, so a
/// plain relative path would be fragile across invocations).
fn config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../env/coral-router.json")
}

/// `env/mock-transcripts.json` — the `--mock` binary's fixture.
fn mock_transcripts_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../env/mock-transcripts.json")
}

/// Load the live config. A failure here IS a test failure: the router boots
/// this exact file, so it must deserialize with the typed schema. The relative
/// `blacklist` path is resolved to an absolute one so the deterministic
/// pre-filter loads the real PII patterns regardless of the test process CWD
/// (cargo runs tests from the package dir, not the repo root).
fn load_config() -> RouterConfig {
    let path = config_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut config: RouterConfig = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("cannot parse {} as RouterConfig: {e}", path.display()));
    let cfg_dir = config_path();
    let repo_root = cfg_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root above env dir");
    for params in config.pipelines.values_mut() {
        if let Some(rel) = params.blacklist.as_mut() {
            *rel = repo_root.join(rel.as_str()).display().to_string();
        }
    }
    config
}

/// Curated primary probe prompts, keyed by route name. `probe_for_route`
/// falls back to the route's `description` (or its name) so a route added to
/// the config is exercised without a code change;
/// `route_probe_seeds_stay_synced_with_config` fails the build if a seed
/// names a route the config no longer declares, or a declared route is
/// neither seeded nor described (the two drift directions that would
/// otherwise degrade a probe silently).
static ROUTE_PROBE_SEEDS: &[(&str, &str)] = &[
    (
        "local",
        "What is the capital of France? Answer in one short sentence.",
    ),
    ("prose", "Write a short gothic story about a lighthouse keeper."),
    ("code", "Write a Rust function to compute Fibonacci numbers."),
    (
        "summarize",
        "Summarize this in one sentence: 'Q3 revenue reached $4.2M, up 12% YoY.'",
    ),
    // explore-absorbs-extract regression probe: a value pull (the former
    // `extract` surface) routes to `explore`, which carries the direct-answer
    // output_template.
    (
        "explore",
        "Extract the dates and amounts from this email as JSON: 'Q3 invoice for $12,400 due October 15.'",
    ),
    // explain-absorbs-translation regression probe: a translation request (the
    // former `translation` surface) routes to `explain`, the deep-answer tool.
    (
        "explain",
        "Translate this into Japanese: 'The party shall be liable for gross negligence.'",
    ),
];

/// One demanding probe prompt per route, whose domain matches the route's
/// description so the intent is unambiguous. Falls back to the route's
/// `description` (or its name) so a route added to the config is exercised
/// without a code change.
fn probe_for_route(route: &str, rref: &RouteRef) -> String {
    let seeds = ROUTE_PROBE_SEEDS;
    seeds
        .iter()
        .find(|(name, _)| *name == route)
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| {
            if rref.description.is_empty() {
                format!("Please help with: {route}")
            } else {
                rref.description.clone()
            }
        })
}

/// Additional phrasings per route so the dispatch path is exercised across
/// more than one surface per intent. A route without variations is still
/// covered by its primary probe.
fn varied_probes_for_route(route: &str) -> Vec<String> {
    let variations: &[(&str, &[&str])] = &[
        (
            "local",
            &[
                "What is 2+2?",
                "hi",
                "What color is the sky?",
                "Who wrote the Iliad?",
            ],
        ),
        (
            "code",
            &[
                "Write a Rust program that prints the first ten primes.",
                "Fix a deadlock in this Go program.",
                "Explain what a monad is in Haskell.",
            ],
        ),
        (
            "prose",
            &[
                "Write a haiku about autumn leaves.",
                "Draft a letter of complaint to a landlord.",
            ],
        ),
        (
            "summarize",
            &["Condense this paragraph into a single sentence: 'The company reported strong Q3 results driven by European expansion.'"],
        ),
        (
            "explore",
            &[
                // Value pulls (the former `extract` surface) plus the other
                // lookup paths `explore` absorbs: search, navigation, API
                // lookup, data-store lookup, knowledge-graph lookup.
                "List every city mentioned in: 'We flew to Berlin, then London, then Tokyo.'",
                "Search the web for the latest Rust release notes.",
                "Go to the project documentation page.",
                "What is the status of the billing API right now?",
                "What does the ledger say about this service?",
                "Find related nodes for this concept.",
            ],
        ),
        (
            "explain",
            &[
                // Translation, analysis/reasoning, and named-entity probes —
                // the former translation/entities/science/medical/legal
                // surfaces are all absorbed by the deep-answer tool.
                "Translate 'Good morning' into French.",
                "Turn this English paragraph into German.",
                "Explain the EPR paradox and Bell's theorem.",
                "Describe the mechanism of action and indications of metformin.",
                "Who are the companies mentioned in this article?",
                "Draft a confidentiality clause for a software licensing agreement.",
            ],
        ),
    ];
    variations
        .iter()
        .find(|(name, _)| *name == route)
        .map(|(_, ps)| ps.iter().map(|p| (*p).to_string()).collect())
        .unwrap_or_default()
}

/// A classifier response that routes the probe to the given route.
///
/// Two forms under the unified confident-offload schema:
/// - `respond: true` — a confident decision on a non-dispatch-only route: the
///   classifier answers directly with the canned answer text.
/// - `respond: false` — a low-confidence decision (routes to the domain's
///   group even on a non-always_route route).
fn route_classifier_response(route: &str, respond: bool) -> String {
    if respond {
        json!({
            "domain": route,
            "response": format!("mock {route} answer"),
            "coherence_score": 0.95,
            "safety_score": 0.9,
            "confidence": 0.99,
            "reason": "config-synced mock probe",
        })
        .to_string()
    } else {
        json!({
            "domain": route,
            "coherence_score": 0.95,
            "safety_score": 0.9,
            "confidence": 0.0,
            "reason": "config-synced mock probe",
        })
        .to_string()
    }
}

/// A mock transcript entry for a probe that must be routed to `route` and
/// dispatched through `expect_model_group`.
fn route_entry(route: &str, expect_model_group: &str, user_message: &str, respond: bool) -> MockTranscriptEntry {
    MockTranscriptEntry {
        user_message: user_message.to_string(),
        classifier_response: route_classifier_response(route, respond),
        expected_route: Some(route.to_string()),
        expect_model_group: if respond { None } else { Some(expect_model_group.to_string()) },
        dispatch_response: Some(format!("mock {route} answer")),
        rejected: false,
        reject_reason_contains: None,
        ..Default::default()
    }
}

/// Derive mock transcript entries per route from the config: the primary probe
/// plus every variation. Each entry records the expected *route* and, for a
/// dispatch (an `always_route` route), the expected *model_group* (from
/// `routes[route].group`) for the router's own validation. A respond-eligible
/// route (not `always_route`) is probed with a confident direct answer — the
/// config-synced gate exercises the confident-offload respond path.
fn transcripts_from_config(config: &RouterConfig) -> Vec<MockTranscriptEntry> {
    config
        .routes
        .iter()
        .flat_map(|(route, rref)| {
            let respond = !rref.always_route;
            let mut probes = vec![probe_for_route(route, rref)];
            probes.extend(varied_probes_for_route(route));
            probes
                .into_iter()
                .map(|probe| route_entry(route, &rref.group, &probe, respond))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Boot the real `serve_http` accept loop with the given config, a transcript
/// classifier that returns each probe's canned `target`, a needle transcript
/// provider derived from each probe's `needle_response` (declining by default
/// so the classifier decides probes that don't exercise Needle), and a dispatch
/// mock that validates route + model_group resolution. Returns the server, the
/// shared mock context (whose `take_failures()` is the routing verdict), and
/// the needle provider (whose `calls()` proves the rung was consulted).
async fn spawn_config_mock_server(
    config: RouterConfig,
    entries: Vec<MockTranscriptEntry>,
) -> (TestServer, Arc<MockDispatchContext>, Arc<crate::testing::mock::NeedleTranscriptProvider>) {
    let backend: Arc<dyn ChatBackend> = Arc::new(transcript_provider_from_entries(&entries));
    let concrete: Arc<crate::testing::mock::NeedleTranscriptProvider> =
        Arc::new(needle_provider_from_entries(&entries));
    let needle = Arc::clone(&concrete);
    let needle_backend: Arc<dyn NeedleBackend> = concrete;
    let pipelines = Arc::new(
        config.build_all_pipelines_with_backends(Some(&backend), Some(&needle_backend)),
    );
    let mock = Arc::new(MockDispatchContext::new(entries, vec![]));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    let deps = crate::tests::common::test_deps(
        pipelines,
        &config,
        Some(Arc::clone(&mock)),
        None,
        None,
        HashMap::new(),
        None,
    );
    let handle = tokio::spawn(async move {
        if let Err(e) = serve_http(listener, deps, None).await {
            tracing::error!(target: "router.test", error = %e, "config-sync test server failed");
        }
    });

    (TestServer { addr, handle }, mock, needle)
}

/// Every route's `group` maps to a non-empty `model_groups` ladder whose
/// members are declared models; `default_route` is a declared route.
#[test]
fn config_route_groups_resolve_to_models() {
    let config = load_config();
    assert!(
        !config.routes.is_empty(),
        "coral-router.json declares no routes — nothing to test"
    );
    for (route, rref) in &config.routes {
        let ladder = config
            .model_groups
            .get(&rref.group)
            .map(ModelGroup::models)
            .unwrap_or(&[]);
        assert!(
            !ladder.is_empty(),
            "route '{route}' -> group '{}' has an empty model ladder",
            rref.group
        );
        for key in ladder {
            assert!(
                config.models.contains_key(key),
                "group '{}' (route '{route}') references unknown model '{key}'",
                rref.group
            );
        }
    }
    assert!(
        config.routes.contains_key(&config.default_route),
        "default_route '{}' must be a declared route",
        config.default_route
    );
}

/// The probe tables stay synced with the config in both drift directions:
/// every seed names a still-declared route, every route has a meaningful
/// probe source (a seed or a description), and the full probe set (primary +
/// variations) is unique and free of deterministic-command prefixes so the
/// transcript classifier can always distinguish a probe and no probe is
/// intercepted by the pre-filter instead of the intent being tested.
#[test]
fn route_probe_seeds_stay_synced_with_config() {
    let config = load_config();
    let seeded: Vec<&str> = ROUTE_PROBE_SEEDS.iter().map(|(n, _)| *n).collect();

    for (name, _) in ROUTE_PROBE_SEEDS {
        assert!(
            config.routes.contains_key(*name),
            "probe seed '{name}' is not a declared route in coral-router.json (rename or drop the seed)"
        );
    }

    for (route, rref) in &config.routes {
        assert!(
            seeded.contains(&route.as_str()) || !rref.description.is_empty(),
            "route '{route}' has neither a probe seed nor a description — its probe would be a weak name fallback"
        );
    }

    let mut seen: HashMap<String, String> = HashMap::new();
    for (route, rref) in &config.routes {
        let mut probes = vec![probe_for_route(route, rref)];
        probes.extend(varied_probes_for_route(route));
        for probe in probes {
            assert!(!probe.is_empty(), "route '{route}' produced an empty probe");
            assert!(
                !probe.starts_with('/') && !probe.starts_with('.') && !probe.starts_with(','),
                "route '{route}' probe '{probe}' starts like a deterministic command"
            );
            let previous = seen.insert(probe.clone(), route.clone());
            assert!(
                previous.is_none(),
                "routes '{previous:?}' and '{route}' would collide on probe '{probe}' (the transcript classifier could not tell them apart)"
            );
        }
    }
}

/// The shipped config's `needle.tool_plans` are executable end-to-end: a
/// `Rerouted` probe on a plan-bearing route runs the bounded plan (dispatch →
/// lookup → compose) and returns a coherent answer with no `[lookup:`
/// placeholder. Hermetic: the registry is built with an in-memory ledger and
/// a chart store (the shipped kinds — `knowledge_graph`/`chart`), so an
/// absent lookup is omitted and the composed answer carries the real dispatch
/// text, never a synthesized lookup. Config-synced: the probed routes and
/// plans are derived from `env/coral-router.json` at runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shipped_tool_plans_execute_placeholder_free() {
    use crate::charts::store::ChartStore;
    use crate::ledger::ContentNodeLedger;
    use crate::server::tool_lookup::build_registry;
    use crate::test_stubs::HashEmbedder;

    let config = load_config();
    let needle = config.needle.as_ref().expect("needle config present");
    let plans = &needle.tool_plans;
    assert!(
        !plans.is_empty(),
        "shipped coral-router.json must declare needle.tool_plans"
    );

    let ledger = Arc::new(ContentNodeLedger::open_in_memory().unwrap());
    let chart_store = Arc::new(ChartStore::new(None));
    let embedder: Arc<dyn fluent_llm::EmbeddingProvider> = Arc::new(HashEmbedder::new(256));
    let registry = build_registry(&config, Some(&ledger), Some(&chart_store), Some(embedder));
    assert!(registry.supports("knowledge_graph"), "explore plan kind installed");
    assert!(registry.supports("chart"), "explain plan kind installed");

    let mut entries: Vec<MockTranscriptEntry> = Vec::new();
    for (route, plan) in plans {
        assert!(
            !plan.steps.is_empty(),
            "shipped plan for '{route}' must declare steps"
        );
        let rref = &config.routes[route];
        entries.push(MockTranscriptEntry {
            user_message: format!("tool plan probe: {route}"),
            classifier_response: route_classifier_response(route, false),
            expected_route: Some(route.clone()),
            expect_model_group: Some(rref.group.clone()),
            dispatch_response: Some(format!("mock {route} answer")),
            rejected: false,
            reject_reason_contains: None,
            ..Default::default()
        });
    }

    let backend: Arc<dyn ChatBackend> = Arc::new(transcript_provider_from_entries(&entries));
    let needle_backend: Arc<dyn NeedleBackend> =
        Arc::new(needle_provider_from_entries(&entries));
    let pipelines = Arc::new(
        config.build_all_pipelines_with_backends(Some(&backend), Some(&needle_backend)),
    );
    let mock = Arc::new(MockDispatchContext::new(entries, vec![]));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let mut deps = crate::tests::common::test_deps(
        pipelines,
        &config,
        Some(Arc::clone(&mock)),
        None,
        None,
        HashMap::new(),
        None,
    );
    deps.tool_plans = plans.clone();
    deps.needle_max_rounds = needle.max_rounds;
    deps.tool_lookup = registry;
    let handle = tokio::spawn(async move {
        if let Err(e) = serve_http(listener, deps, None).await {
            tracing::error!(target: "router.test", error = %e, "tool-plan test server failed");
        }
    });
    let server = TestServer { addr, handle };

    for route in plans.keys() {
        let body = json!({
            "model": route,
            "messages": [{"role": "user", "content": format!("tool plan probe: {route}")}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("plan probe for route '{route}' failed: {e}"));
        assert_eq!(response.status(), 200, "plan route '{route}' must answer");
        let value: Value = response
            .json()
            .await
            .unwrap_or_else(|e| panic!("plan route '{route}' response must be valid JSON: {e}"));
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        assert!(
            !content.contains("[lookup:"),
            "the shipped '{route}' plan must never produce a [lookup: placeholder, got: {content}"
        );
        assert!(
            content.contains(&format!("mock {route} answer")),
            "the '{route}' plan must compose the real dispatch answer, got: {content}"
        );
    }

    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "tool-plan dispatch mismatches:\n  {}",
        failures.join("\n  ")
    );
}

/// Every route (intent) in the config dispatches through its configured
/// `model_group`, across every probe (primary + variations): the router's own
/// route + group validation records zero mismatches, and each probe is
/// answered with its canned dispatch response (HTTP 200).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn route_intents_dispatch_to_their_model_groups() {
    let config = load_config();
    let route_count = config.routes.len();
    let entries = transcripts_from_config(&config);
    let (server, mock, _needle) = spawn_config_mock_server(config, entries.clone()).await;

    let mut probed = 0;
    for entry in &entries {
        let route = entry.expected_route.as_deref().expect("derived route");
        let body = json!({
            "model": route,
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("request for route '{route}' failed: {e}"));
        assert_eq!(
            response.status(),
            200,
            "route '{route}' (probe '{:?}') must answer 200",
            entry.user_message
        );
        let value: Value = response
            .json()
            .await
            .unwrap_or_else(|e| panic!("route '{route}' response must be valid JSON: {e}"));
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let expected = format!("mock {route} answer");
        assert!(
            content.contains(&expected),
            "route '{route}' must return its dispatched answer, got: {content:?}"
        );
        probed += 1;
    }
    assert!(
        probed >= route_count,
        "expected at least one probe per declared route, probed {probed}"
    );

    // The router's own route/group validation (recorded on every mock
    // dispatch) must be clean: any mismatch means an intent did not reach the
    // model_group its route maps to in the config.
    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "intent -> model_group mismatches:\n  {}",
        failures.join("\n  ")
    );
}

/// Needle — the cheapest structured rung — is the primary router for **non-
/// general** routes that carry a `schema_overrides` tool description: a
/// grammar-constrained Needle call that names the route short-circuits the
/// pipeline and dispatches through that route's `model_group`, exactly as the
/// classifier would. This test drives each such route's probe with a canned
/// Needle `call` envelope and a *decoy* classifier response (routed to a
/// different route), so a probe can only answer through the correct group if
/// the Needle rung actually fired and won. Config-synced: the probed routes
/// and expected groups are derived from `env/coral-router.json` at runtime, so
/// the suite cannot drift from it. (General routes — `schema_overrides`
/// entries marked `general` — are excluded: they fall through to the
/// classifier and are covered by
/// `needle_general_category_falls_through_to_classifier`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn needle_routes_dispatch_via_needle_to_their_model_groups() {
    let config = load_config();
    let route_keys: Vec<String> = config.routes.keys().cloned().collect();
    let needle_routes: Vec<String> = config
        .needle
        .as_ref()
        .map(|n| {
            n.schema_overrides
                .iter()
                .filter(|(_, s)| !s.general)
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !needle_routes.is_empty(),
        "needle config declares no non-general schema_overrides — nothing to test"
    );

    let entries: Vec<MockTranscriptEntry> = needle_routes
        .iter()
        .map(|route| {
            let rref = &config.routes[route];
            let probe = probe_for_route(route, rref);
            // A decoy route that differs from the target, so a probe can only
            // reach the expected group if Needle fired (the classifier decoy
            // would send it elsewhere).
            let decoy = route_keys
                .iter()
                .find(|k| *k != route)
                .cloned()
                .unwrap_or_else(|| config.default_route.clone());
            MockTranscriptEntry {
                user_message: probe.clone(),
                classifier_response: route_classifier_response(&decoy, false),
                needle_response: Some(needle_call_envelope(route, 0.95)),
                expected_route: Some(route.clone()),
                expect_model_group: Some(rref.group.clone()),
                dispatch_response: Some(format!("mock {route} answer")),
                rejected: false,
                reject_reason_contains: None,
                ..Default::default()
            }
        })
        .collect();
    let (server, mock, needle) = spawn_config_mock_server(config, entries.clone()).await;

    for entry in &entries {
        let route = entry.expected_route.as_deref().expect("derived route");
        let body = json!({
            "model": route,
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("needle probe for route '{route}' failed: {e}"));
        assert_eq!(response.status(), 200, "needle route '{route}' must answer 200");
        let value: Value = response
            .json()
            .await
            .unwrap_or_else(|e| panic!("needle route '{route}' response must be valid JSON: {e}"));
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let expected = format!("mock {route} answer");
        assert!(
            content.contains(&expected),
            "needle route '{route}' must dispatch its answer (not the classifier's decoy), got: {content:?}"
        );
    }

    assert!(
        needle.calls() > 0,
        "the Needle rung must have been consulted for these probes"
    );
    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "needle -> model_group mismatches (a route did not dispatch through its group):\n  {}",
        failures.join("\n  ")
    );
}

/// The `general` category is **not** a Needle decision: a Needle `call` to a
/// route whose `schema_overrides` entry is marked `general` (e.g. the `local`
/// general Q&A route) falls through to the classifier LLM, which classifies the
/// whole prompt as-is — never a Needle short-circuit. Non-general route tools
/// keep dispatching through their `model_group`. Config-synced: the general and
/// non-general routes, and the fallback/keep-dispatch expectations, are all
/// derived from `env/coral-router.json` at runtime, so the suite cannot drift.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn needle_general_category_falls_through_to_classifier() {
    let config = load_config();
    let needle = config
        .needle
        .as_ref()
        .expect("needle config present in coral-router.json");
    let general: Vec<String> = needle
        .schema_overrides
        .iter()
        .filter(|(_, s)| s.general)
        .map(|(k, _)| k.clone())
        .collect();
    let non_general: Vec<String> = needle
        .schema_overrides
        .iter()
        .filter(|(_, s)| !s.general)
        .map(|(k, _)| k.clone())
        .collect();
    assert!(
        !general.is_empty(),
        "config must mark at least one schema_override `general` — nothing to test"
    );
    assert!(
        !non_general.is_empty(),
        "config must declare at least one non-general schema_override — nothing to test"
    );

    let mut entries: Vec<MockTranscriptEntry> = Vec::new();
    // General routes: a Needle `call` falls through and the classifier answers
    // directly (proving the request reached the classifier, not a Needle
    // dispatch).
    for route in &general {
        entries.push(MockTranscriptEntry {
            user_message: format!("general fallback probe: {route}"),
            classifier_response: json!({
                "domain": route,
                "response": "GENERAL-CLASSIFIER-ANSWER",
                "coherence_score": 0.95,
                "safety_score": 0.9,
                "confidence": 0.99,
                "reason": "general category decided by classifier",
            })
            .to_string(),
            needle_response: Some(needle_call_envelope(route, 0.95)),
            expected_route: None,
            expect_model_group: None,
            dispatch_response: None,
            rejected: false,
            reject_reason_contains: None,
            ..Default::default()
        });
    }
    // Non-general control: still dispatches through its group (Needle decides).
    let control_route = non_general[0].clone();
    let control_rref = &config.routes[&control_route];
    let control_probe = format!("non-general control probe: {control_route}");
    entries.push(MockTranscriptEntry {
        user_message: control_probe.clone(),
        classifier_response: route_classifier_response(&control_route, false),
        needle_response: Some(needle_call_envelope(&control_route, 0.95)),
        expected_route: Some(control_route.clone()),
        expect_model_group: Some(control_rref.group.clone()),
        dispatch_response: Some(format!("mock {control_route} answer")),
        rejected: false,
        reject_reason_contains: None,
        ..Default::default()
    });

    let (server, mock, needle_provider) = spawn_config_mock_server(config, entries.clone()).await;

    for entry in &entries {
        let model = entry
            .expected_route
            .clone()
            .unwrap_or_else(|| "local".to_string());
        let body = json!({
            "model": model,
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("probe '{:?}' failed: {e}", entry.user_message));
        assert_eq!(
            response.status(),
            200,
            "probe '{:?}' must answer",
            entry.user_message
        );
        let text = response
            .text()
            .await
            .unwrap_or_else(|e| panic!("probe '{:?}' body: {e}", entry.user_message));

        if entry.user_message == control_probe {
            let expected = format!("mock {control_route} answer");
            assert!(
                text.contains(&expected),
                "non-general route '{control_route}' must still dispatch via needle, got: {text}"
            );
        } else {
            assert!(
                text.contains("GENERAL-CLASSIFIER-ANSWER"),
                "general route '{:?}' must fall through to the classifier (direct answer), got: {text}",
                entry.user_message
            );
            assert!(
                !text.contains("mock "),
                "general route '{:?}' must NOT dispatch via needle, got: {text}",
                entry.user_message
            );
        }
    }

    assert!(
        needle_provider.calls() > 0,
        "the Needle rung must have been consulted for these probes"
    );
    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "unexpected dispatch/validation for general-fallback probes:\n  {}",
        failures.join("\n  ")
    );
}

/// A tool that declares an `output_template` in `schema_overrides` is answered
/// **directly** by rendering that template with the bound arguments — no
/// dispatch, no group validation, no extra inference. Non-template routes keep
/// dispatching to their model_group. Config-synced: the template-bearing tools,
/// the argument set, and the expected rendered output are all derived from
/// `env/coral-router.json` at runtime, so the test cannot drift from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn needle_direct_tool_response_answers_from_template() {
    let config = load_config();
    let needle = config
        .needle
        .as_ref()
        .expect("needle config present in coral-router.json");
    let template_routes: Vec<(String, String)> = needle
        .schema_overrides
        .iter()
        .filter_map(|(route, schema)| {
            schema
                .output_template
                .as_ref()
                .map(|t| (route.clone(), t.clone()))
        })
        .collect();
    assert!(
        !template_routes.is_empty(),
        "config declares no output_template schema_overrides — nothing to test"
    );

    let mut entries = Vec::new();
    let mut expected_outputs: Vec<String> = Vec::new();
    for (route, template) in &template_routes {
        // Build a complete argument set from the template's placeholders and
        // render the expected output with the same pure function the router
        // uses — the two agree by construction.
        let mut args = serde_json::Map::new();
        for key in template_placeholders(template) {
            args.insert(key.clone(), serde_json::json!(format!("v-{key}")));
        }
        let arguments = serde_json::Value::Object(args);
        let expected = render_output_template(template, arguments.as_object().expect("object"))
            .expect("a complete argument set must render the template");
        assert!(
            !expected.is_empty(),
            "template for '{route}' rendered empty — a direct answer must be non-empty"
        );
        expected_outputs.push(expected.clone());

        // A decoy classifier response: if the probe dispatches (instead of
        // answering directly) it will surface the decoy, failing the assertion.
        let decoy = config.routes.keys().next().cloned().unwrap_or_default();
        entries.push(MockTranscriptEntry {
            user_message: format!("needle direct probe: {route}"),
            classifier_response: route_classifier_response(&decoy, false),
            needle_response: Some(needle_call_envelope_with_args(route, 0.95, &arguments)),
            expected_route: None,
            expect_model_group: None,
            dispatch_response: Some(format!("mock {route} answer")),
            rejected: false,
            reject_reason_contains: None,
            ..Default::default()
        });
    }

    // Control: a non-template schema_override must still dispatch (prove the
    // direct path is opt-in, not the default for every route).
    let non_template = needle
        .schema_overrides
        .iter()
        .find(|(_, s)| s.output_template.is_none())
        .map(|(route, _)| route.clone());
    let control_route = non_template.expect("config must declare a non-template schema_override");
    let control_rref = &config.routes[&control_route];
    entries.push(MockTranscriptEntry {
        user_message: format!("needle control probe: {control_route}"),
        classifier_response: route_classifier_response(&control_route, false),
        needle_response: Some(needle_call_envelope(&control_route, 0.95)),
        expected_route: Some(control_route.clone()),
        expect_model_group: Some(control_rref.group.clone()),
        dispatch_response: Some(format!("mock {control_route} answer")),
        rejected: false,
        reject_reason_contains: None,
        ..Default::default()
    });

    let (server, mock, needle_provider) = spawn_config_mock_server(config, entries.clone()).await;

    // Template-bearing probes are answered directly — the rendered template is
    // the response content, with no dispatch (no route/group validation).
    for (i, entry) in entries.iter().enumerate() {
        let body = json!({
            "model": "local",
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("probe '{:?}' failed: {e}", entry.user_message));
        assert_eq!(response.status(), 200, "probe '{:?}' must answer", entry.user_message);
        let text = response
            .text()
            .await
            .unwrap_or_else(|e| panic!("probe '{:?}' body: {e}", entry.user_message));
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");

        if i < expected_outputs.len() {
            let expected = &expected_outputs[i];
            assert!(
                content.contains(expected),
                "template route '{:?}' must answer directly with the rendered template '{expected}', got: {text}",
                entry.user_message
            );
            assert!(
                !content.contains("mock "),
                "template route '{:?}' must not dispatch, got: {text}",
                entry.user_message
            );
        } else {
            let expected = format!("mock {control_route} answer");
            assert!(
                content.contains(&expected),
                "non-template route '{}' must still dispatch, got: {text}",
                control_route
            );
        }
    }

    assert!(
        needle_provider.calls() > 0,
        "the Needle rung must have been consulted"
    );
    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "unexpected dispatch/validation for direct-template probes:\n  {}",
        failures.join("\n  ")
    );
}

/// Every declared model answers when requested directly by its config key
/// (bypassing the route table): `target_name` resolves to the model key and
/// each request returns its canned answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_declared_model_answers_directly() {
    let config = load_config();
    assert!(
        !config.models.is_empty(),
        "coral-router.json declares no models — nothing to test"
    );
    let entries: Vec<MockTranscriptEntry> = config
        .models
        .keys()
        .map(|key| {
            let user_message = format!("direct model probe: {key}");
            MockTranscriptEntry {
                user_message: user_message.clone(),
                classifier_response: route_classifier_response(&config.default_route, false),
                expected_route: Some(key.clone()),
                expect_model_group: None,
                dispatch_response: Some(format!("mock answer from {key}")),
                rejected: false,
                reject_reason_contains: None,
                ..Default::default()
            }
        })
        .collect();
    let (server, mock, _needle) = spawn_config_mock_server(config, entries.clone()).await;

    for entry in &entries {
        let key = entry.expected_route.as_deref().expect("model key");
        let body = json!({
            "model": key,
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("request for model '{key}' failed: {e}"));
        assert_eq!(
            response.status(),
            200,
            "direct model '{key}' must answer 200"
        );
        let value: Value = response
            .json()
            .await
            .unwrap_or_else(|e| panic!("model '{key}' response must be valid JSON: {e}"));
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let expected = format!("mock answer from {key}");
        assert!(
            content.contains(&expected),
            "model '{key}' must return its direct answer, got: {content:?}"
        );
    }

    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "direct model mismatches:\n  {}",
        failures.join("\n  ")
    );
}

/// `always_route: true` routes dispatch even when the classifier answers
/// directly (the override forces `respond` → `route`), while a non-always_route
/// route honours the classifier's direct answer.
///
/// Config-synced tier — this is the load-bearing probe that every
/// `always_route` route declared in `env/coral-router.json` dispatches
/// end-to-end (it reads the live config at runtime, so it cannot drift and
/// protects the config's flags from silent removal). Under the unified
/// confident-offload schema the *mechanism* is `routing_policy::derive_action`
/// (dispatch-only → always Route, even at maximum confidence) — see
/// `routing_policy`'s unit tests and the domain-resolution tests in
/// `classifier.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_route_routes_force_dispatch_over_classifier_respond() {
    let config = load_config();
    let always: Vec<(String, String, String)> = config
        .routes
        .iter()
        .filter(|(_, rref)| rref.always_route)
        .map(|(route, rref)| (route.clone(), rref.group.clone(), probe_for_route(route, rref)))
        .collect();
    assert!(
        !always.is_empty(),
        "config declares no always_route routes — nothing to test"
    );

    // A maximum-confidence classifier that wants to answer directly on each
    // dispatch-only domain. The `response` field is present (the classifier
    // "wants" to answer), but a dispatch-only domain must never answer directly.
    let mut entries: Vec<MockTranscriptEntry> = always
        .iter()
        .map(|(route, group, probe)| MockTranscriptEntry {
            user_message: probe.clone(),
            classifier_response: json!({
                "domain": route,
                "response": "DIRECT-ANSWER",
                "coherence_score": 0.95,
                "safety_score": 0.9,
                "confidence": 1.0,
                "reason": "mock probe wants a direct answer",
            })
            .to_string(),
            expected_route: Some(route.clone()),
            expect_model_group: Some(group.clone()),
            dispatch_response: Some("DISPATCHED-ANSWER".into()),
            rejected: false,
            reject_reason_contains: None,
            ..Default::default()
        })
        .collect();

    // Control: a confident non-always_route route honours the classifier's
    // direct answer (respond-eligible).
    let control = config
        .routes
        .iter()
        .find(|(_, r)| !r.always_route)
        .map(|(route, rref)| (route.clone(), rref.group.clone(), probe_for_route(route, rref)));
    let (control_probe, is_control) = match &control {
        Some((_, _, probe)) => (probe.clone(), true),
        None => (String::new(), false),
    };
    if let Some((route, _group, probe)) = &control {
        entries.push(MockTranscriptEntry {
            user_message: probe.clone(),
            classifier_response: json!({
                "domain": route,
                "response": "DIRECT-ANSWER",
                "coherence_score": 0.95,
                "safety_score": 0.9,
                "confidence": 1.0,
                "reason": "mock probe wants a direct answer",
            })
            .to_string(),
            expected_route: Some(route.clone()),
            expect_model_group: None,
            dispatch_response: None,
            rejected: false,
            reject_reason_contains: None,
            ..Default::default()
        });
    }

    let (server, mock, _needle) = spawn_config_mock_server(config, entries.clone()).await;

    for entry in &entries {
        let route = entry.expected_route.as_deref().expect("declared route");
        let body = json!({
            "model": route,
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("request for '{route}' failed: {e}"));
        assert_eq!(response.status(), 200, "route '{route}' must answer");
        let text = response
            .text()
            .await
            .unwrap_or_else(|e| panic!("route '{route}' body: {e}"));
        let is_control_probe = is_control && entry.user_message == control_probe;
        if is_control_probe {
            assert!(
                text.contains("DIRECT-ANSWER"),
                "non-always_route '{route}' may answer directly, got: {text}"
            );
        } else {
            assert!(
                text.contains("DISPATCHED-ANSWER"),
                "always_route '{route}' must dispatch instead of answering directly, got: {text}"
            );
        }
    }

    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "always_route dispatch mismatches:\n  {}",
        failures.join("\n  ")
    );
}

/// Deterministic pre-filter commands (`/help`, `/stats`, `/checkpoint <name>`)
/// and unknown commands are intercepted before the classifier.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deterministic_commands_dispatch() {
    let config = load_config();
    let (server, _mock, _needle) = spawn_config_mock_server(config.clone(), vec![]).await;

    let cases: &[(&str, &str)] = &[
        ("/help", "help"),
        ("/stats", "stats"),
        ("/checkpoint snap1", "checkpoint"),
        ("/nonexistent", "unknown"),
    ];
    for (command, fragment) in cases {
        let body = json!({
            "model": config.default_route,
            "messages": [{"role": "user", "content": command}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("command '{command}' request failed: {e}"));
        assert_eq!(response.status(), 200, "command '{command}' must answer 200");
        let text = response
            .text()
            .await
            .unwrap_or_else(|e| panic!("command '{command}' body: {e}"));
        assert!(
            text.contains(fragment),
            "command '{command}' must echo '{fragment}', got: {text}"
        );
    }
}

/// PII is intercepted: SSN/email via the classifier's safety gate (a low
/// `safety_score` — the unified schema's policy-violation signal — is rejected
/// by the coherence/safety threshold) and API keys via the deterministic
/// pre-filter's hard reject (scope `any`). Each body carries the rejecting
/// reason, and the mock's own rejection validation records zero mismatches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pii_requests_are_blocked() {
    let config = load_config();
    let entries = vec![
        MockTranscriptEntry {
            user_message: "My SSN is 123-45-6789".into(),
            classifier_response: json!({"domain":"local","coherence_score":1.0,"safety_score":0.1,"confidence":0.9,"reason":"PII detected"}).to_string(),
            expected_route: None,
            expect_model_group: None,
            dispatch_response: None,
            rejected: true,
            reject_reason_contains: Some("safety".into()),
            ..Default::default()
        },
        MockTranscriptEntry {
            user_message: "Email me@test.com please".into(),
            classifier_response: json!({"domain":"local","coherence_score":1.0,"safety_score":0.1,"confidence":0.9,"reason":"email address detected"}).to_string(),
            expected_route: None,
            expect_model_group: None,
            dispatch_response: None,
            rejected: true,
            reject_reason_contains: Some("safety".into()),
            ..Default::default()
        },
        MockTranscriptEntry {
            user_message: "api_key=sk-abcdefghijklmnop123456".into(),
            classifier_response: json!({"domain":"local","coherence_score":1.0,"safety_score":1.0,"confidence":0.9,"reason":"api key detected"}).to_string(),
            expected_route: None,
            expect_model_group: None,
            dispatch_response: None,
            rejected: true,
            reject_reason_contains: Some("api_key".into()),
            ..Default::default()
        },
    ];
    let (server, mock, _needle) = spawn_config_mock_server(config.clone(), entries.clone()).await;

    for entry in &entries {
        let body = json!({
            "model": config.default_route.clone(),
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("PII probe '{:?}' failed: {e}", entry.user_message));
        assert_eq!(response.status(), 200, "PII probe '{:?}' must answer", entry.user_message);
        let text = response
            .text()
            .await
            .unwrap_or_else(|e| panic!("PII probe '{:?}' body: {e}", entry.user_message));
        let fragment = entry.reject_reason_contains.as_deref().expect("reason fragment");
        assert!(
            text.contains(fragment),
            "PII probe '{:?}' must carry '{fragment}', got: {text}",
            entry.user_message
        );
    }

    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "PII rejection validation mismatches:\n  {}",
        failures.join("\n  ")
    );
}

/// `GET /health` reports `{"status":"ok"}` and `GET /stats` reports the request
/// counters.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_and_stats_endpoints_report_ok() {
    let config = load_config();
    let (server, _mock, _needle) = spawn_config_mock_server(config, vec![]).await;

    let health = get(&server.base_url(), "/health", 10_000)
        .await
        .expect("health request must complete");
    assert_eq!(health.status(), 200, "health must be 200");
    let health_json: Value = health.json().await.expect("health body must be JSON");
    assert_eq!(health_json["status"], "ok");

    let stats = get(&server.base_url(), "/stats", 10_000)
        .await
        .expect("stats request must complete");
    assert_eq!(stats.status(), 200, "stats must be 200");
    let stats_json: Value = stats.json().await.expect("stats body must be JSON");
    assert!(stats_json.get("requests").is_some(), "stats must report requests");
    assert!(stats_json.get("errors").is_some(), "stats must report errors");
}

/// Unknown paths return 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_path_returns_404() {
    let config = load_config();
    let (server, _mock, _needle) = spawn_config_mock_server(config, vec![]).await;

    let response = get(&server.base_url(), "/nonexistent", 10_000)
        .await
        .expect("404 probe must complete");
    assert_eq!(response.status(), 404, "unknown path must 404");
}

/// `env/mock-transcripts.json` (the `--mock` binary's fixture) stays synced
/// with the config: every `expected_route` and every classifier `domain` must
/// be a declared route or model, and where both are present they must agree.
#[test]
fn mock_transcript_fixture_stays_synced_with_config() {
    let config = load_config();
    let entries = load_transcript_file(mock_transcripts_path())
        .unwrap_or_else(|e| panic!("cannot load {}: {e}", mock_transcripts_path().display()));
    assert!(
        !entries.is_empty(),
        "mock-transcripts.json is empty — the --mock binary would have no canned answers"
    );
    for entry in &entries {
        let resolved = |name: &str| config.routes.contains_key(name) || config.models.contains_key(name);
        if let Some(expected_route) = &entry.expected_route {
            assert!(
                resolved(expected_route),
                "mock-transcripts.json: expected_route '{expected_route}' (for '{:?}') is neither a declared route nor a model in coral-router.json",
                entry.user_message
            );
        }
        let output: ClassifierOutput = serde_json::from_str(&entry.classifier_response).unwrap_or_else(
            |e| panic!("mock-transcripts.json: unparseable classifier_response for '{:?}': {e}", entry.user_message),
        );
        if !output.domain.is_empty() {
            assert!(
                resolved(&output.domain),
                "mock-transcripts.json: classifier domain '{}' (for '{:?}') is neither a declared route nor a model",
                output.domain,
                entry.user_message
            );
        }
        if let (Some(expected_route), domain) = (&entry.expected_route, output.domain.as_str()) {
            if !domain.is_empty() {
                assert_eq!(
                    expected_route.as_str(), domain,
                    "mock-transcripts.json: expected_route and classifier domain disagree for '{:?}'",
                    entry.user_message
                );
            }
        }
        if let Some(expected_group) = &entry.expect_model_group {
            let declared_group = entry
                .expected_route
                .as_ref()
                .and_then(|r| config.routes.get(r))
                .map(|r| r.group.as_str());
            assert_eq!(
                declared_group,
                Some(expected_group.as_str()),
                "mock-transcripts.json: expect_model_group '{expected_group}' for '{:?}' must name the model_groups the expected_route maps to",
                entry.user_message
            );
        }
    }
}

/// The `--mock` fixture serves its declared answers end-to-end: every
/// non-rejected entry returns its canned `dispatch_response` and every
/// rejected entry carries its `reject_reason_contains` fragment. The router's
/// own validation (route + rejection reasons) records zero mismatches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mock_transcript_entries_serve_their_expected_answers() {
    let config = load_config();
    let default_route = config.default_route.clone();
    let entries = load_transcript_file(mock_transcripts_path())
        .expect("load mock-transcripts.json (validated by mock_transcript_fixture_stays_synced_with_config)");
    let (server, mock, _needle) = spawn_config_mock_server(config, entries.clone()).await;

    for entry in &entries {
        let body = json!({
            "model": entry.expected_route.clone().unwrap_or_else(|| default_route.clone()),
            "messages": [{"role": "user", "content": entry.user_message}],
        });
        let response = post_chat(&server.base_url(), body, 15_000)
            .await
            .unwrap_or_else(|e| panic!("fixture probe '{:?}' failed: {e}", entry.user_message));
        assert_eq!(
            response.status(),
            200,
            "fixture probe '{:?}' must answer",
            entry.user_message
        );
        let text = response
            .text()
            .await
            .unwrap_or_else(|e| panic!("fixture probe '{:?}' body: {e}", entry.user_message));

        if entry.rejected {
            let fragment = entry.reject_reason_contains.as_deref().unwrap_or("ERROR:");
            assert!(
                text.contains(fragment),
                "rejected probe '{:?}' must carry '{fragment}', got: {text}",
                entry.user_message
            );
            assert!(
                text.contains("ERROR:"),
                "rejected probe '{:?}' must surface an ERROR body, got: {text}",
                entry.user_message
            );
        } else {
            let expected = entry.dispatch_response.as_deref().expect("dispatch response");
            assert!(
                text.contains(expected),
                "routed probe '{:?}' must return its dispatch response, got: {text}",
                entry.user_message
            );
        }
    }

    let failures = mock.take_failures();
    assert!(
        failures.is_empty(),
        "fixture validation mismatches (stale expected_route / reject reason):\n  {}",
        failures.join("\n  ")
    );
}
