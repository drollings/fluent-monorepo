//! Pack budget compliance, determinism, and drop accounting.

use super::{pack_context, ContextItem, DEFAULT_PACK_TOKENS};

fn item(source: &str, text: &str) -> ContextItem {
    ContextItem { text: text.to_string(), source: source.to_string() }
}

#[test]
fn small_packs_keep_everything_in_priority_order() {
    let items = vec![item("a", "alpha"), item("b", "beta"), item("c", "gamma")];
    let pack = pack_context(&items, DEFAULT_PACK_TOKENS);
    assert_eq!(pack.items, items);
    assert_eq!(pack.dropped, 0);
}

#[test]
fn budget_bounds_total_estimate() {
    let big: String = "x ".repeat(10_000);
    let items = vec![item("a", &big), item("b", "tiny")];
    let pack = pack_context(&items, 100);
    let total: u64 = pack.items.iter().map(|item| fluent_llm::tokens::estimate_tokens(&item.text)).sum();
    assert!(total <= 100, "total {total} exceeds budget");
    assert!(pack.dropped >= 1);
    assert_eq!(pack.items.len() + pack.dropped, items.len());
}

#[test]
fn packs_are_deterministic() {
    let items = vec![item("a", "alpha text here"), item("b", "beta text here")];
    assert_eq!(pack_context(&items, 50), pack_context(&items, 50));
}

#[test]
fn zero_budget_drops_everything() {
    let items = vec![item("a", "alpha")];
    let pack = pack_context(&items, 0);
    assert!(pack.items.is_empty());
    assert_eq!(pack.dropped, 1);
}
