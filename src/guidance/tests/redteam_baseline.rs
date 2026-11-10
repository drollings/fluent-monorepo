//! Baseline probe schema tests: deterministic math, no database.

use super::*;

#[test]
fn replace_cost_counts_fts_and_lemma_writes() {
    let cost = replace_file_cost(3, 7);
    assert_eq!(cost.fragments, 3);
    assert_eq!(cost.lemmas, 7);
    assert_eq!(cost.fts_writes, 6);
    assert_eq!(cost.total_row_writes, 3 + 3 + 7 + 6);
}

#[test]
fn replace_cost_empty_is_zero() {
    let cost = replace_file_cost(0, 0);
    assert_eq!(cost.fts_writes, 0);
    assert_eq!(cost.total_row_writes, 0);
}

#[test]
fn size_probe_reports_quantized_and_fp32_bytes() {
    let probe = size_probe_for_fragment("hello", "a b", 768);
    assert_eq!(probe.text_bytes, 5);
    assert_eq!(probe.cjk_bytes, 3);
    assert_eq!(probe.embedding_q8_bytes, 776);
    assert_eq!(probe.embedding_fp32_bytes, 3072);
}

#[test]
fn lemma_density_is_mean_rows_per_fragment() {
    assert!((lemma_density(10, 2) - 5.0).abs() < 1e-9);
    assert!((lemma_density(0, 0) - 0.0).abs() < 1e-9);
    assert!((lemma_density(5, 0) - 0.0).abs() < 1e-9);
}

#[test]
fn phase_split_totals_and_embed_share() {
    let split = PhaseSplit {
        scan_ms: 10,
        parse_ms: 20,
        embed_ms: 60,
        commit_ms: 10,
    };
    assert_eq!(split.total_ms(), 100);
    assert!((split.embed_share() - 0.6).abs() < 1e-9);
    let empty = PhaseSplit {
        scan_ms: 0,
        parse_ms: 0,
        embed_ms: 0,
        commit_ms: 0,
    };
    assert!((empty.embed_share() - 0.0).abs() < 1e-9);
}

#[test]
fn parity_ignores_order_but_not_membership() {
    assert!(file_level_parity(
        vec!["b".to_string(), "a".to_string()],
        vec!["a".to_string(), "b".to_string()],
    ));
    assert!(!file_level_parity(
        vec!["a".to_string()],
        vec!["a".to_string(), "b".to_string()],
    ));
}

#[test]
fn probes_are_deterministic_on_fixed_fixture() {
    let first = replace_file_cost(4, 12);
    let second = replace_file_cost(4, 12);
    assert_eq!(first, second);
    let probe = size_probe_for_fragment("abc", "d", 64);
    assert_eq!(size_probe_for_fragment("abc", "d", 64), probe);
}
