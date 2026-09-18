//! Golden outputs for [`common_core::string::apply_spans_rightmost`]: the
//! shared rightmost-first span splicer. Every vector pins totality
//! (never panics — degenerate spans are skipped) and determinism
//! (input order is irrelevant; output depends only on the span set).
//!
//! Overlap bias is fail-closed: spans win right-to-left, so a span that
//! starts left of handled text is clamped to the unhandled prefix rather
//! than dropped — sensitive text is never left in place by an overlap.

use common_core::string::apply_spans_rightmost;

#[test]
fn disjoint_spans_replace_in_any_input_order() {
    // Uncovered middle ("BBB") survives verbatim.
    let spans = vec![(6usize, 9usize, "C"), (0, 3, "A")];
    assert_eq!(apply_spans_rightmost("aaaBBBccc", spans), "ABBBC");
}

#[test]
fn adjacent_spans_all_apply() {
    let spans = vec![(0usize, 3usize, "A"), (3, 6, "B"), (6, 9, "C")];
    assert_eq!(apply_spans_rightmost("aaaBBBccc", spans), "ABC");
}

#[test]
fn empty_replacement_deletes() {
    assert_eq!(
        apply_spans_rightmost("prefix SECRETVALUE suffix", vec![(7usize, 18usize, "")]),
        "prefix  suffix"
    );
}

#[test]
fn duplicate_spans_apply_once() {
    let spans = vec![(0usize, 3usize, "A"), (0, 3, "A")];
    assert_eq!(apply_spans_rightmost("aaabbb", spans), "Abbb");
}

#[test]
fn empty_and_inverted_spans_are_skipped() {
    // (2,2) is empty (would insert); (4,3) is inverted — both skipped, the
    // valid span still applies.
    let spans = vec![(2usize, 2usize, "X"), (4, 3, "Y"), (0, 1, "A")];
    assert_eq!(apply_spans_rightmost("abcdef", spans), "Abcdef");
}

#[test]
fn end_straddling_spans_clamp_fail_closed() {
    // (0,100) claims past the end of a 6-byte text: scrub the existing
    // unhandled prefix (fail-closed — matched text is never left in place),
    // keep the handled region's replacement.
    let spans = vec![(0usize, 100usize, "X"), (4, 6, "Y")];
    assert_eq!(apply_spans_rightmost("abcdef", spans), "XY");
}

#[test]
fn past_the_end_spans_are_skipped() {
    let spans = vec![(10usize, 20usize, "X"), (4, 6, "Y")];
    assert_eq!(apply_spans_rightmost("abcdef", spans), "abcdY");
}

#[test]
fn non_boundary_spans_are_skipped() {
    // "é" is two bytes (0..2); (1,2) splits it and would panic
    // `replace_range` — skipped, while the valid (0,2) still applies.
    let text = "école";
    let spans = vec![(0usize, 2usize, "E"), (1, 2, "BAD")];
    assert_eq!(apply_spans_rightmost(text, spans), "Ecole");
}

#[test]
fn nested_outer_is_clamped_fail_closed() {
    // (0,6) contains the already-applied (2,4): the outer remainder (0,2)
    // is still replaced (fail-closed — never leave matched text in place),
    // never touching the handled region.
    let spans = vec![(0usize, 6usize, "O"), (2, 4, "I")];
    assert_eq!(apply_spans_rightmost("abcdef", spans), "OIef");
}

#[test]
fn nested_inner_after_outer_is_skipped() {
    // Same set, opposite input order: output depends on the span set, not
    // the order — identical to the clamped result above.
    let spans = vec![(2usize, 4usize, "I"), (0, 6, "O")];
    assert_eq!(apply_spans_rightmost("abcdef", spans), "OIef");
}

#[test]
fn partial_overlap_clamps_to_unhandled_prefix() {
    // (0,4) overlaps applied (2,6): only the unhandled prefix (0,2) takes
    // the replacement; the rightmost region keeps "R".
    let spans = vec![(0usize, 4usize, "L"), (2, 6, "R")];
    assert_eq!(apply_spans_rightmost("abcdefgh", spans), "LRgh");
}

#[test]
fn empty_text_and_no_spans() {
    assert_eq!(apply_spans_rightmost("", vec![]), "");
    assert_eq!(apply_spans_rightmost("abc", vec![]), "abc");
    assert_eq!(
        apply_spans_rightmost("", vec![(0usize, 1usize, "X")]),
        "",
        "span on empty text is out of bounds → skipped"
    );
}
