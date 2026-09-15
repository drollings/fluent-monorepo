//! Classification-tree evaluation cost: full engine evaluation on a
//! representative tree, plus the filter matcher before/after precompilation.
//!
//! The `filter_baseline` bench replicates the removed per-request
//! `Regex::new` loop (same ordered-`any`, invalid-means-non-match semantics)
//! so the numbers show the M10/M11 compile-once win directly. Both filter
//! benches use a non-matching text (worst case: full scan, where the
//! compile cost dominates). Engine verdicts route to an unresolvable
//! terminal on purpose: the measured per-request work (filter scan, prompt
//! render, backend call) is identical on every outcome path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use fluent_llm::client::ChatBackend;
use fluent_llm::LlmError;
use fluent_llm::ChatMessage;

const FILTER_PATTERNS: &[&str] = &[
    "harmful pattern",
    "bad\\d+",
    "(?i)ignore (all|previous) instructions",
    "\\b(drop|delete) table\\b",
    "malware|ransomware|exploit",
    "[unclosed",
    "^[a-z0-9]{32}$",
    "credit.?card.?\\d{4}",
];

const NO_MATCH_TEXT: &str = "write a rust function that bakes bread on weekends";
const ENGINE_TEXT: &str = "write a rust function";

/// A canned classifier backend: always routes to `code` with high scores.
struct CannedBackend;

impl ChatBackend for CannedBackend {
    fn chat_complete(&self, _messages: &[ChatMessage]) -> Result<String, LlmError> {
        Ok(serde_json::json!({
            "route": "code",
            "coherence": 0.9,
            "safety": 0.9,
            "complexity": 2,
            "reason": "bench verdict",
        })
        .to_string())
    }
}

fn tree_json() -> &'static str {
    r#"{"root": {
        "type": "classifier",
        "description": "bench router",
        "model": "fast",
        "coherence_threshold": 0.6,
        "safety_threshold": 0.4,
        "children": [
            {"key": "blocked", "description": "known-bad content", "node": {
                "type": "filter",
                "description": "blocks banned topics",
                "patterns": ["harmful pattern", "bad\\d+", "[unclosed"],
                "outcome": "hard_reject"
            }},
            {"key": "code", "description": "programming and implementation",
             "node": {"type": "terminal", "route": "code", "group": "code"}},
            {"key": "general", "description": "everything else", "node": {
                "type": "fallback",
                "description": "default branch",
                "node": {"type": "terminal", "route": "local", "group": "question"}
            }}
        ]
    }}"#
}

fn engine() -> fluent_router::stages::tree::ClassificationEngine {
    use fluent_router::config::ClassificationTree;
    let tree: ClassificationTree = serde_json::from_str(tree_json()).expect("bench tree parses");
    let compiled = Arc::new(tree.compile().expect("bench tree compiles"));
    let routing = fluent_router::config::RoutingConfig {
        routes: HashMap::new(),
        models: HashMap::new(),
        model_groups: HashMap::new(),
        system_prompt: String::new(),
        safety_threshold: 0.3,
        default_route: "local".into(),
        score_matrix: None,
        onnx_keys: std::collections::BTreeSet::new(),
        roles: HashMap::new(),
    };
    fluent_router::stages::tree::ClassificationEngine::new(
        compiled,
        routing,
        Arc::new(CannedBackend),
        HashMap::new(),
        Arc::new(fluent_concurrency::pool::Limiter::new(4)),
        0.5,
        None,
        None,
    )
}

/// The removed per-request path: compile every pattern on every evaluation.
fn baseline_filter_match(patterns: &[String], text: &str) -> bool {
    patterns.iter().any(|pattern| {
        regex::Regex::new(pattern)
            .ok()
            .is_some_and(|re| re.is_match(text))
    })
}

fn compiled_filter_tree() -> fluent_router::config::CompiledTree {
    use fluent_router::config::ClassificationTree;
    let tree: ClassificationTree = serde_json::from_str(
        r#"{"root": {"type": "classifier", "description": "bench", "children": [
            {"key": "f", "description": "bench filter", "node": {
                "type": "filter", "description": "bench",
                "patterns": ["harmful pattern", "bad\\d+", "[unclosed"],
                "outcome": "hard_reject"
            }},
            {"key": "o", "description": "other",
             "node": {"type": "terminal", "route": "o", "group": "o"}}
        ]}}"#,
    )
    .expect("bench filter tree parses");
    tree.compile().expect("bench filter tree compiles")
}

fn bench_tree_evaluate(c: &mut Criterion) {
    let engine = engine();
    c.bench_function("tree_evaluate", |b| {
        b.iter(|| {
            let evaluation = engine
                .evaluate(ENGINE_TEXT, None, None)
                .expect("bench evaluation completes");
            std::hint::black_box(evaluation);
        });
    });
}

fn bench_filter_before_after(c: &mut Criterion) {
    let owned: Vec<String> = FILTER_PATTERNS.iter().map(ToString::to_string).collect();
    let compiled = compiled_filter_tree();
    c.bench_function("filter_baseline_regex_new_per_request", |b| {
        b.iter(|| {
            std::hint::black_box(baseline_filter_match(&owned, NO_MATCH_TEXT));
        });
    });
    c.bench_function("filter_compiled_regex_set", |b| {
        b.iter(|| {
            std::hint::black_box(compiled.filter_matches(&["f"], NO_MATCH_TEXT));
        });
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets = bench_tree_evaluate, bench_filter_before_after
);
criterion_main!(benches);
