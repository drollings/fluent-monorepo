//! Equivalence corpus for the precompiled classification tree: the compiled
//! filter matcher must agree with the old on-the-fly `Regex::new`-per-request
//! evaluation on every `(patterns, text)` pair, including deliberately
//! invalid patterns (warn-and-non-match, never an error).

use crate::config::classification::{ClassificationChild, ClassificationNode, ClassificationTree};
use crate::config::filters::FilterOutcome;

/// The pre-compilation evaluation semantics: first match wins, an invalid
/// pattern is a non-match.
fn old_on_the_fly(patterns: &[String], text: &str) -> bool {
    patterns.iter().any(|p| {
        regex::Regex::new(p)
            .ok()
            .is_some_and(|re| re.is_match(text))
    })
}

fn filter_tree(patterns: &[&str]) -> ClassificationTree {
    ClassificationTree {
        root: ClassificationNode::Classifier {
            description: "corpus router".into(),
            model: None,
            model_group: None,
            coherence_threshold: None,
            safety_threshold: None,
            children: vec![
                ClassificationChild {
                    key: "watched".into(),
                    description: "watched branch".into(),
                    node: ClassificationNode::Filter {
                        description: "corpus filter".into(),
                        patterns: patterns.iter().map(ToString::to_string).collect(),
                        outcome: FilterOutcome::HardReject,
                        redirect_to: None,
                        match_interlingua: None,
                    },
                },
                ClassificationChild {
                    key: "other".into(),
                    description: "other branch".into(),
                    node: ClassificationNode::Terminal {
                        route: "other".into(),
                        group: None,
                        role: None,
                        description: String::new(),
                        always_route: false,
                    },
                },
            ],
        },
    }
}

fn pattern_sets() -> Vec<Vec<&'static str>> {
    vec![
        vec![],
        vec!["hello"],
        vec!["hello", "world"],
        vec!["he", "hell", "hello"],
        vec!["^\\d+$", "[a-z]+@[a-z]+\\.com"],
        vec!["a.c", "a.*c", "(a|b)+c?"],
        // Deliberately invalid: warn-and-non-match, never an error.
        vec!["["],
        vec!["(unclosed"],
        vec!["a{2,1}"],
        vec!["valid.*pattern", "["],
        vec!["[", "also (broken", "fallback.*here"],
    ]
}

fn texts() -> Vec<&'static str> {
    vec![
        "",
        "hello",
        "say hello world",
        "HELLO",
        "12345",
        "user@example.com",
        "abc",
        "ac",
        "fallback test here",
        "nothing matches this at all",
    ]
}

#[test]
fn compiled_filter_matches_on_the_fly_on_corpus() {
    let _guard = crate::config::compiled_tree::COMPILE_TEST_LOCK
        .lock()
        .unwrap();
    for patterns in pattern_sets() {
        let tree = filter_tree(&patterns);
        let compiled = tree.compile().expect("corpus patterns must compile");
        for text in texts() {
            let owned: Vec<String> = patterns.iter().map(ToString::to_string).collect();
            assert_eq!(
                compiled.filter_matches(&["watched"], text),
                Some(old_on_the_fly(&owned, text)),
                "patterns={patterns:?} text={text:?}"
            );
        }
    }
}

#[test]
fn compiled_prompt_renders_identically() {
    let _guard = crate::config::compiled_tree::COMPILE_TEST_LOCK
        .lock()
        .unwrap();
    let tree: ClassificationTree = serde_json::from_str(
        r#"{"root": {"type": "classifier", "description": "corpus router", "model": "fast",
            "children": [
                {"key": "a", "description": "route a", "node": {"type": "terminal", "route": "a", "group": "a"}},
                {"key": "b", "description": "", "node": {"type": "terminal", "route": "b", "group": "b"}}
            ]}}"#,
    )
    .unwrap();
    let compiled = tree.compile().expect("compile");
    assert_eq!(
        compiled.render_prompt(&[], 0.6, 0.4),
        tree.root.build_prompt(0.6, 0.4),
        "compiled prompt template must render like the live builder"
    );
    // Per-request thresholds only touch the guidance line.
    let low = compiled.render_prompt(&[], 0.1, 0.2).expect("prompt");
    assert!(low.contains("coherence < 0.10"));
    assert!(low.contains("safety < 0.20"));
}

#[test]
fn compile_happens_once_per_compile_never_per_match() {
    use crate::config::compiled_tree::compile_count;
    let _guard = crate::config::compiled_tree::COMPILE_TEST_LOCK
        .lock()
        .unwrap();
    let tree = filter_tree(&["hello", "world"]);
    let before = compile_count();
    let compiled = tree.compile().expect("compile");
    assert_eq!(compile_count(), before + 1);
    for text in texts() {
        let _ = compiled.filter_matches(&["watched"], text);
        let _ = compiled.render_prompt(&[], 0.6, 0.4);
    }
    assert_eq!(
        compile_count(),
        before + 1,
        "matching/rendering must not recompile"
    );
    let _ = tree.compile().expect("compile");
    assert_eq!(compile_count(), before + 2);
}
