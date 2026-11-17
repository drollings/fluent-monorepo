//! M1.2 unit tests for the shared text-shape core in
//! `extractor/mod.rs`, independent of any call site. Call-site
//! equivalence against the historical copies is pinned by
//! `extract_code.rs`, `extract_markdown.rs`, and
//! `extract_text.rs` tables (M1.1); these tests pin the core.

use super::{
    byte_offset_for_units, overlap_lines_back, overlap_lines_back_inclusive, split_long_units,
    utf16_len,
};

#[test]
fn core_byte_offset_table() {
    let cases: &[(&str, usize, usize)] = &[
        ("", 0, 0),
        ("hello", 0, 0),
        ("hello", 2, 2),
        ("hello", 5, 5),
        ("hello", 99, 5),
        ("h\u{e9}llo", 2, 3),
        ("a\u{1f4bb}b", 2, 5),
        ("a\u{1f4bb}b", 4, 6),
    ];
    for (line, units, want) in cases {
        assert_eq!(byte_offset_for_units(line, *units), *want);
    }
}

#[test]
fn core_overlap_exclusive_rule() {
    let lines = ["aaa", "bb", "c"];
    assert_eq!(overlap_lines_back(&lines, 0, 3, 0), 0);
    assert_eq!(overlap_lines_back(&lines, 0, 3, 2), 1);
    assert_eq!(overlap_lines_back(&lines, 0, 3, 1), 0);
    assert_eq!(overlap_lines_back(&lines, 0, 3, 100), 3);
    assert_eq!(overlap_lines_back(&lines, 1, 2, 100), 1);
    assert_eq!(overlap_lines_back(&lines, 0, 0, 100), 0);
}

#[test]
fn core_overlap_inclusive_rule() {
    let lines = ["aaa", "bb", "c"];
    assert_eq!(overlap_lines_back_inclusive(&lines, 0, 3, 0), 0);
    assert_eq!(overlap_lines_back_inclusive(&lines, 0, 3, 2), 1);
    assert_eq!(overlap_lines_back_inclusive(&lines, 0, 3, 100), 3);
}

#[test]
fn core_overlap_rules_differ_only_at_meet() {
    // Single line meeting the budget: exclusive stops before it,
    // inclusive counts it. This is the entire contract delta.
    let lines = ["a"];
    assert_eq!(overlap_lines_back(&lines, 0, 1, 1), 0);
    assert_eq!(overlap_lines_back_inclusive(&lines, 0, 1, 1), 1);
    let lines = ["a", "b"];
    assert_eq!(overlap_lines_back(&lines, 0, 2, 3), 1);
    assert_eq!(overlap_lines_back_inclusive(&lines, 0, 2, 3), 2);
}

#[test]
fn core_split_with_max_cut() {
    fn max_cut(_window: &str, max: usize) -> usize {
        max
    }
    let parts: Vec<(usize, &str)> = split_long_units("hello world", 5, max_cut);
    assert_eq!(
        parts
            .iter()
            .map(|(start, text)| (*start, (*text).to_string()))
            .collect::<Vec<_>>(),
        vec![
            (0, "hello".to_string()),
            (5, " worl".to_string()),
            (10, "d".to_string())
        ]
    );
    assert!(split_long_units("", 5, max_cut).is_empty());
    let parts: Vec<(usize, &str)> = split_long_units("h\u{e9}llo", 3, max_cut);
    assert_eq!(
        parts
            .iter()
            .map(|(start, text)| (*start, (*text).to_string()))
            .collect::<Vec<_>>(),
        vec![(0, "h\u{e9}l".to_string()), (4, "lo".to_string())]
    );
}

#[test]
fn core_split_pairs_reconstruct() {
    fn max_cut(_window: &str, max: usize) -> usize {
        max
    }
    let line = "h\u{e9}llo w\u{f6}rld a\u{1f4bb}b";
    let mut rebuilt = String::new();
    let mut cursor = 0;
    for (start, slice) in split_long_units(line, 4, max_cut) {
        assert!(line.is_char_boundary(start));
        assert!(start >= cursor);
        rebuilt.push_str(slice);
        cursor = start + slice.len();
    }
    assert_eq!(rebuilt, line);
    assert!(utf16_len(line) > 0);
}
