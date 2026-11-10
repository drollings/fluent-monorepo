//! Envelope fields on exact, homonym, and missing-symbol fixtures.

use super::{citations_of, fit_stages_to_budget, gaps_of, AnswerEnvelope, ConfidenceAxis};
use crate::query::strategy::{FsmEngine, QueryIntent};
use crate::query::synthesize::Stage;
use fluent_types::StageKind;

fn code_stage(source: &str, line: u32, name: &str) -> Stage {
    Stage {
        kind: StageKind::Code,
        content: format!("fn {name}()"),
        source: source.to_string(),
        line: Some(line),
        end_line: None,
        member_name: Some(name.to_string()),
        member_type: None,
        trace: None,
    }
}

fn prose_stage(content: &str) -> Stage {
    Stage {
        kind: StageKind::Prose,
        content: content.to_string(),
        source: "doc.md".to_string(),
        line: None,
        end_line: None,
        member_name: None,
        member_type: None,
        trace: None,
    }
}

fn classify(query: &str) -> crate::query::strategy::QueryClass {
    FsmEngine::new().run(query)
}

#[test]
fn exact_query_carries_cited_task_certified_envelope() {
    let class = classify("helloWorld");
    let stages = vec![code_stage("src/test.zig", 1, "helloWorld")];
    let envelope = AnswerEnvelope::from_stages(&class, "word_index", stages);
    assert_eq!(envelope.intent, QueryIntent::SingleIdentifier);
    assert_eq!(envelope.citations, vec!["src/test.zig:1".to_string()]);
    assert!(envelope.gaps.is_empty());
    assert!(envelope.is_answerable());
    assert_eq!(envelope.confidence.axis, ConfidenceAxis::TaskCertified);
    assert!(envelope.resolution.contains("word_index"));
    let rendered = envelope.render_markdown();
    assert!(rendered.contains("src/test.zig:1"));
}

#[test]
fn homonym_members_raise_an_ambiguity_gap() {
    let class = classify("render");
    let stages = vec![
        code_stage("src/a.rs", 3, "render"),
        code_stage("src/b.rs", 9, "render"),
    ];
    let envelope = AnswerEnvelope::from_stages(&class, "word_index", stages);
    assert!(envelope.gaps.iter().any(|gap| gap.contains("ambiguous: 2 candidates")));
    assert!(envelope.is_answerable());
    assert_eq!(envelope.citations.len(), 2);
}

#[test]
fn missing_symbol_reports_no_results_and_stays_producer_only() {
    let class = classify("noSuchThingAnywhere");
    let envelope = AnswerEnvelope::from_stages(&class, "word_index", Vec::new());
    assert!(envelope.gaps.iter().any(|gap| gap.starts_with("no results")));
    assert!(!envelope.is_answerable());
    assert_eq!(envelope.confidence.axis, ConfidenceAxis::ProducerOrderOnly);
    assert!(envelope.citations.is_empty());
}

#[test]
fn ungrounded_prose_withholds_synthesis() {
    let class = classify("how does caching work here");
    let envelope = AnswerEnvelope::from_stages(&class, "hybrid", vec![prose_stage("notes")]);
    assert!(envelope.gaps.iter().any(|gap| gap.starts_with("synthesis withheld")));
    assert!(!envelope.is_answerable());
    assert_eq!(envelope.citations, Vec::<String>::new());
    assert!(citations_of(&[]).is_empty());
    assert_eq!(gaps_of(&[], false), vec!["no results for query".to_string()]);
}

#[test]
fn budget_fit_preserves_rank_order_and_bounds_tokens() {
    let stages = vec![
        code_stage("src/a.rs", 1, "aaa"),
        code_stage("src/b.rs", 2, "bbb"),
        code_stage("src/c.rs", 3, "ccc"),
    ];
    let all = fit_stages_to_budget(&stages, 1_000_000);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].source, "src/a.rs");
    let none = fit_stages_to_budget(&stages, 0);
    assert!(none.is_empty());
    let one = fit_stages_to_budget(&stages, 3);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].source, "src/a.rs");
}
